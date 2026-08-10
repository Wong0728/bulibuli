//! 弹幕文件加载：xml（B站原始格式）与 json（本项目导出格式）解析。

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use tokio::fs;

use super::DanmakuItem;

const MAX_DANMAKU_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DANMAKU_ITEMS: usize = 1_000_000;
const MAX_XML_ATTRIBUTES: usize = 64;

/// 从 xml/json 文件加载弹幕列表。
pub(super) async fn load_danmaku_list(path: &Path) -> Result<Vec<DanmakuItem>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    if ext == "xml" {
        return parse_xml(path).await;
    }

    // JSON：兼容数组或 { danmaku_list: [...] }
    let content = fs::read_to_string(path).await?;
    let data: Value =
        serde_json::from_str(&content).map_err(|e| anyhow!("解析 JSON 弹幕失败: {e}"))?;
    let list = data
        .as_array()
        .cloned()
        .or_else(|| data.get("danmaku_list").and_then(|v| v.as_array().cloned()))
        .unwrap_or_default();

    let mut result = Vec::with_capacity(list.len());
    for item in list {
        let time = item["time"].as_f64().unwrap_or(0.0);
        let mode_int = item["type"].as_i64().unwrap_or(1);
        let mode = match mode_int {
            4 => "BOTTOM",
            5 => "TOP",
            _ => "R2L",
        };
        let size = item["size"].as_i64().unwrap_or(25) as i32;
        let color_val = item["color"].as_i64().unwrap_or(16777215) as i32;
        let text = item["text"].as_str().unwrap_or("").to_string();
        result.push(DanmakuItem {
            text,
            time,
            mode: mode.to_string(),
            size,
            color: format!("{:06X}", color_val & 0xFFFFFF),
            bottom: false,
        });
    }
    Ok(result)
}

async fn parse_xml(xml_path: &Path) -> Result<Vec<DanmakuItem>> {
    let metadata = fs::metadata(xml_path).await?;
    if metadata.len() > MAX_DANMAKU_SOURCE_BYTES {
        return Err(anyhow!(
            "XML 弹幕文件超过 {} MiB 限制",
            MAX_DANMAKU_SOURCE_BYTES / 1024 / 1024
        ));
    }
    let content = fs::read_to_string(xml_path).await?;
    let mut list = Vec::new();

    // 使用 quick_xml 解析
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(&content);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_attr: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == b"d" => {
                let mut attribute_count = 0;
                for attr in e.attributes() {
                    attribute_count += 1;
                    if attribute_count > MAX_XML_ATTRIBUTES {
                        return Err(anyhow!("XML 弹幕标签属性数量超过安全上限"));
                    }
                    let attr = attr.context("读取 XML 属性失败")?;
                    if attr.key.as_ref() == b"p" {
                        current_attr = Some(String::from_utf8_lossy(&attr.value).to_string());
                        break;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if let Some(attr) = current_attr.take() {
                    let decoded = e.decode().context("解码 XML 弹幕文本失败")?;
                    let text = quick_xml::escape::unescape(&decoded)
                        .context("还原 XML 弹幕文本失败")?
                        .into_owned();
                    let fields: Vec<&str> = attr.split(',').collect();
                    if fields.len() >= 4 {
                        let mode_int: i32 = fields[1].parse().unwrap_or(1);
                        let mode = match mode_int {
                            4 => "BOTTOM",
                            5 => "TOP",
                            _ => "R2L",
                        };
                        let color_val: i32 = fields[3].parse().unwrap_or(16777215);
                        let bottom = fields
                            .get(5)
                            .and_then(|s| s.parse::<i32>().ok())
                            .unwrap_or(0)
                            > 0;
                        if list.len() >= MAX_DANMAKU_ITEMS {
                            return Err(anyhow!("XML 弹幕条数超过安全上限"));
                        }
                        list.push(DanmakuItem {
                            text,
                            time: fields[0].parse().unwrap_or(0.0),
                            mode: mode.to_string(),
                            size: fields[2].parse().unwrap_or(25),
                            color: format!("{:06X}", color_val & 0xFFFFFF),
                            bottom,
                        });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("XML 解析错误: {e}")),
            _ => {}
        }
        buf.clear();
    }

    list.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(list)
}
