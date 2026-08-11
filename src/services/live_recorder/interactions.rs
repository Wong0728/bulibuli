use super::RecordingInfo;
use crate::services::danmu_collector::commands::{
    is_link_command, IncomingLiveCommand, LiveCommand,
};
use crate::services::live_source::CaptureMode;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tracing::info;

const MAX_HEAT_BUCKETS: usize = 1_440;
const MAX_PAID_MARKERS: usize = 4_096;
const MAX_LINK_MARKERS: usize = 1_024;
const MAX_UNIQUE_USERS: usize = 100_000;
const MAX_GUARD_DEDUPE: usize = 10_000;
const MAX_CAPTURE_GAPS: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArchivedLiveEvent {
    pub schema_version: u8,
    pub seq: u64,
    pub received_at: String,
    pub media_time_ms: i64,
    pub segment_index: u32,
    pub cmd: String,
    pub event_type: String,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_backfill: bool,
}

#[derive(Clone)]
pub struct InteractionPaths {
    pub legacy: PathBuf,
    pub events: PathBuf,
    pub xml: PathBuf,
    /// 标准 B 站弹幕 XML（PotPlayer 可识别），命名 `{prefix}_danmaku_bilibili.xml`
    pub standard_xml: PathBuf,
    pub summary: PathBuf,
}

pub struct InteractionWriterArgs {
    pub room_id: i64,
    pub title: String,
    pub mode: CaptureMode,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub paths: InteractionPaths,
    pub snapshot: Arc<Mutex<RecordingInfo>>,
    pub recent: Arc<Mutex<VecDeque<ArchivedLiveEvent>>>,
    pub reload_tx: mpsc::Sender<()>,
    pub segment_index: Arc<AtomicU32>,
}

