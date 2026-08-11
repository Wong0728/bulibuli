//! ASS 文本生成与格式化工具：弹幕 ASS 渲染、颜色/时间/转义处理。

use anyhow::Result;
use std::path::Path;
use tokio::fs;

use super::{BurnConfig, PositionedDanmaku, PLAY_RES_X, PLAY_RES_Y};

pub(super) fn rgb_to_bgr(color: &str) -> String {
    if color.len() != 6 {
        return "FFFFFF".to_string();
    }
    let r = &color[0..2];
    let g = &color[2..4];
    let b = &color[4..6];
    format!("{}{}{}", b, g, r)
}

pub(super) fn format_ass_time(seconds: f64) -> String {
    let cents = (seconds * 100.0) as i64;
    let hours = cents / 360000;
    let cents = cents % 360000;
    let minutes = cents / 6000;
    let cents = cents % 6000;
    let secs = cents / 100;
    let cents = cents % 100;
    format!("{}:{:02}:{:02}.{:02}", hours, minutes, secs, cents)
}

pub(super) fn escape_ass(text: &str) -> String {
    text.replace('{', "｛")
        .replace('}', "｝")
        .replace(['\r', '\n'], "")
}

fn is_dark_color(color: &str) -> bool {
    if color.len() != 6 {
        return false;
    }
    let r = i64::from_str_radix(&color[0..2], 16).unwrap_or(255);
    let g = i64::from_str_radix(&color[2..4], 16).unwrap_or(255);
    let b = i64::from_str_radix(&color[4..6], 16).unwrap_or(255);
    let brightness = r as f64 * 0.299 + g as f64 * 0.587 + b as f64 * 0.114;
    brightness < 48.0
}

pub(super) fn hex_alpha(opacity: f64) -> String {
    let alpha = (0xFF as f64 * (1.0 - opacity)) as i32;
    format!("{:02X}", alpha)
}

pub(super) async fn generate_ass(
    positioned: &[PositionedDanmaku],
    output_path: &Path,
    xml_name: &str,
    video_title: &str,
    config: &BurnConfig,
) -> Result<()> {
    let alpha = hex_alpha(config.opacity);
    let font_name = choose_font(&config.font_family);
    let ori_info = if xml_name.is_empty() {
        "弹幕XML文件"
    } else {
        xml_name
    };
    let title = if video_title.is_empty() {
        "bilibili ASS 弹幕"
    } else {
        video_title
    };

    let header = format!(
        "[Script Info]\n\
         Title: {}\n\
         Original Script: 根据 {} 的弹幕信息，由 https://github.com/tiansh/us-danmaku 生成\n\
         ScriptType: v4.00+\n\
         Collisions: Normal\n\
         PlayResX: {}\n\
         PlayResY: {}\n\
         Timer: 10.0000\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Fix,{},25,&H{}FFFFFF,&H{}FFFFFF,&H{}000000,&H{}000000,1,0,0,0,100,100,0,0,1,2,0,2,20,20,2,0\n\
         Style: R2L,{},25,&H{}FFFFFF,&H{}FFFFFF,&H{}000000,&H{}000000,1,0,0,0,100,100,0,0,1,2,0,2,20,20,2,0\n\n\
         [Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        title, ori_info, PLAY_RES_X as i32, PLAY_RES_Y as i32,
        font_name, alpha, alpha, alpha, alpha,
        font_name, alpha, alpha, alpha, alpha
    );

    let mut lines = Vec::new();
    for line in positioned {
        if line.text.is_empty() {
            continue;
        }
        let mut effect_parts = Vec::new();
        if line.mode == "R2L" {
            effect_parts.push(format!(
                "\\move({},{},{},{})",
                line.poss_x.round() as i64,
                line.poss_y.round() as i64,
                line.posd_x.round() as i64,
                line.posd_y.round() as i64
            ));
        } else {
            effect_parts.push(format!(
                "\\pos({},{})",
                line.poss_x.round() as i64,
                line.poss_y.round() as i64
            ));
        }
        let color = if config.color_mode == "uniform" {
            config.color.as_str()
        } else {
            line.color.as_str()
        };
        if color != "FFFFFF" {
            let bgr = rgb_to_bgr(color);
            effect_parts.push(format!("\\c&H{}", bgr));
        }
        if is_dark_color(color) {
            effect_parts.push("\\3c&HFFFFFF".to_string());
        }
        if line.font_size != 25 {
            effect_parts.push(format!("\\fs{}", line.font_size));
        }
        let effect_str = effect_parts.join("");
        let text_escaped = escape_ass(&line.text);
        let start = format_ass_time(line.stime);
        let end = format_ass_time(line.dtime);
        lines.push(format!(
            "Dialogue: 0,{},{},{},,20,20,2,,{{{}}}{}",
            start, end, line.mode, effect_str, text_escaped
        ));
    }

    // 使用 UTF-8 BOM
    let mut content = String::from("\u{FEFF}");
    content.push_str(&header);
    content.push('\n');
    content.push_str(&lines.join("\n"));

    fs::write(output_path, content).await?;
    Ok(())
}

pub(super) fn choose_font(value: &str) -> &'static str {
    match value {
        "Microsoft YaHei UI" => "Microsoft YaHei UI",
        "Noto Sans CJK SC" => "Noto Sans CJK SC",
        "Arial" => "Arial",
        _ if cfg!(target_os = "linux") => "Noto Sans CJK SC",
        _ => "Microsoft YaHei UI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb_to_bgr() {
        assert_eq!(rgb_to_bgr("FF0000"), "0000FF");
        assert_eq!(rgb_to_bgr("FFFFFF"), "FFFFFF");
    }

    #[test]
    fn test_format_ass_time() {
        assert_eq!(format_ass_time(3661.23), "1:01:01.23");
    }
}
