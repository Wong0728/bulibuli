//! 已解析弹幕列表的序列化输出：B 站传统 XML、带元信息头部的 TXT 两种格式。

use chrono::Local;
use serde_json::Value;

use super::DanmakuService;

impl DanmakuService {
    /// 将解析后的弹幕列表重新序列化为 B 站传统 XML 格式。
    pub(super) fn serialize_xml(&self, bvid: &str, cid: i64, list: &[Value]) -> String {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<i>\n");
        s.push_str(&format!(
            "<!-- bvid={bvid} cid={cid} count={} generated={} -->\n",
            list.len(),
            Local::now().format("%Y-%m-%d %H:%M:%S")
        ));
        for dm in list {
            let time = dm["time"].as_f64().unwrap_or(0.0);
            let mode = dm["type"].as_i64().unwrap_or(1);
            let size = dm["size"].as_i64().unwrap_or(25);
            let color = dm["color"].as_i64().unwrap_or(16777215);
            let ts = dm["timestamp"].as_i64().unwrap_or(0);
            let pool = dm["pool"].as_i64().unwrap_or(0);
            let hash = dm["hash"].as_str().unwrap_or("");
            let dmid = dm["dmid"].as_str().unwrap_or("");
            let text = dm["text"].as_str().unwrap_or("");
            let escaped = text
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;");
            s.push_str(&format!(
                "<d p=\"{time},{mode},{size},{color},{ts},{pool},{hash},{dmid}\">{escaped}</d>\n"
            ));
        }
        s.push_str("</i>");
        s
    }

    pub(super) fn format_danmaku_txt(&self, bvid: &str, cid: i64, list: &[Value]) -> String {
        let mut lines = vec![
            format!("视频: {bvid}"),
            format!("CID: {cid}"),
            format!("弹幕总数: {}", list.len()),
            format!("下载时间: {}", Local::now().format("%Y-%m-%d %H:%M:%S")),
            "=".repeat(80),
            "格式说明: [视频内时间] 类型 字号 颜色 发送时间 | 弹幕内容".to_string(),
            "-".repeat(80),
        ];
        for dm in list {
            let t = dm["time"].as_f64().unwrap_or(0.0);
            lines.push(format!(
                "[{:>8}] {:>2} {:>2} #{:06X} {} | {}",
                self.format_video_time(t),
                self.type_name(dm["type"].as_i64().unwrap_or(1)),
                self.size_name(dm["size"].as_i64().unwrap_or(25)),
                dm["color"].as_i64().unwrap_or(0),
                crate::services::time_format::format_unix_local(
                    dm["timestamp"].as_i64().unwrap_or(0),
                    "%Y-%m-%d %H:%M:%S",
                )
                .unwrap_or_default(),
                dm["text"].as_str().unwrap_or("")
            ));
        }
        lines.join("\n")
    }

    fn format_video_time(&self, seconds: f64) -> String {
        let h = (seconds / 3600.0) as i64;
        let m = ((seconds % 3600.0) / 60.0) as i64;
        let s = (seconds % 60.0) as i64;
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m:02}:{s:02}")
        }
    }

    fn type_name(&self, t: i64) -> &'static str {
        match t {
            1..=3 => "普通",
            4 => "底部",
            5 => "顶部",
            6 => "逆向",
            7 => "高级",
            8 => "代码",
            9 => "BAS",
            _ => "未知",
        }
    }

    fn size_name(&self, s: i64) -> &'static str {
        match s {
            18 => "小",
            25 => "标准",
            36 => "大",
            _ => "",
        }
    }
}
