//! 本地指纹生成器与 cookie 合并逻辑（纯计算，无网络/DB 依赖）。

use std::collections::HashMap;

use chrono::Local;
use rand::Rng;

use super::{CookieManager, DeviceCookies};

impl CookieManager {
    /// 合并设备 cookie + 用户 cookie，返回 cookie 字符串。
    /// 设备 cookie 提供基础设备指纹，用户 cookie 提供登录态。
    /// 同名字段时用户 cookie 优先（如 SESSDATA 等登录态字段不应被覆盖）。
    pub(super) fn merge_cookies(device: &DeviceCookies, user_cookies: &str) -> String {
        let mut map: HashMap<String, String> = HashMap::new();
        // 1. 设备 cookie
        map.insert("_uuid".to_string(), device.uuid.clone());
        map.insert("b_lsid".to_string(), device.b_lsid.clone());
        map.insert("b_nut".to_string(), device.b_nut.to_string());
        map.insert("bili_ticket".to_string(), device.bili_ticket.clone());
        map.insert(
            "bili_ticket_expires".to_string(),
            device.bili_ticket_expires.to_string(),
        );
        map.insert("buvid_fp".to_string(), device.buvid_fp.clone());
        map.insert("buvid3".to_string(), device.buvid3.clone());
        map.insert("buvid4".to_string(), device.buvid4.clone());
        map.insert("CURRENT_FNVAL".to_string(), "4048".to_string());
        map.insert("CURRENT_QUALITY".to_string(), "0".to_string());
        // 2. 用户 cookie 覆盖（登录态优先）
        for (k, v) in Self::parse_cookie_str(user_cookies) {
            map.insert(k, v);
        }
        // 3. 拼接为字符串
        let mut parts: Vec<String> = map.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        parts.sort();
        parts.join("; ")
    }

    /// 解析 cookie 字符串为 map。
    fn parse_cookie_str(s: &str) -> HashMap<String, String> {
        s.split(';')
            .filter_map(|part| {
                let part = part.trim();
                let eq = part.find('=')?;
                let k = part[..eq].trim().to_string();
                let v = part[eq + 1..].trim().to_string();
                if k.is_empty() {
                    None
                } else {
                    Some((k, v))
                }
            })
            .collect()
    }

    // ---------- 本地生成器 ----------

    /// 生成 _uuid：与 Bili23 get_uuid() 格式一致。
    /// 格式: `{8}-{4}-{4}-{4}-{12}{5位时间戳补0}infoc`
    /// 字符集: 123456789ABCDEF + "10"
    pub(super) fn gen_uuid() -> String {
        let mp: [&str; 16] = [
            "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F", "10",
        ];
        let mut rng = rand::rng();
        let mut gen_part = |len: usize| -> String {
            (0..len)
                .map(|_| mp[rng.random_range(0..mp.len())])
                .collect::<String>()
        };
        let t = Local::now().timestamp() % 100000;
        let ts_padded = format!("{:05}", t);
        // 分步生成，避免 format! 宏内多次可变借用
        let p1 = gen_part(8);
        let p2 = gen_part(4);
        let p3 = gen_part(4);
        let p4 = gen_part(4);
        let p5 = gen_part(12);
        format!("{p1}-{p2}-{p3}-{p4}-{p5}{ts_padded}infoc")
    }

    /// 生成 b_lsid：8 个随机十六进制字符（大写） + "_" + 时间戳十六进制（大写）。
    pub(super) fn gen_b_lsid() -> String {
        let mut rng = rand::rng();
        let head: String = (0..8)
            .map(|_| format!("{:X}", rng.random_range(0u8..16)))
            .collect();
        let ts = Local::now().timestamp();
        format!("{head}_{ts:X}")
    }

    /// 生成 buvid_fp：murmur3_x64_128(UA, seed=31) 的低64位和高64位 hex 拼接。
    /// 与 Bili23 get_buvid_fp() 行为一致。
    pub(super) fn gen_buvid_fp(ua: &str) -> String {
        // murmur3::murmur3_x64_128 接受 &mut T (T: Read)，返回 Result<u128>
        let mut data = ua.as_bytes();
        let m = murmur3::murmur3_x64_128(&mut data, 31).unwrap_or(0);
        let low = (m & 0xFFFF_FFFF_FFFF_FFFF) as u64;
        let high = (m >> 64) as u64;
        format!("{low:x}{high:x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buvid_fp_format() {
        // 仅验证输出格式：两个 hex 段拼接，不含 0x
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
        let fp = CookieManager::gen_buvid_fp(ua);
        assert!(!fp.is_empty());
        assert!(
            fp.chars().all(|c| c.is_ascii_hexdigit()),
            "buvid_fp 应为纯 hex: {fp}"
        );
    }

    #[test]
    fn test_buvid_fp_deterministic() {
        // 相同 UA 应产生相同 buvid_fp
        let ua = "Mozilla/5.0";
        assert_eq!(
            CookieManager::gen_buvid_fp(ua),
            CookieManager::gen_buvid_fp(ua)
        );
    }

    #[test]
    fn test_b_lsid_format() {
        let lsid = CookieManager::gen_b_lsid();
        assert!(lsid.contains('_'), "b_lsid 应包含下划线: {lsid}");
        let parts: Vec<&str> = lsid.split('_').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 8, "b_lsid 头部应为 8 个 hex 字符: {lsid}");
        // {:X} 产出大写 hex（0-9, A-F），校验字符集
        assert!(
            parts[0].chars().all(|c| "0123456789ABCDEF".contains(c)),
            "b_lsid 头部应为大写 hex: {lsid}"
        );
    }

    #[test]
    fn test_uuid_format() {
        let uuid = CookieManager::gen_uuid();
        assert!(uuid.ends_with("infoc"), "uuid 应以 infoc 结尾: {uuid}");
        // 应有 5 段（用 - 分隔的前 5 段 + 时间戳+infoc）
        let head = uuid.strip_suffix("infoc").unwrap_or(&uuid);
        let segments: Vec<&str> = head.split('-').collect();
        assert!(segments.len() >= 5, "uuid 应有至少 5 个 - 分隔段: {uuid}");
    }

    #[test]
    fn test_parse_cookie_str() {
        let map = CookieManager::parse_cookie_str("a=1; b=2; c=hello world");
        assert_eq!(map.get("a"), Some(&"1".to_string()));
        assert_eq!(map.get("b"), Some(&"2".to_string()));
        assert_eq!(map.get("c"), Some(&"hello world".to_string()));
    }

    #[test]
    fn test_merge_cookies_user_overrides_device() {
        let device = DeviceCookies {
            buvid3: "DEV_BUVID3".to_string(),
            buvid4: "DEV_BUVID4".to_string(),
            buvid_expires: 9999999999,
            bili_ticket: "DEV_TICKET".to_string(),
            bili_ticket_expires: 9999999999,
            uuid: "DEV_UUID".to_string(),
            b_lsid: "DEV_BLSID".to_string(),
            b_nut: 123,
            buvid_fp: "DEV_BUVID_FP".to_string(),
        };
        // 用户传入 buvid3，应覆盖设备的
        let merged = CookieManager::merge_cookies(&device, "buvid3=USER_OVERRIDE; SESSDATA=abc");
        assert!(merged.contains("buvid3=USER_OVERRIDE"));
        assert!(merged.contains("SESSDATA=abc"));
        assert!(!merged.contains("buvid3=DEV_BUVID3"));
        // 设备 cookie 应保留
        assert!(merged.contains("CURRENT_FNVAL=4048"));
        assert!(merged.contains("CURRENT_QUALITY=0"));
    }
}
