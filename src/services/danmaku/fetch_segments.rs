//! protobuf 弹幕分段拉取：视频元信息获取、WBI 签名分段请求、
//! 响应大小限制与解码统计（成功 / 失败分段）。

use crate::services::dm_proto;
use crate::services::wbi;
use anyhow::{anyhow, Result};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, warn};

use super::{cookie_header, DanmakuService, SEGMENT_DURATION};

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
    pub(super) async fn get_video_meta(
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
    pub(super) async fn fetch_protobuf_segments(
        &self,
        bvid: &str,
        cid: i64,
        duration: i64,
        cookies: &HashMap<String, String>,
    ) -> Result<(Vec<Value>, usize, usize, Vec<usize>, bool)> {
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
                        let status = resp.status().as_u16();
                        warn!("弹幕分段 {index} HTTP {status} 失败");
                        self.bili_api
                            .notify_http_status("danmaku_seg", status)
                            .await;
                        continue;
                    }
                    match read_limited_response(resp, MAX_DANMAKU_SEGMENT_BYTES).await {
                        Ok(bytes) => {
                            if bytes.is_empty() {
                                // B 站对无弹幕分段通常返回 200 + 空 body：
                                // 视为成功空段，否则稀疏弹幕视频会恒报 partial 并被
                                // 计划任务反复重试。
                                debug!("弹幕分段 {index} 返回空数据，按无弹幕处理");
                                successful_segments += 1;
                                failed_segments.retain(|failed| *failed != index);
                                continue;
                            }
                            match dm_proto::try_parse_danmaku_bytes(&bytes) {
                                Some(danmaku) => {
                                    list.extend(danmaku);
                                    successful_segments += 1;
                                    failed_segments.retain(|failed| *failed != index);
                                }
                                // 解码失败≠空段：计入失败分段，避免协议变更被
                                // 误报为「暂无弹幕」并跳过重试。
                                None => warn!("弹幕分段 {index} protobuf 解码失败，计入失败分段"),
                            }
                        }
                        Err(e) => warn!("弹幕分段 {index} 读取字节失败: {e}"),
                    }
                }
                Err(e) => warn!("弹幕分段 {index} 请求失败: {e}"),
            }
        }
        Ok((list, successful_segments, parts, failed_segments, truncated))
    }
}
