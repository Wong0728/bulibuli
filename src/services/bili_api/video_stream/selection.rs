//! 视频流选择算法：画质优先、编码偏好（av1/hevc/avc）、最低画质下限，
//! 只允许向下回退。与网络/CDN 无关的纯函数，独立可测试。

use crate::services::bili_api::models::playurl::StreamQuality;

/// 在可用视频流中执行稳定、可测试的“画质优先、编码偏好、最低画质”选择。
/// 只允许向下回退；所有候选都低于最低画质时返回 None。
pub(crate) fn choose_video_stream(
    streams: &[StreamQuality],
    desired_quality: i32,
    minimum_quality: i32,
    codec_preferences: &[String],
    allow_fallback: bool,
) -> Option<StreamQuality> {
    let mut candidates: Vec<StreamQuality> = streams
        .iter()
        .filter(|stream| {
            stream.quality >= minimum_quality
                && (allow_fallback || stream.quality == desired_quality)
                && stream.quality <= desired_quality
        })
        .cloned()
        .collect();
    candidates.sort_by(|left, right| {
        let quality = right.quality.cmp(&left.quality);
        if quality != std::cmp::Ordering::Equal {
            return quality;
        }
        let rank = |stream: &StreamQuality| {
            let actual = stream.codec.as_deref().unwrap_or_default();
            codec_preferences
                .iter()
                .position(|codec| match codec.as_str() {
                    "av1" => actual.contains("av1") || actual.contains("av01"),
                    other => actual.contains(other),
                })
                .unwrap_or(usize::MAX)
        };
        rank(left).cmp(&rank(right))
    });
    candidates.into_iter().next()
}

#[cfg(test)]
mod stream_selection_tests {
    use super::choose_video_stream;
    use crate::services::bili_api::models::playurl::StreamQuality;

    fn stream(quality: i32, codec: &str) -> StreamQuality {
        StreamQuality {
            quality,
            codec: Some(codec.to_string()),
            ..StreamQuality::default()
        }
    }

    #[test]
    fn prefers_requested_quality_then_preferred_codec() {
        let streams = vec![stream(80, "avc1"), stream(80, "av01"), stream(64, "av01")];
        let selected = choose_video_stream(&streams, 80, 64, &["av1".into(), "avc".into()], true)
            .expect("stream");
        assert_eq!(selected.codec.as_deref(), Some("av01"));
    }

    #[test]
    fn refuses_fallback_below_minimum() {
        let streams = vec![stream(64, "avc1")];
        assert!(choose_video_stream(&streams, 80, 80, &["avc".into()], true).is_none());
    }
}
