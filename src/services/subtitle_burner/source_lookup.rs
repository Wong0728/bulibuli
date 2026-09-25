//! 素材定位：在视频所在目录及 `danmaku/`、`subtitle/` 子目录中，
//! 按固定候选路径与 bvid 前缀扫描查找弹幕/字幕文件。

use std::path::{Path, PathBuf};
use tokio::fs;

use super::SubtitleBurner;

impl SubtitleBurner {
    pub(super) async fn find_danmaku_file(&self, parent: &Path, stem: &str) -> Option<PathBuf> {
        let bvid = extract_bvid(stem)?;

        // 如果视频文件名包含 _弹幕版，还原原始 stem（BV 号不变，但用于同名匹配）
        let original_stem = if stem.contains("_弹幕版") {
            stem.replace("_弹幕版", "")
        } else {
            stem.to_string()
        };

        let danmaku_dir = parent.join("danmaku");
        let candidates = vec![
            danmaku_dir.join(format!("{bvid}_danmaku.xml")),
            danmaku_dir.join(format!("{bvid}_danmaku.json")),
            danmaku_dir.join(format!("{bvid}.xml")),
            danmaku_dir.join(format!("{bvid}.json")),
            parent.join(format!("{original_stem}_danmaku.xml")),
            parent.join(format!("{original_stem}_danmaku.json")),
            parent.join(format!("{original_stem}.xml")),
            parent.join(format!("{original_stem}.json")),
            parent.join(format!("{bvid}_danmaku.xml")),
            parent.join(format!("{bvid}_danmaku.json")),
            parent.join(format!("{bvid}.xml")),
            parent.join(format!("{bvid}.json")),
        ];

        if let Some(p) = Self::find_first_existing(&candidates).await {
            return Some(p);
        }

        // 兜底：扫描 parent 及其 danmaku 子目录中名称包含 bvid 的 xml/json
        for dir in [parent, &danmaku_dir] {
            let mut entries = match fs::read_dir(dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_lowercase());
                if matches!(ext.as_deref(), Some("xml") | Some("json")) && name.contains(&bvid) {
                    return Some(path);
                }
            }
        }

        None
    }

    async fn find_first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
        for p in paths {
            if p.exists() {
                return Some(p.clone());
            }
        }
        None
    }

    /// 查找字幕文件：优先固定路径（{stem}.ass/.srt），兜底扫描 subtitle/ 目录下 {bvid}*.srt/.ass。
    /// 兜底匹配使用 bvid 前缀 + 边界字符（`_` / `.`），避免前缀误匹配。
    pub(super) async fn find_subtitle_file(parent: &Path, stem: &str) -> Option<PathBuf> {
        let subtitle_dir = parent.join("subtitle");
        let candidates = [
            subtitle_dir.join(format!("{stem}.ass")),
            subtitle_dir.join(format!("{stem}.srt")),
            parent.join(format!("{stem}.ass")),
            parent.join(format!("{stem}.srt")),
        ];
        if let Some(p) = Self::find_first_existing(&candidates).await {
            return Some(p);
        }
        // 兜底：扫描 subtitle/ 目录下以 {bvid} 开头、扩展名为 .srt 或 .ass 的文件
        let bvid = extract_bvid(stem)?;
        let mut entries = match fs::read_dir(&subtitle_dir).await {
            Ok(e) => e,
            Err(_) => return None,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase());
            if matches!(ext.as_deref(), Some("srt") | Some("ass")) && name.starts_with(&bvid) {
                // 边界检查：bvid 后必须紧跟 `_` 或 `.`（如 {bvid}_zh-CN.srt / {bvid}.srt）
                let rest = &name[bvid.len()..];
                if rest.is_empty() || rest.starts_with('_') || rest.starts_with('.') {
                    return Some(path);
                }
            }
        }
        None
    }
}

fn extract_bvid(text: &str) -> Option<String> {
    use regex::Regex;
    let re = Regex::new(r"BV[0-9a-zA-Z]+").ok()?;
    re.find(text).map(|m| m.as_str().to_string())
}
