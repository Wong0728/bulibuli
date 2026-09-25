//! protobuf 弹幕下载编排：多分P展开、分段拉取结果汇总、partial 判定与
//! XML / JSON / TXT 落盘归档。分段请求见 `fetch_segments`，序列化见 `danmaku_serialize`。

use anyhow::Result;
use chrono::Local;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use super::archive::archive_sidecar_files;
use super::{cookie_map, DanmakuService, SidecarArchivePolicy};

impl DanmakuService {
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
                .get_video_info(bvid, &super::cookie_header(&cookies))
                .await?;
            let valid_pages: Vec<_> = info.pages.iter().filter(|item| item.cid > 0).collect();
            // 超过 100P 的视频显式告警并打标，避免静默截断被当成完整结果。
            let pages_truncated = valid_pages.len() > 100;
            if pages_truncated {
                warn!(
                    "[弹幕] {bvid} 共 {} 个有效分P，仅处理前 100 个",
                    valid_pages.len()
                );
            }
            let pages = valid_pages.iter().take(100).copied().collect::<Vec<_>>();
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
                    "success": success && !pages_truncated,
                    "partial": !success || pages_truncated,
                    "pages_truncated": pages_truncated,
                    "message": if pages_truncated {
                        format!("分P超过 100 个，仅下载前 100 P（共 {} P）", valid_pages.len())
                    } else if success {
                        "所有分P弹幕下载完成".to_string()
                    } else {
                        "部分分P弹幕下载失败".to_string()
                    },
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
}
