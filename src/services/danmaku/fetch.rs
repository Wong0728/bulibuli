//! protobuf 弹幕下载：分段拉取 seg.so、解析并落盘为 XML/JSON/TXT。

use crate::services::dm_proto;
use crate::services::wbi;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{error, info, warn};

use super::archive::archive_sidecar_files;
use super::{cookie_header, cookie_map, DanmakuService, SidecarArchivePolicy, SEGMENT_DURATION};

const MAX_DANMAKU_SEGMENTS: usize = 512;
const MAX_DANMAKU_SEGMENT_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn capped_segment_count(duration: i64) -> (usize, bool) {
    let requested = if duration <= 0 {
        1
    } else {
        ((duration + SEGMENT_DURATION - 1) / SEGMENT_DURATION).max(1) as usize
    };
    (
        requested.min(MAX_DANMAKU_SEGMENTS),
        requested > MAX_DANMAKU_SEGMENTS,
    )
}

async fn read_limited_response(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(anyhow!("danmaku segment exceeds {max_bytes} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl DanmakuService {
    /// 获取视频 cid 与 duration（秒）。复用 BiliApi::get_video_info() 使用 WBI 签名。
    async fn get_video_meta(
        &self,
        bvid: &str,
        cookies: &HashMap<String, String>,
        page: Option<i32>,
    ) -> Result<(i64, i64)> {
        let info = self
            .bili_api
            .get_video_info(bvid, &cookie_header(cookies))
            .await?;
        let selected = page
            .and_then(|number| info.pages.iter().find(|item| item.page == number))
            .or_else(|| info.pages.first());
        let (cid, duration) = selected
            .map(|item| (item.cid, item.duration))
            .unwrap_or((info.cid, info.duration));
        if cid <= 0 {
            return Err(anyhow!("cid 不存在"));
        }
        Ok((cid, duration))
    }

    /// 分段拉取并立即解析 protobuf，避免同时在内存保留全部原始分段。
    /// 单段失败不中断，记 WARN 后继续下一段。
    async fn fetch_protobuf_segments(
        &self,
        bvid: &str,
        cid: i64,
        duration: i64,
        cookies: &HashMap<String, String>,
    ) -> Result<(Vec<Value>, usize, usize, Vec<usize>, bool)> {
        // 先合并设备指纹，再获取 WBI keys：nav 接口在新风控下要求携带登录态 Cookie
        let enriched = self
            .cookie_manager
            .enrich(&cookie_header(cookies))
            .await
            .unwrap_or_else(|_| cookie_header(cookies));
        let (img_key, sub_key) = self.bili_api.get_wbi_keys_public(&enriched).await?;
        let referer = format!("https://www.bilibili.com/video/{bvid}");

        let (parts, truncated) = capped_segment_count(duration);

        let mut list = Vec::new();
        let mut successful_segments = 0usize;
        let mut failed_segments = Vec::new();
        for index in 1..=parts {
            failed_segments.push(index);
            let mut params = HashMap::new();
            params.insert("type".to_string(), "1".to_string());
            params.insert("oid".to_string(), cid.to_string());
            params.insert("segment_index".to_string(), index.to_string());
            if let Err(e) = wbi::enc_wbi(&mut params, &img_key, &sub_key) {
                warn!("弹幕分段 {index} WBI 签名失败: {e}");
                continue;
            }

            match self
                .bili_api
                .send_get_public(
                    "https://api.bilibili.com/x/v2/dm/wbi/web/seg.so",
                    &params,
                    &referer,
                    &enriched,
                )
                .await
            {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        warn!("弹幕分段 {index} HTTP {} 失败", resp.status());
                        continue;
                    }
                    match read_limited_response(resp, MAX_DANMAKU_SEGMENT_BYTES).await {
                        Ok(bytes) => {
                            if bytes.is_empty() {
                                warn!("弹幕分段 {index} 返回空数据");
                                continue;
                            }
                            list.extend(dm_proto::parse_danmaku_bytes(&bytes));
                            successful_segments += 1;
                            failed_segments.retain(|failed| *failed != index);
                        }
                        Err(e) => warn!("弹幕分段 {index} 读取字节失败: {e}"),
                    }
                }
                Err(e) => warn!("弹幕分段 {index} 请求失败: {e}"),
            }
        }
        Ok((list, successful_segments, parts, failed_segments, truncated))
    }

    /// 下载弹幕，支持指定保存目录（用于手动下载时与视频放在同一目录）
    pub async fn download_danmaku_to(
        &self,
        bvid: &str,
        page: Option<i32>,
        cookies_str: Option<&str>,
        uid: Option<&str>,
        archive_policy: SidecarArchivePolicy,
        save_dir_override: Option<&std::path::Path>,
    ) -> Result<Value> {
        info!("[弹幕] 开始下载: bvid={bvid}, page={page:?}, uid={:?}", uid);
        let cookies = cookies_str.map(cookie_map).unwrap_or_default();

        if page.is_none() {
            let info = self
                .bili_api
                .get_video_info(bvid, &cookie_header(&cookies))
                .await?;
            let pages = info
                .pages
                .iter()
                .filter(|item| item.cid > 0)
                .take(100)
                .collect::<Vec<_>>();
            if pages.len() > 1 {
                let mut results = Vec::with_capacity(pages.len());
                for item in pages {
                    let result = self
                        .download_danmaku_page(
                            bvid,
                            Some(item.page),
                            cookies_str,
                            uid,
                            archive_policy,
                            save_dir_override,
                        )
                        .await?;
                    results.push(json!({
                        "page": item.page,
                        "cid": item.cid,
                        "result": result,
                    }));
                }
                let success = results.iter().all(|item| {
                    item.pointer("/result/success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                });
                return Ok(json!({
                    "success": success,
                    "partial": !success,
                    "message": if success { "所有分P弹幕下载完成" } else { "部分分P弹幕下载失败" },
                    "pages": results,
                }));
            }
        }
        self.download_danmaku_page(
            bvid,
            page,
            cookies_str,
            uid,
            archive_policy,
            save_dir_override,
        )
        .await
    }

    async fn download_danmaku_page(
        &self,
        bvid: &str,
        page: Option<i32>,
        cookies_str: Option<&str>,
        uid: Option<&str>,
        archive_policy: SidecarArchivePolicy,
        save_dir_override: Option<&std::path::Path>,
    ) -> Result<Value> {
        info!("[弹幕] 开始下载: bvid={bvid}, page={page:?}, uid={:?}", uid);
        let cookies = cookies_str.map(cookie_map).unwrap_or_default();

        let (cid, duration) = match self.get_video_meta(bvid, &cookies, page).await {
            Ok(v) => {
                info!(
                    "[弹幕] 视频元信息: bvid={bvid}, cid={}, duration={}",
                    v.0, v.1
                );
                v
            }
            Err(e) => {
                error!("[弹幕] 获取视频信息失败 {bvid}: {e}");
                return Ok(json!({
                    "success": false,
                    "message": format!("获取视频信息失败: {e}"),
                }));
            }
        };

        let (mut list, segment_count, expected_segments, failed_segments, truncated) = match self
            .fetch_protobuf_segments(bvid, cid, duration, &cookies)
            .await
        {
            Ok((_, 0, _, failed_segments, truncated)) => {
                warn!("[弹幕] 所有分段拉取失败（可能 Cookies 已过期或被风控）: bvid={bvid}");
                return Ok(json!({
                    "success": false,
                    "partial": true,
                    "failed_segments": failed_segments,
                    "truncated": truncated,
                    "message": "所有弹幕分段拉取失败（可能 Cookies 已过期或被风控）",
                }));
            }
            Ok(result) => {
                info!("[弹幕] 分段拉取成功: bvid={bvid}, 共 {} 个分段", result.1);
                result
            }
            Err(e) => {
                error!("[弹幕] 拉取弹幕分段失败 {bvid}: {e}");
                return Ok(json!({
                    "success": false,
                    "message": format!("拉取弹幕分段失败: {e}"),
                }));
            }
        };

        let partial = truncated || !failed_segments.is_empty() || segment_count < expected_segments;

        if list.is_empty() {
            info!("[弹幕] 该视频暂无弹幕: bvid={bvid}");
            return Ok(json!({
                "success": !partial,
                "message": "该视频暂无弹幕",
                "count": 0,
                "file_path": null,
                "partial": partial,
                "failed_segments": failed_segments,
                "truncated": truncated,
            }));
        }

        list.sort_by(|a, b| {
            a["time"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&b["time"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let save_dir = self.save_dir(uid, save_dir_override).await?;
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            error!("[弹幕] 创建目录失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("创建目录失败: {e}"),
            }));
        }

        let stem = page
            .map(|number| format!("{bvid}_p{number}"))
            .unwrap_or_else(|| bvid.to_string());
        let xml_path = save_dir.join(format!("{stem}_danmaku.xml"));
        let json_path = save_dir.join(format!("{stem}_danmaku.json"));
        let txt_path = save_dir.join(format!("{stem}_danmaku.txt"));

        let xml_content = self.serialize_xml(&stem, cid, &list);
        let json_data = json!({
            "video_info": {
                "bvid": bvid,
                "cid": cid,
                "duration": duration,
                "total_count": list.len(),
                "download_time": Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                "source": "protobuf_seg.so",
                "segments": segment_count,
                "expected_segments": expected_segments,
                "partial": partial,
                "failed_segments": failed_segments,
                "truncated": truncated,
            },
            "danmaku_list": list,
        });

        if let Err(e) = tokio::fs::write(&xml_path, xml_content.as_bytes()).await {
            error!("[弹幕] 写入 XML 失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("写入 XML 失败: {e}"),
            }));
        }
        let json_str = match serde_json::to_string_pretty(&json_data) {
            Ok(s) => s,
            Err(e) => {
                error!("[弹幕] 序列化 JSON 失败 {bvid}: {e}");
                return Ok(json!({
                    "success": false,
                    "message": format!("序列化 JSON 失败: {e}"),
                }));
            }
        };
        if let Err(e) = tokio::fs::write(&json_path, json_str).await {
            error!("[弹幕] 写入 JSON 失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("写入 JSON 失败: {e}"),
            }));
        }
        let txt_content = self.format_danmaku_txt(&stem, cid, &list);
        if let Err(e) = tokio::fs::write(&txt_path, txt_content).await {
            error!("[弹幕] 写入 TXT 失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("写入 TXT 失败: {e}"),
            }));
        }
        if let Err(e) = archive_sidecar_files(
            &save_dir,
            bvid,
            "danmaku",
            &[xml_path.clone(), json_path.clone(), txt_path.clone()],
            archive_policy,
        )
        .await
        {
            error!("[弹幕] 归档失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("归档弹幕文件失败: {e}"),
            }));
        }

        info!(
            "[弹幕] 下载完成: bvid={bvid}, 共 {} 条（{} 个分段）",
            list.len(),
            segment_count
        );
        Ok(json!({
            "success": !partial,
            "message": format!("弹幕下载完成，共 {} 条（{} 个分段）", list.len(), segment_count),
            "files": [
                xml_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                json_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                txt_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            ],
            "count": list.len(),
            "partial": partial,
            "failed_segments": failed_segments,
            "truncated": truncated,
        }))
    }

    /// 将解析后的弹幕列表重新序列化为 B 站传统 XML 格式。
    fn serialize_xml(&self, bvid: &str, cid: i64, list: &[Value]) -> String {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<i>\n");
        s.push_str(&format!(
            "<!-- bvid={bvid} cid={cid} count={} generated={} -->\n",
            list.len(),
            Local::now().format("%Y-%m-%d %H:%M:%S")
        ));
        for dm in list {
            let time = dm["time"].as_f64().unwrap_or(0.0);
            let mode = dm["type"].as_i64().unwrap_or(1);
            let size = dm["size"].as_i64().unwrap_or(25);
            let color = dm["color"].as_i64().unwrap_or(16777215);
            let ts = dm["timestamp"].as_i64().unwrap_or(0);
            let pool = dm["pool"].as_i64().unwrap_or(0);
            let hash = dm["hash"].as_str().unwrap_or("");
            let dmid = dm["dmid"].as_str().unwrap_or("");
            let text = dm["text"].as_str().unwrap_or("");
            let escaped = text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;");
            s.push_str(&format!(
                "<d p=\"{time},{mode},{size},{color},{ts},{pool},{hash},{dmid}\">{escaped}</d>\n"
            ));
        }
        s.push_str("</i>");
        s
    }

    fn format_danmaku_txt(&self, bvid: &str, cid: i64, list: &[Value]) -> String {
        let mut lines = vec![
            format!("视频: {bvid}"),
            format!("CID: {cid}"),
            format!("弹幕总数: {}", list.len()),
            format!("下载时间: {}", Local::now().format("%Y-%m-%d %H:%M:%S")),
            "=".repeat(80),
            "格式说明: [视频内时间] 类型 字号 颜色 发送时间 | 弹幕内容".to_string(),
            "-".repeat(80),
        ];
        for dm in list {
            let t = dm["time"].as_f64().unwrap_or(0.0);
            lines.push(format!(
                "[{:>8}] {:>2} {:>2} #{:06X} {} | {}",
                self.format_video_time(t),
                self.type_name(dm["type"].as_i64().unwrap_or(1)),
                self.size_name(dm["size"].as_i64().unwrap_or(25)),
                dm["color"].as_i64().unwrap_or(0),
                DateTime::from_timestamp(dm["timestamp"].as_i64().unwrap_or(0), 0)
                    .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default(),
                dm["text"].as_str().unwrap_or("")
            ));
        }
        lines.join("\n")
    }

    fn format_video_time(&self, seconds: f64) -> String {
        let h = (seconds / 3600.0) as i64;
        let m = ((seconds % 3600.0) / 60.0) as i64;
        let s = (seconds % 60.0) as i64;
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m:02}:{s:02}")
        }
    }

    fn type_name(&self, t: i64) -> &'static str {
        match t {
            1..=3 => "普通",
            4 => "底部",
            5 => "顶部",
            6 => "逆向",
            7 => "高级",
            8 => "代码",
            9 => "BAS",
            _ => "未知",
        }
    }

    fn size_name(&self, s: i64) -> &'static str {
        match s {
            18 => "小",
            25 => "标准",
            36 => "大",
            _ => "",
        }
    }
}
