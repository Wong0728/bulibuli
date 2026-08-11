use crate::error::{AppError, AppResult};
use reqwest::header::LOCATION;
use serde::Serialize;
use std::sync::LazyLock;

static BV_RE: LazyLock<regex::Regex> =
    // SAFETY: 表达式是编译期常量，并由解析器测试覆盖。
    LazyLock::new(|| regex::Regex::new(r"(?i)(BV[0-9A-Za-z]{10})").expect("static BV regex"));
static AV_RE: LazyLock<regex::Regex> =
    // SAFETY: 表达式是编译期常量，并由解析器测试覆盖。
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)av([0-9]+)").expect("static AV regex"));
static EP_RE: LazyLock<regex::Regex> =
    // SAFETY: 表达式是编译期常量，并由解析器测试覆盖。
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)ep([0-9]+)").expect("static EP regex"));
static SS_RE: LazyLock<regex::Regex> =
    // SAFETY: 表达式是编译期常量，并由解析器测试覆盖。
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)ss([0-9]+)").expect("static SS regex"));
static FP_RE: LazyLock<regex::Regex> =
    // SAFETY: 表达式是编译期常量，并由解析器测试覆盖。
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)fp([0-9]+)").expect("static FP regex"));

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum ResolvedMedia {
    VideoBv(String),
    VideoAv(u64),
    Episode(u64),
    Season(u64),
    Course(u64),
}

pub fn parse_media_input(input: &str) -> AppResult<ResolvedMedia> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AppError::BadRequest("链接或视频编号不能为空".to_string()));
    }
    if let Some(captures) = BV_RE.captures(input) {
        return Ok(ResolvedMedia::VideoBv(captures[1].to_string()));
    }
    for (regex, constructor) in [
        (&*AV_RE, ResolvedMedia::VideoAv as fn(u64) -> ResolvedMedia),
        (&*EP_RE, ResolvedMedia::Episode),
        (&*SS_RE, ResolvedMedia::Season),
        (&*FP_RE, ResolvedMedia::Course),
    ] {
        if let Some(captures) = regex.captures(input) {
            let id = captures[1]
                .parse::<u64>()
                .map_err(|_| AppError::BadRequest("媒体编号超出范围".to_string()))?;
            return Ok(constructor(id));
        }
    }
    Err(AppError::BadRequest(
        "不支持的B站链接，支持 BV/AV/ep/ss/fp/b23.tv".to_string(),
    ))
}

pub async fn resolve_media_input(
    client: &reqwest::Client,
    input: &str,
) -> AppResult<ResolvedMedia> {
    let trimmed = input.trim();
    if let Ok(url) = url::Url::parse(trimmed) {
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if host == "b23.tv" || host.ends_with(".b23.tv") {
            let mut current = url;
            for hop in 0..=3 {
                if !is_allowed_b23_redirect_host(&current) {
                    return Err(AppError::BadRequest(
                        "B23 短链重定向到不受信任的主机".to_string(),
                    ));
                }
                let response = client
                    .get(current.clone())
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await?;
                if !response.status().is_redirection() {
                    let final_url = response.url();
                    if !is_final_bilibili_url(final_url) {
                        return Err(AppError::BadRequest(
                            "B23 短链最终地址必须是 HTTPS B 站地址".to_string(),
                        ));
                    }
                    return parse_media_input(final_url.as_str());
                }
                if hop == 3 {
                    return Err(AppError::BadRequest(
                        "B23 短链重定向次数超过 3 次".to_string(),
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        AppError::BadRequest("B23 短链缺少有效的重定向地址".to_string())
                    })?;
                current = response
                    .url()
                    .join(location)
                    .map_err(|_| AppError::BadRequest("B23 短链重定向地址无效".to_string()))?;
            }
        }
    }
    parse_media_input(trimmed)
}

fn is_allowed_b23_redirect_host(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url
            .host_str()
            .map(|host| {
                let host = host.to_ascii_lowercase();
                host == "b23.tv"
                    || host.ends_with(".b23.tv")
                    || host == "bilibili.com"
                    || host.ends_with(".bilibili.com")
            })
            .unwrap_or(false)
}

fn is_final_bilibili_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .map(|host| {
                let host = host.to_ascii_lowercase();
                host == "bilibili.com" || host.ends_with(".bilibili.com")
            })
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_media_ids() {
        assert_eq!(
            parse_media_input("https://www.bilibili.com/video/BV1xx411c7mD").expect("BV input"),
            ResolvedMedia::VideoBv("BV1xx411c7mD".to_string())
        );
        assert_eq!(
            parse_media_input("av170001").expect("AV input"),
            ResolvedMedia::VideoAv(170001)
        );
        assert_eq!(
            parse_media_input("https://www.bilibili.com/bangumi/play/ep123").expect("EP input"),
            ResolvedMedia::Episode(123)
        );
        assert_eq!(
            parse_media_input("ss456").expect("SS input"),
            ResolvedMedia::Season(456)
        );
        assert_eq!(
            parse_media_input("fp789").expect("FP input"),
            ResolvedMedia::Course(789)
        );
    }

    #[test]
    fn b23_redirect_policy_only_allows_bilibili_hosts() {
        assert!(is_allowed_b23_redirect_host(
            &url::Url::parse("https://b23.tv/BV1xx411c7mD").expect("b23")
        ));
        assert!(is_final_bilibili_url(
            &url::Url::parse("https://www.bilibili.com/video/BV1xx411c7mD").expect("bilibili")
        ));
        assert!(!is_final_bilibili_url(
            &url::Url::parse("http://www.bilibili.com/video/BV1xx411c7mD").expect("http")
        ));
        assert!(!is_allowed_b23_redirect_host(
            &url::Url::parse("https://evil.example/BV1xx411c7mD").expect("evil")
        ));
    }
}
