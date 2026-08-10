use super::RecordingInfo;
use crate::services::danmu_collector::commands::{IncomingLiveCommand, LiveCommand};
use crate::services::live_source::CaptureMode;
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
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
    pub cancellation: CancellationToken,
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
    let mut legacy = tokio::fs::File::create(&args.paths.legacy).await?;
    let mut xml = tokio::fs::File::create(&args.paths.xml).await?;
    legacy.write_all(b"[").await?;
    xml.write_all(xml_header(args.room_id, &args.title).as_bytes())
        .await?;
    let mut first_legacy = true;
    let mut seq = 0_u64;
    let mut since_flush = 0_u32;
    let mut flush_tick = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut heat = Vec::<u64>::new();
    let mut paid_markers = Vec::<Value>::new();
    let mut link_markers = Vec::<Value>::new();
    let mut unique_users = HashSet::<i64>::new();
    let mut guard_dedupe = HashSet::<String>::new();

    loop {
        tokio::select! {
            _ = args.cancellation.cancelled() => {
                while let Ok(incoming) = rx.try_recv() {
                    process(&incoming, &args, &mut jsonl, &mut legacy, &mut xml, &mut first_legacy,
                        &mut seq, &mut heat, &mut paid_markers, &mut link_markers, &mut unique_users, &mut guard_dedupe).await?;
                }
                break;
            }
            _ = flush_tick.tick() => {
                jsonl.flush().await?; legacy.flush().await?; xml.flush().await?; since_flush = 0;
            }
            incoming = rx.recv() => {
                let Some(incoming) = incoming else { break };
                if matches!(incoming.command, LiveCommand::PlayurlReload) {
                    let _ = args.reload_tx.try_send(());
                    continue;
                }
                process(&incoming, &args, &mut jsonl, &mut legacy, &mut xml, &mut first_legacy,
                    &mut seq, &mut heat, &mut paid_markers, &mut link_markers, &mut unique_users, &mut guard_dedupe).await?;
                since_flush += 1;
                if since_flush >= 100 { jsonl.flush().await?; legacy.flush().await?; xml.flush().await?; since_flush = 0; }
            }
        }
    }

    legacy.write_all(b"\n]\n").await?;
    xml.write_all(b"</i>\n").await?;
    jsonl.flush().await?;
    legacy.flush().await?;
    xml.flush().await?;
    let snapshot = args.snapshot.lock().await.clone();
    let summary = json!({
        "schema_version": 1, "room_id": args.room_id, "title": args.title,
        "capture_mode": args.mode.as_str(), "finished_at": chrono::Utc::now().to_rfc3339(),
        "danmaku_count": snapshot.danmaku_count, "unique_user_count": snapshot.unique_user_count,
        "free_gift_count": snapshot.free_gift_count, "paid_gift_count": snapshot.paid_gift_count,
        "sc_count": snapshot.sc_count, "guard_count": snapshot.guard_count,
        "peak_watched": snapshot.peak_watched, "estimated_paid_value": snapshot.estimated_paid_value,
        "dropped_event_count": snapshot.dropped_event_count, "capture_gaps": [],
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
    legacy: &mut tokio::fs::File,
    xml: &mut tokio::fs::File,
    first_legacy: &mut bool,
    seq: &mut u64,
    heat: &mut Vec<u64>,
    paid_markers: &mut Vec<Value>,
    link_markers: &mut Vec<Value>,
    unique_users: &mut HashSet<i64>,
    guard_dedupe: &mut HashSet<String>,
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
    if uid > 0 {
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
    if !*first_legacy {
        legacy.write_all(b",").await?;
    }
    *first_legacy = false;
    legacy.write_all(b"\n").await?;
    legacy.write_all(&serde_json::to_vec(&data)?).await?;
    write_xml(xml, &incoming.command, media_time_ms).await?;

    {
        let mut snapshot = args.snapshot.lock().await;
        snapshot.last_event_seq = *seq;
        snapshot.unique_user_count = unique_users.len() as i64;
        match &incoming.command {
            LiveCommand::Danmaku { .. } => {
                snapshot.danmaku_count += 1;
                let bucket = (media_time_ms / 30_000) as usize;
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
                    paid_markers.push(json!({"time_ms":media_time_ms,"type":"gift"}));
                } else {
                    snapshot.free_gift_count += i64::from(*num);
                }
            }
            LiveCommand::SuperChat { price, .. } => {
                snapshot.sc_count += 1;
                snapshot.estimated_paid_value += f64::from(*price);
                paid_markers.push(json!({"time_ms":media_time_ms,"type":"sc"}));
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
                if guard_dedupe.insert(key) {
                    snapshot.guard_count += i64::from(*num);
                    snapshot.estimated_paid_value += f64::from(*price) / 1000.0;
                    paid_markers.push(json!({"time_ms":media_time_ms,"type":"guard"}));
                }
            }
            LiveCommand::WatchedChange { count } => {
                snapshot.peak_watched = snapshot.peak_watched.max(*count)
            }
            _ => {}
        }
        if incoming.cmd == "CAPTURE_GAP" {
            snapshot.dropped_event_count += incoming
                .raw
                .pointer("/data/dropped")
                .and_then(Value::as_i64)
                .unwrap_or(0);
        }
    }
    if is_link {
        link_markers.push(json!({"time_ms":media_time_ms,"cmd":incoming.cmd}));
    }
    let mut recent = args.recent.lock().await;
    recent.push_back(event);
    while recent.len() > 100 {
        recent.pop_front();
    }
    Ok(())
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

fn is_link_command(cmd: &str) -> bool {
    let base = cmd.split(':').next().unwrap_or(cmd);
    ["VOICE_JOIN", "LINK_MIC", "PK_", "LIVE_MULTI_VIEW"]
        .iter()
        .any(|prefix| base.starts_with(prefix))
}

async fn write_xml(
    file: &mut tokio::fs::File,
    command: &LiveCommand,
    media_time_ms: i64,
) -> Result<()> {
    let seconds = media_time_ms as f64 / 1000.0;
    let line = match command {
        LiveCommand::Danmaku { uid, text, mode, font_size, color, .. } => format!("<d p=\"{seconds:.3},{mode},{font_size},{color},0,0,{uid},0\">{}</d>\n", escape_xml(text)),
        LiveCommand::Gift { uid, uname, gift_name, num, total_coin, coin_type, .. } => format!("<gift ts=\"{seconds:.3}\" uid=\"{uid}\" user=\"{}\" giftname=\"{}\" giftcount=\"{num}\" total_coin=\"{total_coin}\" coin_type=\"{}\" />\n", escape_xml(uname), escape_xml(gift_name), escape_xml(coin_type)),
        LiveCommand::SuperChat { uid, uname, message, price, duration, .. } => format!("<sc ts=\"{seconds:.3}\" uid=\"{uid}\" user=\"{}\" price=\"{price}\" time=\"{duration}\">{}</sc>\n", escape_xml(uname), escape_xml(message)),
        LiveCommand::GuardBuy { uid, uname, guard_level, price, num, .. } => format!("<guard ts=\"{seconds:.3}\" uid=\"{uid}\" user=\"{}\" level=\"{guard_level}\" price=\"{price}\" num=\"{num}\" />\n", escape_xml(uname)),
        _ => return Ok(()),
    };
    file.write_all(line.as_bytes()).await?;
    Ok(())
}

fn xml_header(room_id: i64, title: &str) -> String {
    format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<?xml-stylesheet type="text/xsl" href="#style"?>
<i><chatserver>live.bilibili.com</chatserver><chatid>{room_id}</chatid><mission>0</mission><maxlimit>0</maxlimit><state>0</state><real_name>0</real_name><source>k-v</source><title>{}</title>
<xsl:stylesheet id="style" version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:template match="/"><html><head><meta charset="utf-8"/><style>body{{font:14px sans-serif;background:#111;color:#eee}}table{{border-collapse:collapse;width:100%}}td,th{{padding:6px;border-bottom:1px solid #333;text-align:left}}</style></head><body><h2>直播互动档案</h2><table><tr><th>时间</th><th>类型</th><th>内容</th></tr><xsl:for-each select="i/*[self::d or self::gift or self::sc or self::guard]"><tr><td><xsl:value-of select="@ts|substring-before(@p, ',')"/></td><td><xsl:value-of select="name()"/></td><td><xsl:value-of select="."/><xsl:value-of select="@giftname"/></td></tr></xsl:for-each></table></body></html></xsl:template></xsl:stylesheet>
"##,
        escape_xml(title)
    )
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
}
