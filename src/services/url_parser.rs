use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::sync::LazyLock;

static BV_RE: LazyLock<regex::Regex> =
    // SAFETY: the expression is a compile-time literal covered by parser tests.
    LazyLock::new(|| regex::Regex::new(r"(?i)(BV[0-9A-Za-z]{10})").expect("static BV regex"));
static AV_RE: LazyLock<regex::Regex> =
    // SAFETY: the expression is a compile-time literal covered by parser tests.
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)av([0-9]+)").expect("static AV regex"));
static EP_RE: LazyLock<regex::Regex> =
    // SAFETY: the expression is a compile-time literal covered by parser tests.
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)ep([0-9]+)").expect("static EP regex"));
static SS_RE: LazyLock<regex::Regex> =
    // SAFETY: the expression is a compile-time literal covered by parser tests.
    LazyLock::new(|| regex::Regex::new(r"(?i)(?:^|/|\\b)ss([0-9]+)").expect("static SS regex"));
static FP_RE: LazyLock<regex::Regex> =
    // SAFETY: the expression is a compile-time literal covered by parser tests.
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
            let response = client
                .get(url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await?;
            return parse_media_input(response.url().as_str());
        }
    }
    parse_media_input(trimmed)
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
}
