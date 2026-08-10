//! 评论 HTML 渲染：自包含页面（内嵌样式 + 结构化 JSON 数据）。

use chrono::DateTime;
use serde_json::Value;

use super::DanmakuService;

/// 评论 HTML 的内嵌样式（简洁，贴近下载助手配色）。
const COMMENT_HTML_STYLE: &str = r#"<style>
:root { --brand:#00a1d6; --text:#18191c; --muted:#9499a0; --border:#e3e5e7; --bg:#f6f7f8; }
* { box-sizing:border-box; }
body { margin:0; padding:20px; background:var(--bg); color:var(--text); font-family:-apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif; font-size:14px; line-height:1.6; }
.wrap { max-width:820px; margin:0 auto; }
.page-head { padding:16px 20px; background:#fff; border:1px solid var(--border); border-radius:12px; margin-bottom:16px; }
.page-head h1 { margin:0 0 6px; font-size:18px; }
.page-head .sub { color:var(--muted); font-size:13px; }
.cmt { padding:14px 16px; background:#fff; border:1px solid var(--border); border-radius:12px; margin-bottom:12px; }
.cmt-head { display:flex; flex-wrap:wrap; align-items:center; gap:8px; margin-bottom:8px; }
.cmt-idx { color:var(--muted); font-size:12px; }
.cmt-user { font-weight:600; color:var(--brand); }
.cmt-lv { font-size:11px; color:var(--muted); border:1px solid var(--border); border-radius:4px; padding:0 5px; }
.cmt-meta { color:var(--muted); font-size:12px; margin-left:auto; }
.cmt-body { white-space:pre-wrap; word-break:break-word; }
.cmt-replies { margin-top:10px; padding-left:12px; border-left:2px solid var(--border); }
.cmt-replies-title { color:var(--muted); font-size:12px; margin-bottom:6px; }
.reply { padding:6px 0; border-top:1px dashed var(--border); }
.reply:first-of-type { border-top:none; }
.reply-head { display:flex; flex-wrap:wrap; align-items:center; gap:6px; margin-bottom:2px; }
.reply-body { white-space:pre-wrap; word-break:break-word; color:#61666d; }
</style>
"#;

impl DanmakuService {
    /// HTML 转义，避免用户内容破坏页面结构。
    fn html_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }

    /// 生成自包含评论 HTML：可直接浏览器打开，样式贴近下载助手；
    /// 文末内嵌 <script id="cmt-data"> 结构化 JSON，供前端/程序解析渲染。
    pub(super) fn format_comments_html(&self, bvid: &str, avid: i64, comments: &[Value]) -> String {
        let fmt_time = |ts: i64| -> String {
            DateTime::from_timestamp(ts, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default()
        };

        let mut html = String::new();
        html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
        html.push_str(&format!(
            "<title>评论 · {}</title>\n",
            Self::html_escape(bvid)
        ));
        html.push_str(COMMENT_HTML_STYLE);
        html.push_str("</head>\n<body>\n<div class=\"wrap\">\n");
        html.push_str(&format!(
            "<div class=\"page-head\"><h1>视频评论</h1><div class=\"sub\">{} · av{} · 共 {} 条主评论</div></div>\n",
            Self::html_escape(bvid), avid, comments.len()
        ));

        for (i, c) in comments.iter().enumerate() {
            let uname = Self::html_escape(c["uname"].as_str().unwrap_or(""));
            let level = c["level"].as_i64().unwrap_or(0);
            let like = c["like"].as_i64().unwrap_or(0);
            let rcount = c["total_replies"].as_i64().unwrap_or(0);
            let time = fmt_time(c["ctime"].as_i64().unwrap_or(0));
            let message = Self::html_escape(c["message"].as_str().unwrap_or(""));
            html.push_str(&format!(
                "<div class=\"cmt\"><div class=\"cmt-head\"><span class=\"cmt-idx\">#{}</span><span class=\"cmt-user\">{}</span><span class=\"cmt-lv\">Lv{}</span><span class=\"cmt-meta\">👍 {} · 💬 {} · {}</span></div><div class=\"cmt-body\">{}</div>",
                i + 1, uname, level, like, rcount, time, message
            ));
            if let Some(replies) = c["replies"].as_array() {
                if !replies.is_empty() {
                    html.push_str(&format!(
                        "<div class=\"cmt-replies\"><div class=\"cmt-replies-title\">回复（显示 {}/{} 条）</div>",
                        replies.len(), rcount
                    ));
                    for r in replies {
                        let runame = Self::html_escape(r["uname"].as_str().unwrap_or(""));
                        let rlevel = r["level"].as_i64().unwrap_or(0);
                        let rlike = r["like"].as_i64().unwrap_or(0);
                        let rtime = fmt_time(r["ctime"].as_i64().unwrap_or(0));
                        let rmsg = Self::html_escape(r["message"].as_str().unwrap_or(""));
                        html.push_str(&format!(
                            "<div class=\"reply\"><div class=\"reply-head\"><span class=\"cmt-user\">{}</span><span class=\"cmt-lv\">Lv{}</span><span class=\"cmt-meta\">👍 {} · {}</span></div><div class=\"reply-body\">{}</div></div>",
                            runame, rlevel, rlike, rtime, rmsg
                        ));
                    }
                    html.push_str("</div>");
                }
            }
            html.push_str("</div>\n");
        }

        let data_json = serde_json::to_string(comments)
            .unwrap_or_else(|_| "[]".to_string())
            .replace("</", "<\\/");
        html.push_str("</div>\n<script type=\"application/json\" id=\"cmt-data\">");
        html.push_str(&data_json);
        html.push_str("</script>\n</body>\n</html>");
        html
    }
}
