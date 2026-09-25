//! 评论 API 请求层：带风控重试的单页请求、二级回复全量展开、
//! 回复字段解析与跨页去重判定。评论下载编排见 `comments`。

use crate::services::wbi;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::warn;

use super::DanmakuService;

impl DanmakuService {
    /// 发送单页评论请求，遇到风控 HTML 响应时退避重试。
    pub(super) async fn fetch_comment_page_with_retry(
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
                warn!("[评论] B站返回非 JSON 响应 (Content-Type: {content_type})，可能触发风控");
                last_err = Some(anyhow!(
                    "B站返回非 JSON 响应 (Content-Type: {content_type})，可能触发风控"
                ));
                continue;
            }

            let data: Value = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    last_err = Some(anyhow!("评论 JSON 解析失败: {e}"));
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
    pub(super) async fn fetch_all_replies(
        &self,
        avid: i64,
        root_rpid: i64,
        referer: &str,
        cookies: &str,
        wbi_keys: (&str, &str),
        remaining_budget: Arc<AtomicUsize>,
    ) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        // 翻页期间回复列表会漂移（新回复插入、排序重排），同一 rpid 可能在相邻两页
        // 都出现：不按 rpid 去重的话重复条目会直接进结果，还白占二级回复预算。
        let mut seen: HashSet<i64> = HashSet::new();
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
            let mut added = 0usize;
            for r in &replies {
                let parsed = self.parse_reply(r);
                if !accept_new_reply(&mut seen, &parsed) {
                    continue;
                }
                if remaining_budget
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_err()
                {
                    warn!("[评论] 单任务二级回复达到 5000 条安全上限");
                    break 'pages;
                }
                all.push(parsed);
                added += 1;
            }
            if added == 0 {
                // 整页都是已见过的 rpid：分页已停滞，继续翻只会重复消耗请求量
                warn!("[评论] 回复翻页返回整页重复条目，停止展开 (root={root_rpid}, pn={pn})");
                break;
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

    pub(super) fn parse_reply(&self, reply: &Value) -> Value {
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
}

/// 判断一条已解析回复是否首次出现，并把它的 rpid 记进 `seen`。
///
/// rpid 缺失或非法（0/负数/非数字）时无法判重，条目原样保留——宁可重复也不漏。
fn accept_new_reply(seen: &mut HashSet<i64>, parsed: &Value) -> bool {
    match parsed.get("rpid").and_then(Value::as_i64) {
        Some(rpid) if rpid > 0 => seen.insert(rpid),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::accept_new_reply;
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn accept_new_reply_drops_repeated_rpid_across_pages() {
        let mut seen = HashSet::new();
        assert!(accept_new_reply(&mut seen, &json!({"rpid": 11})));
        assert!(accept_new_reply(&mut seen, &json!({"rpid": 12})));
        assert!(
            !accept_new_reply(&mut seen, &json!({"rpid": 11})),
            "翻页重叠回来的同一条回复不能重复入结果"
        );
        assert!(accept_new_reply(&mut seen, &json!({"rpid": 13})));
    }

    #[test]
    fn accept_new_reply_keeps_entries_without_usable_rpid() {
        let mut seen = HashSet::new();
        assert!(accept_new_reply(&mut seen, &json!({})));
        assert!(accept_new_reply(&mut seen, &json!({"rpid": null})));
        assert!(accept_new_reply(&mut seen, &json!({"rpid": "abc"})));
        assert!(accept_new_reply(&mut seen, &json!({"rpid": 0})));
        assert!(seen.is_empty(), "无效 rpid 不应污染去重集合");
    }
}
