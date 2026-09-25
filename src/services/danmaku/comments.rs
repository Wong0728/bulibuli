//! 评论下载编排：游标分页拉取主评论、正则过滤与落盘归档。
//! 评论 API 请求与二级回复展开见 `comment_replies`。

use crate::services::wbi;
use anyhow::{anyhow, Result};
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tracing::{error, info, warn};

use super::archive::archive_sidecar_files;
use super::{bv2av, cookie_header, cookie_map, DanmakuService, SidecarArchivePolicy};

impl DanmakuService {
    /// 下载评论，支持指定保存目录
    #[allow(clippy::too_many_arguments)]
    pub async fn download_comments_to(
        &self,
        bvid: &str,
        cookies_str: Option<&str>,
        uid: Option<&str>,
        main_limit: usize,
        reply_mode: &str,
        filter_regex: &str,
        archive_policy: SidecarArchivePolicy,
        save_dir_override: Option<&std::path::Path>,
    ) -> Result<Value> {
        info!("[评论] 开始下载: bvid={bvid}, uid={:?}, main_limit={main_limit}, reply_mode={reply_mode}", uid);
        let cookies = cookies_str.map(cookie_map).unwrap_or_default();

        let avid = match bv2av(bvid) {
            Ok(v) => {
                info!("[评论] BV→AV 转换成功: {bvid} → av{v}");
                v
            }
            Err(e) => {
                error!("[评论] BV→AV 转换失败 {bvid}: {e}");
                return Ok(json!({
                    "success": false,
                    "message": format!("BV号转换失败: {e}"),
                }));
            }
        };

        let comments = match self
            .fetch_comments(avid, bvid, &cookies, main_limit, reply_mode)
            .await
        {
            Ok(c) => {
                info!("[评论] 拉取成功: bvid={bvid}, 共 {} 条", c.len());
                self.apply_comment_filter(c, filter_regex)
            }
            Err(e) => {
                error!("[评论] 拉取失败: bvid={bvid}, {e}");
                return Ok(json!({
                    "success": false,
                    "message": format!("获取评论失败: {e}"),
                }));
            }
        };

        if comments.is_empty() {
            info!("[评论] 该视频暂无评论: bvid={bvid}");
            return Ok(json!({
                "success": true,
                "message": "该视频暂无评论",
                "count": 0,
                "file_path": null,
            }));
        }

        let save_dir = self.save_dir(uid, save_dir_override).await?;
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            error!("[评论] 创建目录失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("创建目录失败: {e}"),
            }));
        }

        let output = save_dir.join(format!("{bvid}_comments.html"));
        if let Err(e) =
            tokio::fs::write(&output, self.format_comments_html(bvid, avid, &comments)).await
        {
            error!("[评论] 写入评论文件失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("写入评论文件失败: {e}"),
            }));
        }
        if let Err(e) = archive_sidecar_files(
            &save_dir,
            bvid,
            "comments",
            std::slice::from_ref(&output),
            archive_policy,
        )
        .await
        {
            error!("[评论] 归档失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("归档评论文件失败: {e}"),
            }));
        }

        info!(
            "[评论] 下载完成: bvid={bvid}, 共 {} 条主评论",
            comments.len()
        );
        Ok(json!({
            "success": true,
            "message": format!("评论下载完成，共 {} 条主评论", comments.len()),
            "count": comments.len(),
        }))
    }

    async fn fetch_comments(
        &self,
        avid: i64,
        bvid: &str,
        cookies: &HashMap<String, String>,
        main_limit: usize,
        reply_mode: &str,
    ) -> Result<Vec<Value>> {
        // 先合并设备指纹，再获取 WBI keys：nav 接口在新风控下要求携带登录态 Cookie
        let enriched = match self.cookie_manager.enrich(&cookie_header(cookies)).await {
            Ok(value) => value,
            Err(error) => {
                // 无设备指纹的冷设备极易命中 -352，降级必须留痕，
                // 否则前端只看到一个没有原因的 partial。
                warn!("合并设备指纹失败，降级为无指纹 Cookie 继续拉取: {error}");
                cookie_header(cookies)
            }
        };
        let (img_key, sub_key) = self.bili_api.get_wbi_keys_public(&enriched).await?;
        let referer = format!("https://www.bilibili.com/video/{bvid}");

        let mut all = Vec::new();
        let reply_budget = Arc::new(AtomicUsize::new(5_000));
        // 游标分页：首页 offset 为空字符串
        let mut next_offset = String::new();
        while all.len() < main_limit {
            let pagination_str = json!({"offset": next_offset}).to_string();
            let mut params = HashMap::new();
            params.insert("type".to_string(), "1".to_string());
            params.insert("oid".to_string(), avid.to_string());
            params.insert("mode".to_string(), "3".to_string());
            params.insert("ps".to_string(), "20".to_string());
            params.insert("pagination_str".to_string(), pagination_str);
            params.insert("web_location".to_string(), "143".to_string());
            if let Err(e) = wbi::enc_wbi(&mut params, &img_key, &sub_key) {
                return Err(anyhow!("评论请求 WBI 签名失败: {e}"));
            }

            // 带重试的请求：风控返回 HTML 时退避重试（最多 3 次）
            let data = match self
                .fetch_comment_page_with_retry(
                    "https://api.bilibili.com/x/v2/reply/wbi/main",
                    &params,
                    &referer,
                    &enriched,
                    3,
                )
                .await
            {
                Ok(d) => d,
                Err(e) => return Err(e),
            };

            let replies = data["data"]["replies"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if replies.is_empty() {
                break;
            }
            let page_limit = main_limit.saturating_sub(all.len());
            let page_comments = stream::iter(replies.into_iter().take(page_limit))
                .map(|reply| {
                    let referer = referer.clone();
                    let enriched = enriched.clone();
                    let img_key = img_key.clone();
                    let sub_key = sub_key.clone();
                    let reply_budget = reply_budget.clone();
                    async move {
                        let mut main = self.parse_reply(&reply);
                        let root_rpid = main["rpid"].as_i64().unwrap_or(0);
                        let mut embedded: Vec<Value> = reply["replies"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .map(|item| self.parse_reply(item))
                            .collect();
                        embedded.sort_by(|a, b| b["like"].as_i64().cmp(&a["like"].as_i64()));
                        let sub_replies = if reply_mode == "all" && root_rpid > 0 {
                            match self
                                .fetch_all_replies(
                                    avid,
                                    root_rpid,
                                    &referer,
                                    &enriched,
                                    (&img_key, &sub_key),
                                    reply_budget,
                                )
                                .await
                            {
                                Ok(items) if !items.is_empty() => items,
                                _ => embedded,
                            }
                        } else {
                            embedded.into_iter().take(3).collect()
                        };
                        main["replies"] = json!(sub_replies);
                        main["total_replies"] = reply["rcount"].clone();
                        main
                    }
                })
                .buffered(3)
                .collect::<Vec<_>>()
                .await;
            all.extend(page_comments);

            // 游标分页：从 cursor.next 获取下一页偏移量
            let cursor = &data["data"]["cursor"];
            let is_end = cursor["is_end"].as_bool().unwrap_or(true);
            if is_end {
                break;
            }
            next_offset = cursor["next"].as_str().unwrap_or("").to_string();
            if next_offset.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(all.into_iter().take(main_limit).collect())
    }

    /// 用正则过滤评论：命中的主评论整条丢弃；命中的回复从该评论回复列表移除。
    /// 正则为空或无效时不过滤（无效时告警）。
    fn apply_comment_filter(&self, comments: Vec<Value>, filter_regex: &str) -> Vec<Value> {
        let pattern = filter_regex.trim();
        if pattern.is_empty() {
            return comments;
        }
        let re = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                warn!("[评论] 过滤正则无效，已忽略过滤: {e}");
                return comments;
            }
        };
        comments
            .into_iter()
            .filter_map(|mut c| {
                if re.is_match(c["message"].as_str().unwrap_or("")) {
                    return None;
                }
                if let Some(replies) = c["replies"].as_array() {
                    let kept: Vec<Value> = replies
                        .iter()
                        .filter(|r| !re.is_match(r["message"].as_str().unwrap_or("")))
                        .cloned()
                        .collect();
                    c["replies"] = json!(kept);
                }
                Some(c)
            })
            .collect()
    }
}
