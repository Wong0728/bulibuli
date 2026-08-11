//! 评论抓取：游标分页拉取主评论、风控重试、二级回复展开与正则过滤。

use crate::services::wbi;
use anyhow::{anyhow, Result};
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        let enriched = self
            .cookie_manager
            .enrich(&cookie_header(cookies))
            .await
            .unwrap_or_else(|_| cookie_header(cookies));
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

    /// 发送单页评论请求，遇到风控 HTML 响应时退避重试。
    async fn fetch_comment_page_with_retry(
        &self,
        url: &str,
        params: &HashMap<String, String>,
        referer: &str,
        cookies: &str,
        max_retries: u32,
    ) -> Result<Value> {
        let mut last_err = None;
        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = std::time::Duration::from_secs((attempt * 2) as u64);
                warn!(
                    "[评论] 风控重试 {}/{}，等待 {:?}...",
                    attempt, max_retries, delay
                );
                tokio::time::sleep(delay).await;
            }

            let resp = match self
                .bili_api
                .send_get_public(url, params, referer, cookies)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(anyhow!("评论请求发送失败: {e}"));
                    continue;
                }
            };

            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    last_err = Some(anyhow!("评论响应读取失败: {e}"));
                    continue;
                }
            };

            // 检测风控：返回 HTML 而非 JSON
            if !content_type.contains("application/json") {
                let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
                warn!(
                    "[评论] B站返回非 JSON 响应 (Content-Type: {content_type})，可能触发风控。前 200 字节: {preview}"
                );
                last_err = Some(anyhow!(
                    "B站返回非 JSON 响应 (Content-Type: {content_type})，可能触发风控"
                ));
                continue;
            }

            let data: Value = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
                    last_err = Some(anyhow!("评论 JSON 解析失败: {e}。前 200 字节: {preview}"));
                    continue;
                }
            };

            let code = data["code"].as_i64().unwrap_or(-1);
            if code == -352 || code == -412 {
                // -352: 风控校验失败; -412: 请求被拦截
                warn!(
                    "[评论] B站返回风控错误码: code={}, message={}",
                    code,
                    data["message"].as_str().unwrap_or("")
                );
                last_err = Some(anyhow!(
                    "评论请求被风控拦截: code={} message={}",
                    code,
                    data["message"].as_str().unwrap_or("")
                ));
                continue;
            }
            if code != 0 {
                return Err(anyhow!(
                    "获取评论失败: {}",
                    data["message"].as_str().unwrap_or("")
                ));
            }

            return Ok(data);
        }
        Err(last_err.unwrap_or_else(|| anyhow!("评论请求重试耗尽")))
    }

    /// 展开某条主评论的全部二级回复（分页调用 reply/reply）。
    /// 仅在“全部回复”模式下使用；请求量随回复数增长，风控风险更高，故加节流与安全上限。
    async fn fetch_all_replies(
        &self,
        avid: i64,
        root_rpid: i64,
        referer: &str,
        cookies: &str,
        wbi_keys: (&str, &str),
        remaining_budget: Arc<AtomicUsize>,
    ) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut pn = 1u32;
        'pages: loop {
            let mut params = HashMap::new();
            params.insert("type".to_string(), "1".to_string());
            params.insert("oid".to_string(), avid.to_string());
            params.insert("root".to_string(), root_rpid.to_string());
            params.insert("ps".to_string(), "20".to_string());
            params.insert("pn".to_string(), pn.to_string());
            if let Err(e) = wbi::enc_wbi(&mut params, wbi_keys.0, wbi_keys.1) {
                return Err(anyhow!("回复展开 WBI 签名失败: {e}"));
            }
            let data = self
                .fetch_comment_page_with_retry(
                    "https://api.bilibili.com/x/v2/reply/reply",
                    &params,
                    referer,
                    cookies,
                    2,
                )
                .await?;
            let replies = data["data"]["replies"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if replies.is_empty() {
                break;
            }
            for r in &replies {
                if remaining_budget
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_err()
                {
                    warn!("[评论] 单任务二级回复达到 5000 条安全上限");
                    break 'pages;
                }
                all.push(self.parse_reply(r));
            }
            let total = data["data"]["page"]["count"].as_i64().unwrap_or(0);
            if replies.len() < 20 || (all.len() as i64) >= total {
                break;
            }
            pn += 1;
            if pn > 50 {
                warn!("[评论] 回复展开达到安全页数上限 (root={root_rpid})");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
        Ok(all)
    }

    fn parse_reply(&self, reply: &Value) -> Value {
        let member = &reply["member"];
        let content = &reply["content"];
        let vip_status = member["vip"]["status"]
            .as_i64()
            .or_else(|| member["vip_status"].as_i64())
            .unwrap_or(0);
        let vip_label = member["vip"]["label"]["text"]
            .as_str()
            .or_else(|| member["vip_label"]["text"].as_str())
            .or_else(|| member["vip_label"].as_str())
            .unwrap_or("");
        let name_color = member["vip"]["nickname_color"]
            .as_str()
            .or_else(|| member["nameplate"]["nickname_color"].as_str())
            .unwrap_or("");
        json!({
            "rpid": reply["rpid"],
            "mid": member["mid"],
            "uname": member["uname"].as_str().unwrap_or(""),
            "level": member["level_info"]["current_level"].as_i64().unwrap_or(0),
            "vip_status": vip_status,
            "vip_label": vip_label,
            "name_color": name_color,
            "message": content["message"].as_str().unwrap_or(""),
            "like": reply["like"].as_i64().unwrap_or(0),
            "ctime": reply["ctime"].as_i64().unwrap_or(0),
        })
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
