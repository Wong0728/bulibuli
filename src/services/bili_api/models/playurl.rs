//! playurl 流解析模型：B 站原始 DASH/durl 结构与对外流结构。

use serde::{Deserialize, Serialize};

/// `/x/player/wbi/playurl` 的 data（只声明用到的字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayurlData {
    pub dash: Option<Dash>,
    pub durl: Option<Vec<DurlItem>>,
    pub accept_quality: Vec<i64>,
    /// 实际返回的清晰度（durl 分支没有逐流的 id 字段，清晰度只在 data 顶层）。
    pub quality: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Dash {
    pub video: Vec<DashStream>,
    pub audio: Option<Vec<DashStream>>,
    /// 杜比全景声轨（需大会员 + fnval 含 1<<7；不满足时为 null/空）。
    pub dolby: Option<DolbyAudio>,
    /// Hi-Res 无损音轨（需大会员 + fnval 含 1<<11；不满足时为 null）。
    pub flac: Option<FlacAudio>,
}

/// `dash.dolby` 容器：内含 `audio[]` 数组（每项结构与普通 audio 一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DolbyAudio {
    /// B 站在无杜比权限或当前视频没有杜比音轨时会显式返回 `null`。
    pub audio: Option<Vec<DashStream>>,
}

/// `dash.flac` 容器：`audio` 为单个对象（非数组），无权限时整个 flac 为 null。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FlacAudio {
    pub audio: Option<DashStream>,
}

/// DASH 流（视频/音频共用）。
/// B 站会同时返回 camelCase 与 snake_case 两套 URL 字段，需分别声明后合并。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DashStream {
    pub id: i64,
    pub width: i64,
    pub height: i64,
    pub size: i64,
    pub bandwidth: i64,
    pub codecs: Option<String>,
    #[serde(rename = "baseUrl")]
    pub base_url_camel: Option<String>,
    pub base_url: Option<String>,
    #[serde(rename = "backupUrl")]
    pub backup_url_camel: Option<Vec<String>>,
    pub backup_url: Option<Vec<String>>,
    pub url: Option<String>,
}

impl Default for DashStream {
    fn default() -> Self {
        Self {
            id: 0,
            // 与旧实现的兜底值保持一致（width/height 缺失时按 720P 处理）
            width: 1280,
            height: 720,
            size: 0,
            bandwidth: 0,
            codecs: None,
            base_url_camel: None,
            base_url: None,
            backup_url_camel: None,
            backup_url: None,
            url: None,
        }
    }
}

impl DashStream {
    /// 收集候选 URL，顺序与 Bili23-Downloader 一致：
    /// baseUrl, base_url, backupUrl, backup_url, url。
    pub fn collect_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        for url in [&self.base_url_camel, &self.base_url].into_iter().flatten() {
            if !url.is_empty() {
                urls.push(url.clone());
            }
        }
        for list in [&self.backup_url_camel, &self.backup_url]
            .into_iter()
            .flatten()
        {
            urls.extend(list.iter().filter(|u| !u.is_empty()).cloned());
        }
        if let Some(url) = &self.url {
            if !url.is_empty() {
                urls.push(url.clone());
            }
        }
        urls
    }
}

/// FLV/MP4 直链分段（durl 分支）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DurlItem {
    pub size: i64,
    pub url: Option<String>,
    pub backup_url: Option<Vec<String>>,
}

// `DurlItem::collect_urls` 仅在下方 `#[cfg(test)]` 中调用，生产路径不遍历 durl 项。
// 把整个 impl 标为 test-only，避免 dead_code 警告同时不在 release 二进制里保留无用代码。
#[cfg(test)]
impl DurlItem {
    pub fn collect_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        if let Some(list) = &self.backup_url {
            urls.extend(list.iter().filter(|u| !u.is_empty()).cloned());
        }
        if let Some(url) = &self.url {
            if !url.is_empty() {
                // durl 分支主 URL 优先
                urls.insert(0, url.clone());
            }
        }
        urls
    }
}

