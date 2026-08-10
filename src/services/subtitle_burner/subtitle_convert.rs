//! 字幕转换：SRT → ASS、多 ASS 文件合并。

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use tokio::fs;

use super::ass_render::{choose_font, escape_ass, format_ass_time, hex_alpha};
use super::{BurnConfig, PLAY_RES_X, PLAY_RES_Y};

/// 将字幕文件（ass/srt）统一转换为 ass 路径。
/// ass 直接返回原路径；srt 生成临时 ass 文件。
/// `config` 提供 opacity（透明度），与弹幕烧录保持视觉一致。
pub(super) async fn convert_subtitle_to_ass(
    path: &Path,
    output_dir: &Path,
    config: &BurnConfig,
) -> Result<PathBuf> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if ext == "ass" {
        return Ok(path.to_path_buf());
    }

    if ext != "srt" {
        return Err(anyhow!("不支持的 subtitle 格式: {ext}"));
    }

    let content = fs::read_to_string(path).await?;
    let cues = parse_srt(&content);
    if cues.is_empty() {
        return Err(anyhow!("SRT 文件中没有有效字幕"));
    }

    fs::create_dir_all(output_dir).await?;
    let ass_path = output_dir.join(format!("subtitle-{}.ass", uuid::Uuid::new_v4()));

    let alpha = hex_alpha(config.opacity);
    let font_name = choose_font();
    let mut lines = vec![
        format!(
            "[Script Info]\n\
             Title: CC字幕\n\
             Original Script: 根据 SRT 字幕文件转换\n\
             ScriptType: v4.00+\n\
             Collisions: Normal\n\
             PlayResX: {}\n\
             PlayResY: {}\n\
             Timer: 10.0000\n\n\
             [V4+ Styles]\n\
             Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
             Style: Fix,{},25,&H{}FFFFFF,&H{}FFFFFF,&H{}000000,&H{}000000,1,0,0,0,100,100,0,0,1,2,0,2,20,20,2,0\n\n\
             [Events]\n\
             Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text",
            PLAY_RES_X as i32, PLAY_RES_Y as i32,
            font_name, alpha, alpha, alpha, alpha
        ),
    ];

    let center_x = (PLAY_RES_X / 2.0).round() as i64;
    let bottom_y = (PLAY_RES_Y - 30.0).round() as i64;
    for cue in cues {
        let text = escape_ass(&cue.text).replace('\n', "\\N");
        let effect = format!("\\pos({},{})\\an2", center_x, bottom_y);
        lines.push(format!(
            "Dialogue: 0,{},{},Fix,,20,20,2,,{{{}}}{}",
            format_ass_time(cue.start),
            format_ass_time(cue.end),
            effect,
            text
        ));
    }

    let mut content = String::from("\u{FEFF}");
    content.push_str(&lines.join("\n"));
    fs::write(&ass_path, content).await?;
    Ok(ass_path)
}

#[derive(Clone)]
struct SrtCue {
    start: f64,
    end: f64,
    text: String,
}

fn parse_srt(content: &str) -> Vec<SrtCue> {
    let mut cues = Vec::new();
    let blocks = content.split("\n\n");
    for block in blocks {
        let lines: Vec<&str> = block.lines().collect();
        if lines.len() < 2 {
            continue;
        }
        // 跳过序号行
        let time_line = if lines[0].trim().parse::<usize>().is_ok() {
            lines[1]
        } else {
            lines[0]
        };
        let Some((start_s, end_s)) = time_line.split_once("-->") else {
            continue;
        };
        let (Some(start), Some(end)) =
            (parse_srt_time(start_s.trim()), parse_srt_time(end_s.trim()))
        else {
            continue;
        };
        let text = lines
            .iter()
            .skip(if lines[0].trim().parse::<usize>().is_ok() {
                2
            } else {
                1
            })
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        cues.push(SrtCue { start, end, text });
    }
    cues
}

fn parse_srt_time(s: &str) -> Option<f64> {
    // 00:00:00,000 或 00:00:00.000
    let s = s.replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: f64 = parts[0].parse().ok()?;
    let minutes: f64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// 合并两个 ASS 文件（弹幕 + 字幕）为一个 ASS。
/// 使用第一个文件的 Styles/Header，合并所有 Dialogue 行。
pub(super) async fn merge_ass_files(
    output: &Path,
    danmaku: Option<&Path>,
    subtitle: Option<&Path>,
) -> Result<()> {
    let inputs: Vec<&Path> = [danmaku, subtitle].into_iter().flatten().collect();
    if inputs.is_empty() {
        return Err(anyhow!("没有可合并的 ASS 文件"));
    }

    let mut file_lines: Vec<Vec<String>> = Vec::new();
    for p in &inputs {
        let content = fs::read_to_string(p).await?;
        file_lines.push(content.lines().map(|l| l.to_string()).collect());
    }

    // 取第一个文件作为 header（到 Format 行为止）
    let first = &file_lines[0];
    let mut format_idx = None;
    for (i, line) in first.iter().enumerate() {
        if line.trim().starts_with("Format:") && line.contains("Layer") {
            format_idx = Some(i);
            break;
        }
    }
    let format_idx = format_idx.ok_or_else(|| anyhow!("ASS 文件缺少 Events Format 行"))?;
    let mut output_lines: Vec<String> = first[..=format_idx].to_vec();

    // 收集所有 Dialogue 行
    for lines in &file_lines {
        let mut in_events = false;
        for line in lines {
            if line.trim() == "[Events]" {
                in_events = true;
                continue;
            }
            if !in_events {
                continue;
            }
            if line.trim().starts_with("Format:") {
                continue;
            }
            if line.trim().starts_with("Dialogue:") {
                output_lines.push(line.clone());
            }
        }
    }

    let mut content = String::from("\u{FEFF}");
    content.push_str(&output_lines.join("\n"));
    fs::write(output, content).await?;
    Ok(())
}
