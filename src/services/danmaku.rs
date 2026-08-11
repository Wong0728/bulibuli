//! 弹幕/评论下载服务：BV 号解码与 cookie 工具在本文件，
//! protobuf 弹幕下载见 `fetch`，评论抓取见 `comments`，评论 HTML 渲染见 `comment_html`。

mod archive;
mod comment_html;
mod comments;
mod fetch;

use crate::config::AppPaths;
use crate::services::bili_api::BiliApi;
use crate::services::cookie_manager::CookieManager;
use crate::services::file_safety::{ensure_existing_within_root, validate_uid};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) use archive::SidecarArchivePolicy;
// 归档工具供字幕等其他 sidecar 服务复用（family 名不同，逻辑一致）。
pub(crate) use archive::archive_sidecar_files;

const XOR_CODE: i64 = 23442827791579;
const MASK_CODE: i64 = 2251799813685247;
const ALPHABET: &str = "FcwAPNKTMug3GV5Lj7EJnHpWsx4tb8haYeviqBz6rkCy12mUSDQX9RdoZf";
/// 解码时使用 ENCODE_MAP 的逆序（与 Python DECODE_MAP = tuple(reversed(ENCODE_MAP)) 一致）
const DECODE_MAP: [usize; 9] = [6, 4, 2, 3, 1, 5, 0, 7, 8];

/// 单段弹幕时长（秒），与 Bili23 一致：6 分钟一包。
const SEGMENT_DURATION: i64 = 360;

fn bv2av(bvid: &str) -> Result<i64> {
    if !bvid.starts_with("BV1") {
        return Err(anyhow!("BV号格式错误"));
    }
    let bvid = &bvid[3..];
    let chars: Vec<char> = bvid.chars().collect();
    let base = ALPHABET.len() as i64;
    let mut tmp = 0i64;
    for &idx in &DECODE_MAP {
        let ch = chars
            .get(idx)
            .copied()
            .ok_or_else(|| anyhow!("BV号长度不足"))?;
        let idx = ALPHABET
            .find(ch)
            .ok_or_else(|| anyhow!("BV号包含非法字符"))? as i64;
        tmp = tmp * base + idx;
    }
    Ok((tmp & MASK_CODE) ^ XOR_CODE)
}

fn cookie_map(cookies: &str) -> HashMap<String, String> {
    cookies
        .split(';')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let mut parts = s.splitn(2, '=');
            let k = parts.next()?.trim().to_string();
            let v = parts.next().unwrap_or("").trim().to_string();
            Some((k, v))
        })
        .collect()
}

fn cookie_header(cookies: &HashMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Clone)]
pub struct DanmakuService {
    paths: Arc<AppPaths>,
    bili_api: Arc<BiliApi>,
    cookie_manager: Arc<CookieManager>,
}

impl DanmakuService {
    pub fn new(
        paths: Arc<AppPaths>,
        bili_api: Arc<BiliApi>,
        cookie_manager: Arc<CookieManager>,
    ) -> Self {
        Self {
            paths,
            bili_api,
            cookie_manager,
        }
    }

    async fn save_dir(
        &self,
        uid: Option<&str>,
        override_dir: Option<&std::path::Path>,
    ) -> Result<PathBuf> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::dm_proto;
    use prost::Message;

    #[test]
    fn test_bv2av() {
        let av = bv2av("BV1L9Uoa9EUx").unwrap();
        assert_eq!(av, 111298867365120);
    }

    #[test]
    fn test_parse_protobuf_segment() {
        // 构造一个含 2 条弹幕的分段
        let e1 = dm_proto::DanmakuElem {
            id: 1,
            progress: 1000,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            mid_hash: "h1".to_string(),
            text: "hello".to_string(),
            ctime: 1700000000,
            weight: 0,
            action: String::new(),
            pool: 0,
            id_str: "id1".to_string(),
        };
        let e2 = dm_proto::DanmakuElem {
            id: 2,
            progress: 2000,
            mode: 4,
            fontsize: 36,
            color: 0,
            mid_hash: "h2".to_string(),
            text: "bottom".to_string(),
            ctime: 1700000010,
            weight: 0,
            action: String::new(),
            pool: 0,
            id_str: "id2".to_string(),
        };
        let seg = dm_proto::DanmakuSeg {
            elems: vec![e1, e2],
        };
        let mut buf = Vec::new();
        seg.encode(&mut buf).expect("encode");

        let list = dm_proto::parse_danmaku_bytes(&buf);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["text"].as_str().unwrap(), "hello");
        assert_eq!(list[0]["time"].as_f64().unwrap(), 1.0);
        assert_eq!(list[1]["type"].as_i64().unwrap(), 4);
    }

    #[test]
    fn test_cookie_map_and_header() {
        let m = cookie_map("a=1; b=2=3; c=");
        assert_eq!(m.get("a").unwrap(), "1");
        assert_eq!(m.get("b").unwrap(), "2=3");
        let h = cookie_header(&m);
        assert!(h.contains("a=1"));
        assert!(h.contains("b=2=3"));
    }

    #[test]
    fn danmaku_segment_count_is_bounded() {
        assert_eq!(fetch::capped_segment_count(0), (1, false));
        assert_eq!(
            fetch::capped_segment_count(SEGMENT_DURATION * 512),
            (512, false)
        );
        assert_eq!(
            fetch::capped_segment_count(SEGMENT_DURATION * 513),
            (512, true)
        );
    }
}
