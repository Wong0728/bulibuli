//! 媒体探测与分段校验：用 ffprobe（回退 ffmpeg）验证合并输出的容器、轨道与
//! 时长，防止空文件或强杀分段混进成品。

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tracing::warn;

const MIN_MERGED_DURATION_RATIO: f64 = 0.90;
/// 探测挂起（损坏文件/异常 IO）会让 finalize 永久停留在 Finalizing：
/// ffprobe 与回退的 ffmpeg 探测都按此超时收口，量级与合并超时对齐。
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub(super) struct MediaProbe {
    pub(super) duration_secs: f64,
    pub(super) has_video: bool,
    pub(super) has_audio: bool,
}

/// 分段有效性判定：时长为 0 不等于坏文件。
#[derive(Debug, PartialEq)]
enum SegmentProbe {
    /// 时长可信，计入输入总时长。
    Timestamped(f64),
    /// 时长不可信（强杀的 FLV 会把 onMetaData.duration 占位写成 0），
    /// 但音视频轨道与体积有效。
    Untimestamped,
    /// 既无时长也无可用轨道或体积为空：真实坏输入。
    Invalid(&'static str),
}

/// 非空 MP4 不能单独证明拼接成功；必须先探测容器，调用方才能删除原始 FLV 分段。
pub(super) async fn verify_merged_output(
    ffmpeg_path: &Path,
    output: &Path,
    segments: &[PathBuf],
) -> Result<()> {
    let output_probe = probe_media(ffmpeg_path, output).await?;
    if !output_probe.has_video {
        return Err(anyhow!("合并输出缺少视频轨道"));
    }
    let mut input_duration = 0.0;
    let mut input_has_audio = false;
    let mut untimestamped = Vec::new();
    for segment in segments {
        let probe = probe_media(ffmpeg_path, segment)
            .await
            .with_context(|| format!("校验合并输入分段失败: {}", segment.display()))?;
        input_has_audio |= probe.has_audio;
        let size = tokio::fs::metadata(segment)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        match classify_segment_probe(&probe, size) {
            SegmentProbe::Timestamped(duration) => input_duration += duration,
            SegmentProbe::Untimestamped => untimestamped.push(segment.clone()),
            SegmentProbe::Invalid(reason) => {
                return Err(anyhow!("输入分段无效（{reason}）: {}", segment.display()))
            }
        }
    }
    // 纯视频直播流（房间无音频轨）合并后本就无声：只有输入带音频时才要求输出含
    // 音频，否则这类房间每一场都会被判成合并失败。
    if input_has_audio && !output_probe.has_audio {
        return Err(anyhow!("合并输出缺少音频轨道"));
    }
    if !untimestamped.is_empty() {
        warn!(
            count = untimestamped.len(),
            total = segments.len(),
            "部分分段时间戳不可信（探测时长为 0，常见于被强杀的 FLV），已按轨道与体积校验后放行，且不计入输出时长比对"
        );
    }
    if input_duration > 0.0
        && output_probe.duration_secs < input_duration * MIN_MERGED_DURATION_RATIO
    {
        return Err(anyhow!(
            "合并输出时长不足: 输出 {:.3}s，输入可确认合计 {:.3}s（{} 个分段时间戳不可信未计入）",
            output_probe.duration_secs,
            input_duration,
            untimestamped.len()
        ));
    }
    Ok(())
}

/// 分段判定：时长优先；时长缺失/为 0 时退化为「有音视频轨道 + 非空体积」。
fn classify_segment_probe(probe: &MediaProbe, size_bytes: u64) -> SegmentProbe {
    if probe.duration_secs.is_finite() && probe.duration_secs > 0.0 {
        return SegmentProbe::Timestamped(probe.duration_secs);
    }
    if size_bytes == 0 {
        return SegmentProbe::Invalid("文件为空");
    }
    if !probe.has_video && !probe.has_audio {
        return SegmentProbe::Invalid("无音视频轨道");
    }
    SegmentProbe::Untimestamped
}

pub(super) async fn probe_media(ffmpeg_path: &Path, input: &Path) -> Result<MediaProbe> {
    let probe = ffprobe_path(ffmpeg_path);
    let mut cmd = Command::new(&probe);
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration:stream=codec_type",
        "-of",
        "json",
    ])
    .arg(input)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    let result = match tokio::time::timeout(PROBE_TIMEOUT, probe_output(&mut cmd)).await {
        Ok(result) => result,
        Err(_) => {
            return Err(anyhow!(
                "ffprobe 校验超时（{} 秒）",
                PROBE_TIMEOUT.as_secs()
            ));
        }
    };
    match result {
        Ok(result) if result.status.success() => parse_probe_json(&result.stdout),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            probe_media_with_ffmpeg(ffmpeg_path, input).await
        }
        Ok(result) => Err(anyhow!(
            "ffprobe 校验失败: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
        Err(error) => {
            Err(error).with_context(|| format!("启动 ffprobe 校验失败: {}", probe.display()))
        }
    }
}

/// 与 `Command::output()` 等价的进程输出收集（stdin 由调用方置为 null），
/// 但先 spawn 以便把探测进程也纳入 Job Object / pdeathsig 回收。
async fn probe_output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    let child = crate::services::spawn_util::child_lifetime::spawn(cmd)?;
    child.wait_with_output().await
}

