//! FLV 分段合并的真实媒体集成测试：需要可用的 ffmpeg/ffprobe（FFMPEG_BIN 或 PATH）。

use super::*;
use crate::services::spawn_util::child_lifetime;
use tempfile::tempdir;

fn required_ffmpeg_bin() -> PathBuf {
    let bin = std::env::var_os("FFMPEG_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            })
        });
    std::process::Command::new(&bin)
        .arg("-version")
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "真实媒体集成测试要求可用 ffmpeg；请设置 FFMPEG_BIN 或将 ffmpeg 加入 PATH: {error}"
            )
        });
    let probe = super::super::ffmpeg_probe::ffprobe_path(&bin);
    std::process::Command::new(&probe)
        .arg("-version")
        .output()
        .unwrap_or_else(|error| {
            panic!("真实媒体集成测试要求可用 ffprobe；请将其置于 ffmpeg 同目录或 PATH: {error}")
        });
    bin
}

async fn generate_fixture(ffmpeg: &Path, output: &Path) {
    let result = Command::new(ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=44100",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-f",
            "flv",
        ])
        .arg(output)
        .output()
        .await
        .expect("run ffmpeg fixture generation");
    assert!(
        result.status.success(),
        "fixture generation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn merge_real_flv_segments_preserves_duration_and_tracks() {
    let ffmpeg = required_ffmpeg_bin();
    let temp = tempdir().expect("temporary directory");
    let media_dir = temp.path().join("直播 ' 媒体夹具");
    tokio::fs::create_dir_all(&media_dir)
        .await
        .expect("media directory");
    let first = media_dir.join("第一段.flv");
    let second = media_dir.join("第二段.flv");
    generate_fixture(&ffmpeg, &first).await;
    generate_fixture(&ffmpeg, &second).await;

    let output = merge_segments_to_mp4(&ffmpeg, &[first.clone(), second.clone()])
        .await
        .expect("merge real FLV fixtures");
    let probe = super::super::ffmpeg_probe::probe_media(&ffmpeg, &output)
        .await
        .expect("probe merged output");
    assert!(probe.has_video && probe.has_audio);
    assert!(
        probe.duration_secs >= 1.8,
        "merged duration: {}",
        probe.duration_secs
    );
    assert!(
        first.exists() && second.exists(),
        "merger must not remove source segments"
    );
}

#[tokio::test]
async fn invalid_merge_keeps_source_segment() {
    let ffmpeg = required_ffmpeg_bin();
    let temp = tempdir().expect("temporary directory");
    let source = temp.path().join("损坏片段.flv");
    tokio::fs::write(&source, b"not a media stream")
        .await
        .expect("write source");
    assert!(
        merge_segments_to_mp4(&ffmpeg, std::slice::from_ref(&source))
            .await
            .is_err()
    );
    assert!(
        source.exists(),
        "failed validation must retain recovery input"
    );
}

#[test]
fn concat_path_escapes_single_quotes() {
    let path = PathBuf::from("C:\\recordings\\主播'segment.flv");
    assert_eq!(
        escape_concat_path(&path),
        "C:\\recordings\\主播'\\''segment.flv"
    );
}

#[test]
fn unverified_output_keeps_mp4_name_and_drops_partial() {
    let partial = PathBuf::from("/data/live/room_1/主播 第一段.mp4.partial");
    assert_eq!(
        unverified_output_path(&partial),
        PathBuf::from("/data/live/room_1/主播 第一段.mp4.unverified")
    );
}

#[tokio::test]
async fn merge_of_segment_killed_mid_write_still_produces_output() {
    let ffmpeg = required_ffmpeg_bin();
    let temp = tempdir().expect("temporary directory");
    let healthy = temp.path().join("room_segment_0001.flv");
    generate_fixture(&ffmpeg, &healthy).await;
    let killed = spawn_killed_flv(&ffmpeg, temp.path().join("room_segment_0002.flv")).await;

    let output = merge_segments_to_mp4(&ffmpeg, &[healthy.clone(), killed.clone()])
        .await
        .expect("被强杀的分段只能按未知时长处理，不得毁掉整场合并");
    let probe = super::super::ffmpeg_probe::probe_media(&ffmpeg, &output)
        .await
        .expect("probe merged output");
    assert!(probe.has_video && probe.has_audio);
    // 输出必须比单个健康分段更大，否则说明强杀分段被静默丢弃。
    let healthy_size = tokio::fs::metadata(&healthy).await.expect("meta").len();
    let killed_size = tokio::fs::metadata(&killed).await.expect("meta").len();
    let output_size = tokio::fs::metadata(&output).await.expect("meta").len();
    assert!(
        output_size > healthy_size + killed_size / 2,
        "merged output too small: {output_size} vs killed {killed_size}"
    );
    assert!(
        killed_size > 1024,
        "强杀分段应保留可用内容，实际 {killed_size} 字节"
    );
}