pub async fn run(
    rx: &mut mpsc::Receiver<IncomingLiveCommand>,
    args: InteractionWriterArgs,
) -> Result<()> {
    if let Some(parent) = args.paths.events.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut jsonl = tokio::fs::File::create(&args.paths.events).await?;
    let mut seq = 0_u64;
    let mut since_flush = 0_u32;
    let mut flush_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut heat = Vec::<u64>::new();
    let mut paid_markers = Vec::<Value>::new();
    let mut link_markers = Vec::<Value>::new();
    let mut unique_users = HashSet::<i64>::new();
    let mut guard_dedupe = HashSet::<String>::new();
    let mut capture_gaps = Vec::<Value>::new();

    loop {
        tokio::select! {
            _ = flush_tick.tick() => {
                jsonl.flush().await?; since_flush = 0;
            }
            incoming = rx.recv() => {
                let Some(incoming) = incoming else { break };
                if matches!(incoming.command, LiveCommand::PlayurlReload) {
                    let _ = args.reload_tx.try_send(());
                    continue;
                }
                process(&incoming, &args, &mut jsonl, &mut seq, &mut heat, &mut paid_markers,
                    &mut link_markers, &mut unique_users, &mut guard_dedupe, &mut capture_gaps).await?;
                since_flush += 1;
                if since_flush >= 100 { jsonl.flush().await?; since_flush = 0; }
            }
        }
    }

    jsonl.flush().await?;
    let archive_truncated = archive_legacy_and_xml(&args.paths, args.room_id, &args.title).await?;
    let snapshot = args.snapshot.lock().await.clone();
    let summary = json!({
        "schema_version": 1, "room_id": args.room_id, "title": args.title,
        "capture_mode": args.mode.as_str(), "finished_at": chrono::Utc::now().to_rfc3339(),
        "danmaku_count": snapshot.danmaku_count, "unique_user_count": snapshot.unique_user_count,
        "free_gift_count": snapshot.free_gift_count, "paid_gift_count": snapshot.paid_gift_count,
        "sc_count": snapshot.sc_count, "guard_count": snapshot.guard_count,
        "peak_watched": snapshot.peak_watched, "estimated_paid_value": snapshot.estimated_paid_value,
        "dropped_event_count": snapshot.dropped_event_count, "capture_gaps": capture_gaps,
        "archive_truncated": archive_truncated,
        "danmaku_density_30s": heat, "paid_markers": paid_markers, "link_mic_pk_markers": link_markers,
    });
    tokio::fs::write(&args.paths.summary, serde_json::to_vec_pretty(&summary)?).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn process(
    incoming: &IncomingLiveCommand,
    args: &InteractionWriterArgs,
    jsonl: &mut tokio::fs::File,
    seq: &mut u64,
    heat: &mut Vec<u64>,
    paid_markers: &mut Vec<Value>,
    link_markers: &mut Vec<Value>,
    unique_users: &mut HashSet<i64>,
    guard_dedupe: &mut HashSet<String>,
    capture_gaps: &mut Vec<Value>,
) -> Result<()> {
    if incoming.cmd == "DANMU_CONNECTION_STATUS" {
        let mut snapshot = args.snapshot.lock().await;
        snapshot.interaction_capture_status = incoming
            .raw
            .pointer("/data/status")
            .and_then(Value::as_str)
            .unwrap_or("degraded")
            .to_owned();
        snapshot.interaction_error = incoming
            .raw
            .pointer("/data/error")
            .and_then(Value::as_str)
            .map(str::to_owned);
        snapshot.danmu_unavailable = snapshot.interaction_capture_status == "unavailable";
        return Ok(());
    }
    if args.mode == CaptureMode::Standard
        && matches!(incoming.command, LiveCommand::Interact { .. })
    {
        return Ok(());
    }
    *seq += 1;
    let media_time_ms = incoming
        .received_at
        .signed_duration_since(args.started_at)
        .num_milliseconds()
        .max(0);
    let (kind, data, uid) = if incoming.cmd == "CAPTURE_GAP" {
        (
            "capture_gap",
            incoming.raw.get("data").cloned().unwrap_or(Value::Null),
            0,
        )
    } else {
        normalize(&incoming.command, &incoming.cmd)
    };
    if uid > 0 && unique_users.len() < MAX_UNIQUE_USERS {
        unique_users.insert(uid);
    }
    let is_link = is_link_command(&incoming.cmd);
    let keep_raw = args.mode == CaptureMode::Full || is_link || kind == "unknown";
    let event = ArchivedLiveEvent {
        schema_version: 1,
        seq: *seq,
        received_at: incoming.received_at.to_rfc3339(),
        media_time_ms,
        segment_index: args.segment_index.load(Ordering::Relaxed),
        cmd: incoming.cmd.clone(),
        event_type: kind.to_owned(),
        data: data.clone(),
        raw: keep_raw.then(|| incoming.raw.clone()),
        history_backfill: incoming.history_backfill,
    };
    jsonl.write_all(&serde_json::to_vec(&event)?).await?;
    jsonl.write_all(b"\n").await?;

    {
        let mut snapshot = args.snapshot.lock().await;
        snapshot.last_event_seq = *seq;
        snapshot.unique_user_count = unique_users.len() as i64;
        match &incoming.command {
            LiveCommand::Danmaku { .. } => {
                snapshot.danmaku_count += 1;
                let bucket = (media_time_ms / 30_000) as usize;
                let bucket = bucket.min(MAX_HEAT_BUCKETS.saturating_sub(1));
                if heat.len() <= bucket {
                    heat.resize(bucket + 1, 0);
                }
                heat[bucket] += 1;
            }
            LiveCommand::Gift {
                num,
                total_coin,
                coin_type,
                ..
            } => {
                if let Some(value) = gift_paid_value(coin_type, *total_coin) {
                    snapshot.paid_gift_count += i64::from(*num);
                    snapshot.estimated_paid_value += value;
                    push_bounded(
                        paid_markers,
                        json!({"time_ms":media_time_ms,"type":"gift"}),
                        MAX_PAID_MARKERS,
                    );
                } else {
                    snapshot.free_gift_count += i64::from(*num);
                }
            }
            LiveCommand::SuperChat { price, .. } => {
                snapshot.sc_count += 1;
                snapshot.estimated_paid_value += f64::from(*price);
                push_bounded(
                    paid_markers,
                    json!({"time_ms":media_time_ms,"type":"sc"}),
                    MAX_PAID_MARKERS,
                );
            }
            LiveCommand::GuardBuy {
                uid,
                guard_level,
                price,
                num,
                order_id,
                ..
            } => {
                let key = if order_id.is_empty() {
                    format!("{uid}:{guard_level}:{num}:{}", media_time_ms / 10_000)
                } else {
                    order_id.clone()
                };
                if guard_dedupe.len() >= MAX_GUARD_DEDUPE {
                    if let Some(oldest) = guard_dedupe.iter().next().cloned() {
                        guard_dedupe.remove(&oldest);
                    }
                }
                if guard_dedupe.insert(key) {
                    snapshot.guard_count += i64::from(*num);
                    snapshot.estimated_paid_value += f64::from(*price) / 1000.0;
                    push_bounded(
                        paid_markers,
                        json!({"time_ms":media_time_ms,"type":"guard"}),
                        MAX_PAID_MARKERS,
                    );
                }
            }
            LiveCommand::WatchedChange { count } => {
                snapshot.peak_watched = snapshot.peak_watched.max(*count)
            }
            _ => {}
        }
        if incoming.cmd == "CAPTURE_GAP" {
            push_bounded(
                capture_gaps,
                incoming.raw.get("data").cloned().unwrap_or(Value::Null),
                MAX_CAPTURE_GAPS,
            );
            snapshot.dropped_event_count += incoming
                .raw
                .pointer("/data/dropped")
                .and_then(Value::as_i64)
                .unwrap_or(0);
        }
    }
    if is_link {
        push_bounded(
            link_markers,
            json!({"time_ms":media_time_ms,"cmd":incoming.cmd}),
            MAX_LINK_MARKERS,
        );
    }
    let mut recent = args.recent.lock().await;
    recent.push_back(event);
    while recent.len() > 100 {
        recent.pop_front();
    }
    Ok(())
}

fn push_bounded(values: &mut Vec<Value>, value: Value, limit: usize) {
    values.push(value);
    if values.len() > limit {
        let overflow = values.len() - limit;
        values.drain(..overflow);
    }
}

async fn archive_legacy_and_xml(
    paths: &InteractionPaths,
    room_id: i64,
    title: &str,
) -> Result<bool> {
    const MAX_ARCHIVE_EVENTS: usize = 500_000;
    const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
    // ponytail: bounded archive output prevents one corrupt JSONL from exhausting memory;
    // move archival to an external stream processor if larger captures become a requirement.
    let mut input = BufReader::new(tokio::fs::File::open(&paths.events).await?).lines();
    let mut legacy = tokio::fs::File::create(&paths.legacy).await?;
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

    let mut xml = tokio::fs::File::create(&paths.xml).await?;
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
    // 同时生成标准 B 站弹幕 XML（PotPlayer 可识别）
    write_standard_bilibili_xml(paths).await?;
    Ok(truncated)
}

/// 将 events.jsonl 中的弹幕/SC 事件转换为标准 B 站弹幕 XML 格式，供 PotPlayer 挂载。
async fn write_standard_bilibili_xml(paths: &InteractionPaths) -> Result<()> {
    // 重新从头读取 events.jsonl
    let file = tokio::fs::File::open(&paths.events).await?;
    let mut reader = BufReader::new(file).lines();

    let mut xml = tokio::fs::File::create(&paths.standard_xml).await?;
    xml.write_all(
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<i>\n\
          <chatserver>chat.bilibili.com</chatserver>\n\
          <mission>0</mission>\n\
          <maxlimit>8000</maxlimit>\n",
    )
    .await?;

    let mut count = 0u64;
    while let Some(line) = reader.next_line().await? {
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
    info!(
        standard_xml = %paths.standard_xml.display(),
        count,
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

fn normalize(command: &LiveCommand, cmd: &str) -> (&'static str, Value, i64) {
    match command {
        LiveCommand::Danmaku { uid, .. } => (
            "danmaku",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::Gift { uid, .. } => (
            "gift",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::SuperChat { uid, .. } => (
            "super_chat",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::GuardBuy { uid, .. } => (
            "guard",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::Interact { uid, .. } => (
            "interact",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::WatchedChange { .. } => (
            "watched",
            serde_json::to_value(command).unwrap_or(Value::Null),
            0,
        ),
        LiveCommand::Like { uid, .. } => (
            "like",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::EntryEffect { uid, .. } => (
            "entry",
            serde_json::to_value(command).unwrap_or(Value::Null),
            *uid,
        ),
        LiveCommand::Stats { .. } => (
            "stats",
            serde_json::to_value(command).unwrap_or(Value::Null),
            0,
        ),
        LiveCommand::System { .. }
        | LiveCommand::LiveStart { .. }
        | LiveCommand::LiveEnd { .. } => (
            "system",
            serde_json::to_value(command).unwrap_or(Value::Null),
            0,
        ),
        LiveCommand::LinkMicPk { .. } => (
            "link_mic_pk",
            serde_json::to_value(command).unwrap_or(Value::Null),
            0,
        ),
        _ if is_link_command(cmd) => (
            "link_mic_pk",
            serde_json::to_value(command).unwrap_or(Value::Null),
            0,
        ),
        _ => (
            "unknown",
            serde_json::to_value(command).unwrap_or(Value::Null),
            0,
        ),
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn gift_paid_value(coin_type: &str, total_coin: i64) -> Option<f64> {
    (coin_type == "gold" && total_coin > 0).then_some(total_coin as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_recording_info() -> RecordingInfo {
        RecordingInfo {
            room_id: 1,
            recording_id: None,
            title: "tail drain".to_owned(),
            status: super::super::RecordingStatus::Recording,
            output_path: String::new(),
            started_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: 0,
            file_size: 0,
            error_msg: None,
            danmu_unavailable: false,
            stream_quality: None,
            stream_protocol: None,
            stream_format: None,
            stream_codec: None,
            trigger: "manual".to_owned(),
            capture_mode: "standard".to_owned(),
            interaction_capture_status: "capturing".to_owned(),
            interaction_error: None,
            event_path: None,
            xml_path: None,
            summary_path: None,
            danmaku_count: 0,
            unique_user_count: 0,
            free_gift_count: 0,
            paid_gift_count: 0,
            sc_count: 0,
            guard_count: 0,
            peak_watched: 0,
            dropped_event_count: 0,
            estimated_paid_value: 0.0,
            last_event_seq: 0,
        }
    }
    #[test]
    fn identifies_link_commands() {
        assert!(is_link_command("PK_BATTLE_START_NEW"));
        assert!(is_link_command("VOICE_JOIN_STATUS"));
    }
    #[test]
    fn escapes_xml_text() {
        assert_eq!(escape_xml("<a&b>"), "&lt;a&amp;b&gt;");
    }
    #[test]
    fn free_gifts_are_excluded_from_estimated_paid_value() {
        assert_eq!(gift_paid_value("silver", 1000), None);
        assert_eq!(gift_paid_value("gold", 0), None);
        assert_eq!(gift_paid_value("gold", 2500), Some(2.5));
    }

    #[tokio::test]
    async fn writer_drains_tail_event_after_collector_sender_closes() {
        let temp = tempdir().expect("temporary directory");
        let paths = InteractionPaths {
            legacy: temp.path().join("danmu.json"),
            events: temp.path().join("events.jsonl"),
            xml: temp.path().join("danmaku.xml"),
            standard_xml: temp.path().join("danmaku_bilibili.xml"),
            summary: temp.path().join("summary.json"),
        };
        let (tx, mut rx) = mpsc::channel(4);
        let (reload_tx, _reload_rx) = mpsc::channel(1);
        let args = InteractionWriterArgs {
            room_id: 1,
            title: "tail drain".to_owned(),
            mode: CaptureMode::Standard,
            started_at: chrono::Utc::now(),
            paths: paths.clone(),
            snapshot: Arc::new(Mutex::new(test_recording_info())),
            recent: Arc::new(Mutex::new(VecDeque::new())),
            reload_tx,
            segment_index: Arc::new(AtomicU32::new(0)),
        };
        let writer = tokio::spawn(async move { run(&mut rx, args).await });
        tx.send(IncomingLiveCommand::from_json(serde_json::json!({
            "cmd": "DANMU_MSG", "info": [[], "tail event", [42, "tester"]]
        })))
        .await
        .expect("queue tail event");
        drop(tx);
        writer.await.expect("writer task").expect("writer result");

        let jsonl = tokio::fs::read_to_string(&paths.events)
            .await
            .expect("events archive");
        let legacy = tokio::fs::read_to_string(&paths.legacy)
            .await
            .expect("legacy archive");
        let xml = tokio::fs::read_to_string(&paths.xml)
            .await
            .expect("xml archive");
        assert!(jsonl.contains("tail event"));
        assert!(legacy.contains("tail event"));
        assert!(xml.contains("tail event"));
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