/// 对外视频流集合（序列化字段名与前端契约一致）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct VideoStreams {
    pub cid: i64,
    pub qualities: Vec<StreamQuality>,
    pub selected_quality: Option<StreamQuality>,
    pub available_qualities: Vec<i64>,
    pub accept_quality: Vec<i64>,
}

/// 单条可下载视频流。
#[derive(Debug, Clone, Default, Serialize)]
pub struct StreamQuality {
    pub quality: i32,
    pub quality_name: String,
    pub width: i64,
    pub height: i64,
    pub url: String,
    pub urls: Vec<String>,
    pub size: i64,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

/// 对外音频流集合。
#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioStreams {
    pub audio_url: String,
    pub qualities: Vec<AudioQuality>,
    pub ext: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioQuality {
    pub id: i64,
    pub bandwidth: i64,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_stream_merges_camel_and_snake_urls_in_order() {
        // B 站真实响应同时携带 baseUrl 与 base_url，二者不能用 serde alias 合并
        let stream: DashStream = serde_json::from_str(
            r#"{
                "id": 80,
                "baseUrl": "https://cdn-a/main",
                "base_url": "https://cdn-a/main",
                "backupUrl": ["https://cdn-b/backup"],
                "backup_url": ["https://cdn-b/backup"],
                "codecs": "avc1.640032",
                "width": 1920,
                "height": 1080,
                "bandwidth": 800000
            }"#,
        )
        .expect("dash stream");
        assert_eq!(
            stream.collect_urls(),
            vec![
                "https://cdn-a/main",
                "https://cdn-a/main",
                "https://cdn-b/backup",
                "https://cdn-b/backup",
            ]
        );
    }

    #[test]
    fn playurl_data_tolerates_missing_fields() {
        let data: PlayurlData = serde_json::from_str(r#"{"accept_quality": [80, 64]}"#)
            .expect("playurl without dash/durl");
        assert!(data.dash.is_none());
        assert!(data.durl.is_none());
        assert_eq!(data.accept_quality, vec![80, 64]);

        let empty_stream: DashStream = serde_json::from_str("{}").expect("empty stream");
        assert_eq!(empty_stream.width, 1280);
        assert_eq!(empty_stream.height, 720);
        assert!(empty_stream.collect_urls().is_empty());
    }

    #[test]
    fn dolby_audio_accepts_null_missing_empty_and_streams() {
        for payload in [
            r#"{"dash":{"dolby":{"audio":null}}}"#,
            r#"{"dash":{"dolby":{}}}"#,
            r#"{"dash":{"dolby":{"audio":[]}}}"#,
        ] {
            let data: PlayurlData = serde_json::from_str(payload).expect("nullable dolby audio");
            let audio = data
                .dash
                .and_then(|dash| dash.dolby)
                .and_then(|dolby| dolby.audio)
                .unwrap_or_default();
            assert!(audio.is_empty());
        }

        let data: PlayurlData = serde_json::from_str(
            r#"{"dash":{"dolby":{"audio":[{"id":30250,"baseUrl":"https://cdn/dolby"}]}}}"#,
        )
        .expect("dolby stream array");
        let audio = data
            .dash
            .and_then(|dash| dash.dolby)
            .and_then(|dolby| dolby.audio)
            .expect("dolby audio array");
        assert_eq!(audio.len(), 1);
        assert_eq!(audio[0].id, 30250);
    }

    #[test]
    fn durl_item_prefers_main_url() {
        let item: DurlItem = serde_json::from_str(
            r#"{"url": "https://cdn/main.flv", "backup_url": ["https://cdn/bak.flv"], "size": 9}"#,
        )
        .expect("durl item");
        assert_eq!(
            item.collect_urls(),
            vec!["https://cdn/main.flv", "https://cdn/bak.flv"]
        );
    }
}
