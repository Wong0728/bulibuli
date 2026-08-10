//! 视频窗口截取与标题相似度：供 `blogger_check` 的自动下载窗口与重投检测使用。

use crate::services::bili_api::models::user::UserVideo;

/// 自动下载窗口截取：从新到旧遍历，凑满 `limit` 个非充电视频即停止。
/// `skip_charge` 开启时，充电视频（is_charging_arc）不占名额但保留在结果里，
/// 由 gate_download 落 pay_blocked 记录；关闭时直接取前 limit 个。
pub(super) fn select_video_window(
    all: Vec<UserVideo>,
    limit: usize,
    skip_charge: bool,
) -> Vec<UserVideo> {
    if !skip_charge {
        return all.into_iter().take(limit).collect();
    }
    let mut selected = Vec::new();
    let mut normal = 0usize;
    for v in all {
        if v.is_charging_arc {
            selected.push(v);
        } else {
            if normal >= limit {
                break;
            }
            selected.push(v);
            normal += 1;
        }
    }
    selected
}

/// 标题相似度（Jaccard on character bigrams）。返回 [0, 1]。
/// 用于"重投检测"——当新发现 bvid 的标题与该博主最近 90 天 history 的标题相似度 ≥ 0.8 时，
/// 标记为新 bvid 的 `reupload_of` 指向老 bvid（纯提示，不自动重下）。
pub(super) fn title_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let sa = char_bigrams(a);
    let sb = char_bigrams(b);
    if sa.is_empty() || sb.is_empty() {
        // 退化：直接按字符完全相同判定
        return if a == b { 1.0 } else { 0.0 };
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 把字符串拆成字符级 bigram 集合（用于 Jaccard 相似度）。
fn char_bigrams(s: &str) -> std::collections::HashSet<(char, char)> {
    let chars: Vec<char> = s.chars().collect();
    chars.windows(2).map(|w| (w[0], w[1])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_title_similarity_identical() {
        assert!((title_similarity("hello", "hello") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_title_similarity_similar() {
        // 重投常见场景：标题几乎一致，仅末尾标点不同
        let sim = title_similarity("【测试】视频标题第一集", "【测试】视频标题第一集!");
        assert!(sim >= 0.8, "sim = {sim}");
    }

    #[test]
    fn test_title_similarity_different() {
        let sim = title_similarity("hello", "world");
        assert!(sim < 0.3, "sim = {sim}");
    }

    #[test]
    fn test_select_video_window_skip_charge() {
        let mk = |bvid: &str, charge: bool| UserVideo {
            bvid: bvid.to_string(),
            is_charging_arc: charge,
            ..UserVideo::default()
        };
        // 最新列表里夹着 1 个充电视频：应取 3 个非充电 + 途中的充电视频（供落记录），后续不再取
        let all = vec![
            mk("a", false),
            mk("b", true),
            mk("c", false),
            mk("d", false),
            mk("e", false),
        ];
        let sel = select_video_window(all, 3, true);
        let bvids: Vec<&str> = sel.iter().map(|v| v.bvid.as_str()).collect();
        assert_eq!(bvids, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_select_video_window_no_skip() {
        let mk = |bvid: &str, charge: bool| UserVideo {
            bvid: bvid.to_string(),
            is_charging_arc: charge,
            ..UserVideo::default()
        };
        // 不跳过充电视频时，它们同样占用窗口名额。
        let all = vec![
            mk("a", false),
            mk("b", true),
            mk("c", false),
            mk("d", false),
        ];
        let sel = select_video_window(all, 3, false);
        let bvids: Vec<&str> = sel.iter().map(|v| v.bvid.as_str()).collect();
        assert_eq!(bvids, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_select_video_window_all_charge() {
        let mk = |bvid: &str, charge: bool| UserVideo {
            bvid: bvid.to_string(),
            is_charging_arc: charge,
            ..UserVideo::default()
        };
        // 全是充电视频：全部保留（都不占名额，逐个落 pay_blocked 记录）
        let all = vec![mk("a", true), mk("b", true)];
        let sel = select_video_window(all, 3, true);
        assert_eq!(sel.len(), 2);
    }
}