/// 录制中途强杀 FFmpeg：留下的 FLV 分段头部 metadata 停在占位值，
/// 与生产上 taskkill 的场次一致。
async fn spawn_killed_flv(ffmpeg: &Path, output: PathBuf) -> PathBuf {
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=44100",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-flvflags",
            "add_keyframe_index",
            "-f",
            "flv",
        ])
        .arg(&output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = child_lifetime::spawn(&mut command).expect("start ffmpeg for kill simulation");
    for _ in 0..100 {
        if tokio::fs::metadata(&output)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            > 256 * 1024
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    child.kill().await.expect("force kill ffmpeg");
    child.wait().await.expect("reap killed child");
    output
}

/// 只含一种媒体轨道的 FLV 分段：`video` 决定保留视频轨还是音频轨。
async fn generate_single_track_fixture(ffmpeg: &Path, output: &Path, video: bool) {
    let mut command = Command::new(ffmpeg);
    command.args(["-y", "-v", "error", "-f", "lavfi", "-i"]);
    if video {
        command
            .arg("testsrc=size=160x90:rate=25")
            .args(["-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    } else {
        command
            .arg("sine=frequency=1000:sample_rate=44100")
            .args(["-t", "1", "-c:a", "aac"]);
    }
    let result = command
        .args(["-f", "flv"])
        .arg(output)
        .output()
        .await
        .expect("run ffmpeg single-track fixture generation");
    assert!(
        result.status.success(),
        "fixture generation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn merge_video_only_segments_produces_complete_output() {
    // 无音频轨的直播间（纯视频流）真实存在：合并必须走仅视频轨的 remux，
    // 不得因为输出没有音轨就被校验判成失败、把整场留给分段。
    let ffmpeg = required_ffmpeg_bin();
    let temp = tempdir().expect("temporary directory");
    let first = temp.path().join("room_segment_0001.flv");
    let second = temp.path().join("room_segment_0002.flv");
    generate_single_track_fixture(&ffmpeg, &first, true).await;
    generate_single_track_fixture(&ffmpeg, &second, true).await;

    let output = merge_segments_to_mp4(&ffmpeg, &[first.clone(), second.clone()])
        .await
        .expect("纯视频分段应能合并为成品");
    let probe = super::super::ffmpeg_probe::probe_media(&ffmpeg, &output)
        .await
        .expect("probe merged output");
    assert!(probe.has_video, "合并输出必须保留视频轨");
    assert!(!probe.has_audio, "夹具本就无音轨");
    assert!(
        probe.duration_secs >= 1.8,
        "merged duration: {}",
        probe.duration_secs
    );
    assert!(
        first.exists() && second.exists(),
        "merger must not remove source segments"
    );
}

#[tokio::test]
async fn failed_validation_preserves_merged_output_for_rescue() {
    let ffmpeg = required_ffmpeg_bin();
    let temp = tempdir().expect("temporary directory");
    // 无视频轨的分段：合并能跑完，但输出缺视频轨道，校验必然失败。
    let voice_only = temp.path().join("room_segment_0001.flv");
    generate_single_track_fixture(&ffmpeg, &voice_only, false).await;

    let error = merge_segments_to_mp4(&ffmpeg, &[voice_only.clone(), voice_only])
        .await
        .expect_err("缺视频轨的输出必须校验失败");
    let message = error.to_string();
    assert!(
        message.contains("缺少视频轨道"),
        "错误文案需说明失败原因: {message}"
    );
    assert!(
        message.contains("已保留未通过校验的合并产物") && message.contains(".mp4.unverified"),
        "错误文案需说明保留了什么: {message}"
    );
    let salvage = temp.path().join("room_segment_0001.mp4.unverified");
    assert!(
        tokio::fs::metadata(&salvage)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            > 0,
        "校验失败必须保留合并产物供用户抢救"
    );
    assert!(
        tokio::fs::metadata(temp.path().join("room_segment_0001.mp4"))
            .await
            .is_err(),
        "未通过校验的输出不得冒充成品"
    );
}