fn parse_probe_json(stdout: &[u8]) -> Result<MediaProbe> {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).context("解析 ffprobe 校验结果失败")?;
    let streams = value
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("ffprobe 未返回 streams"))?;
    let has_video = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
    });
    let has_audio = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
    });
    let duration_secs = value
        .pointer("/format/duration")
        .and_then(serde_json::Value::as_str)
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration >= 0.0)
        // 缺失/为 0 只表示「时长不可信」，由调用方按轨道与体积判定有效性；
        // 强杀留下的 FLV 分段就是 duration=0 但内容可播。
        .unwrap_or(0.0);
    Ok(MediaProbe {
        duration_secs,
        has_video,
        has_audio,
    })
}

async fn probe_media_with_ffmpeg(ffmpeg_path: &Path, input: &Path) -> Result<MediaProbe> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-hide_banner", "-i"])
        .arg(input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // 没有 ffprobe 时回退到 ffmpeg 探测，同样必须可超时：挂住的 ffmpeg 会让
    // 收尾永久停在 Finalizing，且这条路径上没有任何其它超时兜底。
    let result = match tokio::time::timeout(PROBE_TIMEOUT, probe_output(&mut cmd)).await {
        Ok(result) => result,
        Err(_) => {
            return Err(anyhow!(
                "FFmpeg 媒体探测超时（{} 秒）",
                PROBE_TIMEOUT.as_secs()
            ));
        }
    }
    .with_context(|| format!("使用 FFmpeg 读取媒体信息失败: {}", ffmpeg_path.display()))?;
    let text = String::from_utf8_lossy(&result.stderr);
    let duration_secs = text
        .lines()
        .find_map(|line| line.split("Duration: ").nth(1))
        .and_then(|value| value.split(',').next())
        .and_then(parse_timestamp)
        .ok_or_else(|| anyhow!("FFmpeg 未返回有效媒体时长"))?;
    let has_video = text.lines().any(|line| line.contains(" Video:"));
    let has_audio = text.lines().any(|line| line.contains(" Audio:"));
    if !result.status.success() && !has_video && !has_audio {
        return Err(anyhow!("FFmpeg 媒体探测失败: {}", text.trim()));
    }
    Ok(MediaProbe {
        duration_secs,
        has_video,
        has_audio,
    })
}

fn parse_timestamp(value: &str) -> Option<f64> {
    let mut parts = value.trim().split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

pub(super) fn ffprobe_path(ffmpeg_path: &Path) -> PathBuf {
    let probe_name = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    if let Some(candidate) = ffmpeg_path
        .parent()
        .map(|parent| parent.join(probe_name))
        .filter(|path| path.is_file())
    {
        return candidate;
    }
    for variable in [
        "FFPROBE_PATH",
        "FFPROBE",
        "FFMPEG_PATH",
        "FFMPEG_HOME",
        "FFMPEG_DIR",
    ] {
        if let Ok(value) = std::env::var(variable) {
            let configured = PathBuf::from(value);
            let candidate = if configured.is_file() {
                configured
            } else {
                configured.join(probe_name)
            };
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    which::which(probe_name).unwrap_or_else(|_| PathBuf::from(probe_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_duration_segment_with_tracks_is_untimestamped_not_invalid() {
        // 实测：FLV 分段被强杀后 onMetaData.duration 停在占位 0，ffprobe 仍列出音视频轨。
        let probe = MediaProbe {
            duration_secs: 0.0,
            has_video: true,
            has_audio: true,
        };
        assert_eq!(
            classify_segment_probe(&probe, 4096),
            SegmentProbe::Untimestamped
        );
        let audio_only = MediaProbe {
            duration_secs: 0.0,
            has_video: false,
            has_audio: true,
        };
        assert_eq!(
            classify_segment_probe(&audio_only, 4096),
            SegmentProbe::Untimestamped
        );
        assert_eq!(
            classify_segment_probe(&audio_only, 0),
            SegmentProbe::Invalid("文件为空")
        );
        let streamless = MediaProbe {
            duration_secs: 0.0,
            has_video: false,
            has_audio: false,
        };
        assert_eq!(
            classify_segment_probe(&streamless, 4096),
            SegmentProbe::Invalid("无音视频轨道")
        );
        let timed = MediaProbe {
            duration_secs: 12.5,
            has_video: true,
            has_audio: true,
        };
        assert_eq!(
            classify_segment_probe(&timed, 4096),
            SegmentProbe::Timestamped(12.5)
        );
    }

    #[test]
    fn probe_json_treats_missing_duration_as_unknown() {
        let without_duration =
            br#"{"streams":[{"codec_type":"video"},{"codec_type":"audio"}],"format":{}}"#;
        let probe = parse_probe_json(without_duration).expect("streams 有效即应通过");
        assert_eq!(probe.duration_secs, 0.0);
        assert!(probe.has_video && probe.has_audio);

        let zero = br#"{"streams":[{"codec_type":"video"}],"format":{"duration":"0.000000"}}"#;
        assert_eq!(parse_probe_json(zero).expect("零时长").duration_secs, 0.0);

        let real = br#"{"streams":[{"codec_type":"audio"}],"format":{"duration":"7.5"}}"#;
        assert_eq!(parse_probe_json(real).expect("真实时长").duration_secs, 7.5);
    }
}
