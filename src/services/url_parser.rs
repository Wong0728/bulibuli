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

/// B 站官方域名白名单：b23/短链解析的最终结果 URL 必须落在这些域名（或其子域）上。
/// 该校验与 TLS 校验开关解耦：即使匿名客户端按配置关闭了证书校验
/// （`tls_verify=false`，存在 MITM 风险），解析结果域名不在白名单也一律拒绝，
/// 防止被污染的跳转目标混入后续业务流程。
fn is_bilibili_trusted_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    ["bilibili.com", "bilibili.tv", "b23.tv", "hdslb.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn is_allowed_b23_redirect_host(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url
            .host_str()
            .map(is_bilibili_trusted_host)
            .unwrap_or(false)
}

fn is_final_bilibili_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .map(is_bilibili_trusted_host)
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

    #[test]
    fn final_url_whitelist_covers_official_domains_only() {
        // 白名单与 TLS 开关解耦：仅 B 站官方域名（含子域）可通过。
        for ok in [
            "https://www.bilibili.com/video/BV1xx411c7mD",
            "https://b23.tv/BV1xx411c7mD",
            "https://www.bilibili.tv/video/BV1xx411c7mD",
            "https://i0.hdslb.com/bfs/face.png",
        ] {
            assert!(
                is_final_bilibili_url(&url::Url::parse(ok).expect("ok")),
                "应放行: {ok}"
            );
        }
        for bad in [
            "https://evil.example/BV1xx411c7mD",
            "https://bilibili.com.evil.example/BV1xx411c7mD",
            "http://www.bilibili.com/video/BV1xx411c7mD",
        ] {
            assert!(
                !is_final_bilibili_url(&url::Url::parse(bad).expect("bad")),
                "应拒绝: {bad}"
            );
        }
    }

    mod proptest_suite {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// 任意输入（含乱码、超长、控制字符）解析不得 panic；
            /// 接受的结果必须落在五种已知变体之一。
            #[test]
            fn parse_media_input_never_panics_and_result_is_bounded(input in ".*") {
                if let Ok(resolved) = parse_media_input(&input) {
                    prop_assert!(matches!(
                        resolved,
                        ResolvedMedia::VideoBv(_)
                            | ResolvedMedia::VideoAv(_)
                            | ResolvedMedia::Episode(_)
                            | ResolvedMedia::Season(_)
                            | ResolvedMedia::Course(_)
                    ));
                }
            }

            /// 白名单属性：无论 host 如何构造，最终 URL 校验要么拒绝，
            /// 要么放行的域名必然是 B 站官方域名（或其子域）。
            #[test]
            fn final_url_accepts_only_whitelisted_hosts(
                scheme in proptest::sample::select(vec!["http".to_string(), "https".to_string()]),
                host in "[a-zA-Z0-9.\\-]{0,64}",
            ) {
                let candidate = format!("{scheme}://{host}/video/BV1xx411c7mD");
                if let Ok(url) = url::Url::parse(&candidate) {
                    if is_final_bilibili_url(&url) {
                        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
                        let whitelisted = ["bilibili.com", "bilibili.tv", "b23.tv", "hdslb.com"]
                            .iter()
                            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")));
                        prop_assert!(whitelisted, "非白名单域名被放行: {host}");
                        prop_assert_eq!(url.scheme(), "https");
                    }
                }
            }
        }
    }
}
