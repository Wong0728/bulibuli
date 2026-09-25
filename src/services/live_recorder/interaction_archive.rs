//! 互动归档收尾：把 events.jsonl 转写为 legacy JSON、XML 与标准 B 站弹幕 XML，
//! 并对 uid 字段脱敏。全部先写中间文件、成功后改名，abort 不留半截文件。

use super::interactions::{ArchivedLiveEvent, InteractionPaths};
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tracing::{info, warn};

/// 归档中间文件后缀：正常收尾后会改名去掉；中止/报错残留的由启动清理扫除。
pub(crate) const ARCHIVE_STAGING_SUFFIX: &str = ".archive-part";

/// 归档目标对应的中间文件路径。
fn archive_staging_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("interaction-archive");
    target.with_file_name(format!("{name}{ARCHIVE_STAGING_SUFFIX}"))
}

/// 把写满并 flush 过的中间文件改名成最终文件。
async fn publish_staged_archive(target: &Path) -> Result<()> {
    tokio::fs::rename(archive_staging_path(target), target).await?;
    Ok(())
}

pub(super) async fn archive_legacy_and_xml(
    paths: &InteractionPaths,
    room_id: i64,
    title: &str,
) -> Result<bool> {
    const MAX_ARCHIVE_EVENTS: usize = 500_000;
    const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
    // ponytail：限制归档输出，避免损坏的 JSONL 耗尽内存；
    // 如果未来需要更大规模采集，再迁移到外部流式处理器。
    let mut input = BufReader::new(tokio::fs::File::open(&paths.events).await?).lines();
    // 两个归档都先写 *.archive-part，完整收尾后才改名到位：收尾超时被 abort 时，
    // 最终文件名上不会出现半截 JSON/XML（半截文件语法不完整，用户事后打不开）。
    let legacy_target = paths.legacy.clone();
    let xml_target = paths.xml.clone();
    let mut legacy =
        BufWriter::new(tokio::fs::File::create(archive_staging_path(&legacy_target)).await?);
    let title_json = serde_json::to_string(title)?;
    legacy
        .write_all(
            format!(
                "{{\"schema_version\":2,\"room_id\":{},\"title\":{},\"events\":[",
                room_id, title_json
            )
            .as_bytes(),
        )
        .await?;

    let mut xml = BufWriter::new(tokio::fs::File::create(archive_staging_path(&xml_target)).await?);
    xml.write_all(
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<live_archive schema_version=\"2\" room_id=\"{}\" title=\"{}\">\n<metadata><source>jsonl-archive</source><uid_policy>redacted</uid_policy></metadata>\n",
            room_id,
            escape_xml(title)
        )
        .as_bytes(),
    )
    .await?;

    let mut count = 0usize;
    let mut bytes = 0u64;
    let mut truncated = false;
    let mut first = true;
    while let Some(line) = input.next_line().await? {
        bytes = bytes.saturating_add(line.len() as u64);
        if bytes > MAX_ARCHIVE_BYTES || count >= MAX_ARCHIVE_EVENTS {
            truncated = true;
            break;
        }
        let Ok(event) = serde_json::from_str::<ArchivedLiveEvent>(&line) else {
            continue;
        };
        if !first {
            legacy.write_all(b",").await?;
        }
        first = false;
        legacy.write_all(&serde_json::to_vec(&event)?).await?;
        let data = redact_uid_fields(event.data.clone());
        xml.write_all(
            format!(
                "<event seq=\"{}\" ts=\"{}\" type=\"{}\" segment=\"{}\"><data>{}</data></event>\n",
                event.seq,
                event.media_time_ms,
                escape_xml(&event.event_type),
                event.segment_index,
                escape_xml(&data.to_string()),
            )
            .as_bytes(),
        )
        .await?;
        count += 1;
    }
    legacy
        .write_all(format!("],\"event_count\":{},\"truncated\":{}}}", count, truncated).as_bytes())
        .await?;
    xml.write_all(
        format!("<metadata><event_count>{count}</event_count><truncated>{truncated}</truncated></metadata>\n</live_archive>\n").as_bytes(),
    )
    .await?;
    legacy.flush().await?;
    xml.flush().await?;
    // 先关闭句柄再改名：Windows 上文件仍被打开时 rename 会失败。
    drop(legacy);
    drop(xml);
    publish_staged_archive(&legacy_target).await?;
    publish_staged_archive(&xml_target).await?;
    // 同时生成标准 B 站弹幕 XML（PotPlayer 可识别）
    write_standard_bilibili_xml(paths).await?;
    Ok(truncated)
}

