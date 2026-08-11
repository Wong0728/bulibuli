//! CC 字幕下载服务：拉取 B 站 player/wbi/v2 字幕列表，下载 JSON 字幕体并转换为 SRT 落盘。
//!
//! 结构参照 `danmaku` 模块：服务持有 `AppPaths` / `BiliApi` / `CookieManager`，
//! 通过 `download_subtitles_to` 完成单视频字幕下载，落盘到 `subtitle/` 子目录，
//! 并复用 `archive_sidecar_files` 实现与弹幕一致的归档策略。

use crate::config::AppPaths;
use crate::services::bili_api::models::SubtitleInfo;
use crate::services::bili_api::BiliApi;
use crate::services::danmaku::{archive_sidecar_files, SidecarArchivePolicy};
use crate::services::file_safety::{ensure_existing_within_root, validate_uid};
use crate::services::settings::SubtitleSettings;
use anyhow::{anyhow, Result};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, LOCATION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

/// B 站字幕 JSON 体结构：`{ body: [{ from, to, content }, ...] }`。
/// 仅声明用到的字段，`from`/`to` 为秒（浮点），`content` 为字幕文本。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct SubtitleBody {
    body: Vec<SubtitleLine>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct SubtitleLine {
    from: f64,
    to: f64,
    content: String,
}

#[derive(Clone)]
pub struct SubtitleFetchService {
    paths: Arc<AppPaths>,
    bili_api: Arc<BiliApi>,
}

/// `download_subtitles_to` 的参数集合：将原本 7 个参数收敛为一个结构体，
/// 既符合 clippy::too_many_arguments 限制，也便于调用方按字段名传参。
pub struct SubtitleDownloadRequest<'a> {
    pub bvid: &'a str,
    pub cid: i64,
    pub cookies: Option<&'a str>,
    pub uid: Option<&'a str>,
    pub archive_policy: SidecarArchivePolicy,
    pub settings: &'a SubtitleSettings,
    pub save_dir_override: Option<&'a Path>,
}

impl SubtitleFetchService {
    pub fn new(paths: Arc<AppPaths>, bili_api: Arc<BiliApi>) -> Self {
        Self { paths, bili_api }
    }

    async fn save_dir(&self, uid: Option<&str>, override_dir: Option<&Path>) -> Result<PathBuf> {
        let directory = match override_dir {
            Some(path) => path.to_path_buf(),
            None => match uid {
                Some(raw) => self.paths.download_dir.join(validate_uid(raw)?.as_str()),
                None => self.paths.download_dir.clone(),
            },
        };
        ensure_existing_within_root(&self.paths.download_dir, &directory)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(directory)
    }

