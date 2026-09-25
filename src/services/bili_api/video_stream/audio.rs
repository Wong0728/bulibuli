//! 音频下载流：DASH 音轨（m4a 按码率降序）、杜比全景声与 Hi-Res 无损，
//! 按调用方偏好（"m4a"/"dolby"/"flac"）选择音轨，未命中回退最高码率 m4a。

use anyhow::{anyhow, Result};

use crate::services::bili_api::models::playurl::{AudioQuality, AudioStreams};
use crate::services::bili_api::BiliApi;

impl BiliApi {
    /// 获取音频下载流。`cid = Some(..)` 时直接复用，省一次 get_video_info 调用。
    /// 视频无音频流时返回 `Ok(None)`（调用方按“未找到音频流”处理）。
    /// `preference` 控制音轨偏好："m4a"（默认最高码率）/ "dolby"（杜比全景声优先）/
    /// "flac"（Hi-Res 无损优先）。命中时 ext 切换为 ec3/flac，未命中回退 m4a。
    pub async fn get_audio_url(
        &self,
        bvid: &str,
        cid: Option<i64>,
        cookies: &str,
        preference: &str,
    ) -> Result<Option<AudioStreams>> {
        let cid = match cid {
            Some(c) => c,
            None => {
                let info = self.get_video_info(bvid, cookies).await?;
                if info.cid <= 0 {
                    return Err(anyhow!("未找到视频 cid"));
                }
                info.cid
            }
        };

        let data = self.request_playurl(bvid, cid, cookies, 127, 4048).await?;
        let dash = match data.dash {
            Some(d) => d,
            None => return Ok(None),
        };

        // 收集常规 m4a 音频流（按码率降序）
        let mut qualities: Vec<AudioQuality> = Vec::new();
        for stream in dash.audio.as_deref().unwrap_or(&[]) {
            let urls = stream.collect_urls();
            if urls.is_empty() {
                continue;
            }
            let selected_url = self
                .bad_cdns
                .choose_url(&urls)
                .await
                .unwrap_or(&urls[0])
                .to_string();
            qualities.push(AudioQuality {
                id: stream.id,
                bandwidth: stream.bandwidth,
                url: selected_url,
            });
        }
        qualities.sort_by_key(|q| std::cmp::Reverse(q.bandwidth));

        // 杜比全景声：dash.dolby.audio[]，无权限/无 fnval 位时整段为 null
        let mut dolby_url: Option<String> = None;
        if let Some(dolby) = dash.dolby.as_ref() {
            for stream in dolby.audio.as_deref().unwrap_or_default() {
                let urls = stream.collect_urls();
                if urls.is_empty() {
                    continue;
                }
                dolby_url = Some(
                    self.bad_cdns
                        .choose_url(&urls)
                        .await
                        .unwrap_or(&urls[0])
                        .to_string(),
                );
                break;
            }
        }

        // Hi-Res 无损：dash.flac.audio（单个对象，可能为 null）
        let mut flac_url: Option<String> = None;
        if let Some(flac) = dash.flac.as_ref() {
            if let Some(stream) = flac.audio.as_ref() {
                let urls = stream.collect_urls();
                if !urls.is_empty() {
                    flac_url = Some(
                        self.bad_cdns
                            .choose_url(&urls)
                            .await
                            .unwrap_or(&urls[0])
                            .to_string(),
                    );
                }
            }
        }

        // 按偏好选择：命中 → 用对应轨 + 切换 ext；未命中 → 回退最高码率 m4a（保持现状）
        let (selected_url, ext) = match preference {
            "flac" if flac_url.is_some() => (flac_url.unwrap(), "flac".to_string()),
            "dolby" if dolby_url.is_some() => (dolby_url.unwrap(), "ec3".to_string()),
            "flac" if dolby_url.is_some() => (dolby_url.unwrap(), "ec3".to_string()),
            _ => {
                if qualities.is_empty() {
                    return Ok(None);
                }
                (qualities[0].url.clone(), "m4a".to_string())
            }
        };

        if qualities.is_empty() && selected_url.is_empty() {
            return Ok(None);
        }

        Ok(Some(AudioStreams {
            audio_url: selected_url,
            qualities,
            ext,
        }))
    }
}