/// 将 events.jsonl 中的弹幕/SC 事件转换为标准 B 站弹幕 XML 格式，供 PotPlayer 挂载。
async fn write_standard_bilibili_xml(paths: &InteractionPaths) -> Result<()> {
    // 与 archive_legacy_and_xml 的 MAX_ARCHIVE_EVENTS 对齐：第二遍扫描同样限制事件数，
    // 避免损坏的 events.jsonl 让归档耗时/输出体积失去上界（超限截断但保证 XML 闭合）。
    const MAX_STANDARD_XML_EVENTS: usize = 500_000;
    // 重新从头读取 events.jsonl
    let file = tokio::fs::File::open(&paths.events).await?;
    let mut reader = BufReader::new(file).lines();

    // 与 legacy/XML 归档同口径：写中间文件，收尾后改名，abort 时不会留下半截 XML。
    let target = paths.standard_xml.clone();
    let mut xml = BufWriter::new(tokio::fs::File::create(archive_staging_path(&target)).await?);
    xml.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<i>\n\
          <chatserver>chat.bilibili.com</chatserver>\n\
          <mission>0</mission>\n\
          <maxlimit>8000</maxlimit>\n",
    )
    .await?;

    let mut count = 0u64;
    let mut seen = 0usize;
    let mut truncated = false;
    while let Some(line) = reader.next_line().await? {
        if seen >= MAX_STANDARD_XML_EVENTS {
            truncated = true;
            warn!(
                standard_xml = %paths.standard_xml.display(),
                "标准弹幕 XML 达到事件上限，已截断"
            );
            break;
        }
        seen += 1;
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<ArchivedLiveEvent>(&line) else {
            continue;
        };
        match event.event_type.as_str() {
            "danmaku" => {
                let text = event
                    .data
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if text.is_empty() {
                    continue;
                }
                let stime = event.media_time_ms as f64 / 1000.0;
                let mode = event
                    .data
                    .get("mode")
                    .and_then(Value::as_i64)
                    .unwrap_or(1)
                    .clamp(1, 8);
                let size = event
                    .data
                    .get("font_size")
                    .and_then(Value::as_i64)
                    .unwrap_or(25)
                    .clamp(1, 100);
                let color = event
                    .data
                    .get("color")
                    .and_then(Value::as_i64)
                    .unwrap_or(0xFFFFFF)
                    & 0xFFFFFF;
                xml.write_all(
                    format!(
                        "<d p=\"{:.5},{},{},{},0,0,0,0\">{}</d>\n",
                        stime,
                        mode,
                        size,
                        color,
                        escape_xml(text)
                    )
                    .as_bytes(),
                )
                .await?;
                count += 1;
            }
            "super_chat" => {
                let message = event
                    .data
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if message.is_empty() {
                    continue;
                }
                let stime = event.media_time_ms as f64 / 1000.0;
                // SC 用顶部固定弹幕(mode=5)
                xml.write_all(
                    format!(
                        "<d p=\"{:.5},5,25,16777215,0,0,0,0\">SC: {}</d>\n",
                        stime,
                        escape_xml(message)
                    )
                    .as_bytes(),
                )
                .await?;
                count += 1;
            }
            _ => {}
        }
    }
    xml.write_all(b"</i>\n").await?;
    xml.flush().await?;
    drop(xml);
    publish_staged_archive(&target).await?;
    info!(
        standard_xml = %paths.standard_xml.display(),
        count,
        truncated,
        "已生成标准 B 站弹幕 XML"
    );
    Ok(())
}

fn redact_uid_fields(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            for key in ["uid", "user_id", "ruid", "sender_uid", "guard_uid"] {
                if object.contains_key(key) {
                    object.insert(key.to_owned(), Value::String("redacted".to_owned()));
                }
            }
            for child in object.values_mut() {
                let replacement = redact_uid_fields(std::mem::take(child));
                *child = replacement;
            }
        }
        Value::Array(items) => {
            for child in items {
                let replacement = redact_uid_fields(std::mem::take(child));
                *child = replacement;
            }
        }
        _ => {}
    }
    value
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn escapes_xml_text() {
        assert_eq!(escape_xml("<a&b>"), "&lt;a&amp;b&gt;");
    }

    #[tokio::test]
    async fn partial_archive_never_occupies_the_final_file_name() {
        // 中止/报错留在磁盘上的只能是中间名：最终文件名要么承载完整文件，要么不存在。
        let temp = tempdir().expect("temporary directory");
        let target = temp.path().join("room_danmu.json");
        tokio::fs::write(archive_staging_path(&target), b"{\"events\":[tru")
            .await
            .expect("写入中间文件");
        assert!(!target.exists(), "半截归档不得占用最终文件名");

        publish_staged_archive(&target).await.expect("改名到位");
        assert_eq!(
            tokio::fs::read_to_string(&target).await.expect("读取归档"),
            "{\"events\":[tru"
        );
        assert!(!archive_staging_path(&target).exists());
    }

    #[tokio::test]
    async fn standard_xml_preserves_source_danmaku_style() {
        let temp = tempdir().expect("temporary directory");
        let paths = InteractionPaths {
            legacy: temp.path().join("danmu.json"),
            events: temp.path().join("events.jsonl"),
            xml: temp.path().join("danmaku.xml"),
            standard_xml: temp.path().join("danmaku_bilibili.xml"),
            summary: temp.path().join("summary.json"),
        };
        tokio::fs::write(
            &paths.events,
            r#"{"schema_version":1,"seq":1,"received_at":"2026-01-01T00:00:00Z","media_time_ms":1250,"segment_index":0,"cmd":"DANMU_MSG","event_type":"danmaku","data":{"text":"彩色","mode":4,"font_size":33,"color":1122867}}"#,
        )
        .await
        .expect("write event archive");

        write_standard_bilibili_xml(&paths)
            .await
            .expect("write standard XML");
        let xml = tokio::fs::read_to_string(&paths.standard_xml)
            .await
            .expect("read standard XML");
        assert!(xml.contains("1.25000,4,33,1122867"));
        assert!(xml.contains(">彩色</d>"));
    }
}