    /// 下载视频 CC 字幕并落盘为 SRT sidecar。
    ///
    /// 流程：
    /// 1. 调 `bili_api.get_subtitles` 拉取字幕列表
    /// 2. 按 `settings.languages` / `settings.accept_ai` 过滤
    /// 3. 逐条下载 JSON 字幕体并转 SRT
    /// 4. 落盘到 `save_dir/subtitle/{bvid}_{lan}.srt`，主语言额外保存 `{bvid}.srt`
    /// 5. 使用 `archive_sidecar_files` 归档（family="subtitle"）
    ///
    /// 无字幕时返回 `success=true, count=0`（静默跳过，不报错）。
    pub async fn download_subtitles_to(&self, req: SubtitleDownloadRequest<'_>) -> Result<Value> {
        let SubtitleDownloadRequest {
            bvid,
            cid,
            cookies,
            uid,
            archive_policy,
            settings,
            save_dir_override,
        } = req;
        info!(
            "[字幕] 开始下载: bvid={bvid}, cid={cid}, uid={:?}, accept_ai={}, languages={:?}",
            uid, settings.accept_ai, settings.languages
        );
        let cookies = cookies.unwrap_or("");

        // 1. 拉取字幕列表
        let subtitles = match self.bili_api.get_subtitles(bvid, cid, cookies).await {
            Ok(list) => {
                info!("[字幕] 拉取到 {} 条字幕: bvid={bvid}", list.len());
                list
            }
            Err(e) => {
                error!("[字幕] 拉取字幕列表失败 {bvid}: {e}");
                return Ok(json!({
                    "success": false,
                    "message": format!("拉取字幕列表失败: {e}"),
                }));
            }
        };

        // 2. 过滤：languages 非空时只保留精确匹配；accept_ai=false 时跳过 ai- 开头
        let filtered: Vec<SubtitleInfo> = subtitles
            .into_iter()
            .filter(|s| {
                // AI 字幕过滤
                if !settings.accept_ai && s.lan.starts_with("ai-") {
                    return false;
                }
                // 语言过滤：空数组表示下载全部
                if !settings.languages.is_empty() {
                    return settings.languages.iter().any(|l| l == &s.lan);
                }
                true
            })
            .collect();

        if filtered.is_empty() {
            info!("[字幕] 过滤后无字幕可下载: bvid={bvid}");
            return Ok(json!({
                "success": true,
                "message": "无字幕（或已被语言/AI 过滤）",
                "count": 0,
                "files": [],
            }));
        }

        // 3. 确定保存目录
        let save_dir = self.save_dir(uid, save_dir_override).await?;
        let subtitle_dir = save_dir.join("subtitle");
        if let Err(e) = tokio::fs::create_dir_all(&subtitle_dir).await {
            error!("[字幕] 创建字幕目录失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("创建字幕目录失败: {e}"),
            }));
        }

        // 4. 逐条下载并转 SRT 落盘
        let mut saved_files: Vec<PathBuf> = Vec::new();
        let mut primary_path: Option<PathBuf> = None;
        for (idx, sub) in filtered.iter().enumerate() {
            let srt_content = match self.fetch_and_convert_srt(&sub.subtitle_url, cookies).await {
                Ok(content) => content,
                Err(e) => {
                    warn!("[字幕] 下载/转换失败 {bvid} lan={}: {e}", sub.lan);
                    continue;
                }
            };

            let file_path = subtitle_dir.join(format!("{bvid}_{}.srt", sub.lan));
            if let Err(e) = tokio::fs::write(&file_path, srt_content.as_bytes()).await {
                error!("[字幕] 写入 SRT 失败 {bvid} lan={}: {e}", sub.lan);
                continue;
            }
            info!(
                "[字幕] 已保存: bvid={bvid}, lan={}, lan_doc={}, path={}",
                sub.lan,
                sub.lan_doc,
                file_path.display()
            );
            saved_files.push(file_path.clone());

            // 第一个匹配的语言作为主语言副本（覆盖式），供 burn_subtitle 精确查找
            if idx == 0 {
                let primary = subtitle_dir.join(format!("{bvid}.srt"));
                if let Err(e) = tokio::fs::copy(&file_path, &primary).await {
                    warn!("[字幕] 复制主语言副本失败 {bvid}: {e}");
                } else {
                    primary_path = Some(primary);
                }
            }
        }

        if saved_files.is_empty() {
            warn!("[字幕] 所有字幕下载/转换均失败: bvid={bvid}");
            return Ok(json!({
                "success": false,
                "message": "所有字幕下载/转换均失败",
            }));
        }

        // 5. 归档：主语言副本 + 各语言文件，family="subtitle"
        let mut archive_paths = saved_files.clone();
        if let Some(ref primary) = primary_path {
            archive_paths.push(primary.clone());
        }
        if let Err(e) = archive_sidecar_files(
            &subtitle_dir,
            bvid,
            "subtitle",
            &archive_paths,
            archive_policy,
        )
        .await
        {
            error!("[字幕] 归档失败 {bvid}: {e}");
            return Ok(json!({
                "success": false,
                "message": format!("归档字幕文件失败: {e}"),
            }));
        }

        // 6. 额外复制主语言 SRT 到视频旁边（{bvid}.srt），供 PotPlayer 自动挂载
        if let Some(ref primary) = primary_path {
            let player_path = save_dir.join(format!("{bvid}.srt"));
            match tokio::fs::copy(primary, &player_path).await {
                Ok(_) => info!("[字幕] 已复制到视频旁边: {}", player_path.display()),
                Err(e) => warn!("[字幕] 复制到视频旁边失败 {}: {e}", player_path.display()),
            }
        }

        let file_names: Vec<String> = saved_files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        info!(
            "[字幕] 下载完成: bvid={bvid}, 共 {} 条字幕",
            saved_files.len()
        );
        Ok(json!({
            "success": true,
            "message": format!("字幕下载完成，共 {} 条", saved_files.len()),
            "count": saved_files.len(),
            "files": file_names,
        }))
    }

    /// 下载字幕 JSON 并转换为 SRT 文本。
    /// `subtitle_url` 可能为 `//` 开头的协议相对路径，自动补 `https:`。
    async fn fetch_and_convert_srt(&self, subtitle_url: &str, cookies: &str) -> Result<String> {
        let url = fix_subtitle_url(subtitle_url);
        if url.is_empty() {
            return Err(anyhow!("字幕 URL 为空"));
        }

        const MAX_SUBTITLE_BYTES: usize = 2 * 1024 * 1024;
        let mut current = crate::services::bili_url_policy::validate(&url)
            .await
            .map_err(|e| anyhow!("字幕 URL 不受支持: {e}"))?;
        let mut headers = HeaderMap::new();
        headers.insert("User-Agent", HeaderValue::from_static("Mozilla/5.0"));
        headers.insert(
            "Referer",
            HeaderValue::from_static("https://www.bilibili.com/"),
        );
        if !cookies.trim().is_empty() {
            let enriched = self.bili_api.enrich_cookies_public(cookies).await?;
            headers.insert("Cookie", HeaderValue::from_str(&enriched)?);
        }
        let mut redirect_count = 0u8;
        let resp = loop {
            let response = self
                .bili_api
                .client_for(current.as_str())
                .get(current.as_str())
                .headers(headers.clone())
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map_err(|e| anyhow!("请求字幕 JSON 失败 url={current}: {e}"))?;
            if response.status().is_redirection() {
                if redirect_count >= 5 {
                    return Err(anyhow!("字幕重定向超过 5 次限制"));
                }
                redirect_count += 1;
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| anyhow!("字幕重定向缺少 Location"))?;
                current = current
                    .join(location)
                    .map_err(|_| anyhow!("字幕重定向 URL 无效"))?;
                if !current.path().is_empty() {
                    current = crate::services::bili_url_policy::validate(current.as_str())
                        .await
                        .map_err(|e| anyhow!("字幕重定向 URL 不受支持: {e}"))?;
                }
                continue;
            }
            break response;
        };
        if !resp.status().is_success() {
            return Err(anyhow!(
                "字幕 JSON 返回 HTTP {} url={current}",
                resp.status()
            ));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.is_empty() && !content_type.contains("json") {
            return Err(anyhow!("字幕响应不是 JSON: {content_type}"));
        }
        let mut bytes = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > MAX_SUBTITLE_BYTES {
                return Err(anyhow!("字幕 JSON 超过 2 MiB 限制"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: SubtitleBody = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow!("解析字幕 JSON 失败 url={current}: {e}"))?;

        Ok(convert_to_srt(&body.body))
    }
}

/// 将 `//` 开头的协议相对路径补全为 `https:` 开头的绝对 URL。
fn fix_subtitle_url(url: &str) -> String {
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
}

/// 将秒（浮点）格式化为 SRT 时间戳 `HH:MM:SS,mmm`（毫秒用逗号）。
fn format_srt_time(seconds: f64) -> String {
    if seconds < 0.0 {
        return "00:00:00,000".to_string();
    }
    let total_ms = (seconds * 1000.0).round() as i64;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// 将字幕行列表转换为 SRT 格式字符串。
fn convert_to_srt(lines: &[SubtitleLine]) -> String {
    let mut output = String::new();
    for (idx, line) in lines.iter().enumerate() {
        let index = idx + 1;
        let from = format_srt_time(line.from);
        let to = format_srt_time(line.to);
        output.push_str(&format!("{index}\n"));
        output.push_str(&format!("{from} --> {to}\n"));
        output.push_str(line.content.trim());
        output.push_str("\n\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_protocol_relative_url() {
        assert_eq!(
            fix_subtitle_url("//i0.hdslb.com/sub.json"),
            "https://i0.hdslb.com/sub.json"
        );
        assert_eq!(
            fix_subtitle_url("https://i0.hdslb.com/sub.json"),
            "https://i0.hdslb.com/sub.json"
        );
        assert_eq!(fix_subtitle_url(""), "");
    }

    #[test]
    fn formats_srt_time_correctly() {
        assert_eq!(format_srt_time(0.0), "00:00:00,000");
        assert_eq!(format_srt_time(1.5), "00:00:01,500");
        assert_eq!(format_srt_time(63.456), "00:01:03,456");
        assert_eq!(format_srt_time(3661.789), "01:01:01,789");
        assert_eq!(format_srt_time(-1.0), "00:00:00,000");
    }

    #[test]
    fn converts_subtitle_body_to_srt() {
        let lines = vec![
            SubtitleLine {
                from: 1.0,
                to: 3.5,
                content: "你好世界".to_string(),
            },
            SubtitleLine {
                from: 4.0,
                to: 6.5,
                content: "第二行".to_string(),
            },
        ];
        let srt = convert_to_srt(&lines);
        assert!(srt.contains("1\n00:00:01,000 --> 00:00:03,500\n你好世界\n\n"));
        assert!(srt.contains("2\n00:00:04,000 --> 00:00:06,500\n第二行\n\n"));
    }

    #[test]
    fn deserializes_subtitle_body() {
        let json = r#"{"body":[{"from":1.0,"to":3.5,"content":"测试"},{"from":4.0,"to":6.5,"content":"第二行"}]}"#;
        let body: SubtitleBody = serde_json::from_str(json).expect("parse");
        assert_eq!(body.body.len(), 2);
        assert_eq!(body.body[0].from, 1.0);
        assert_eq!(body.body[0].content, "测试");
    }
}
