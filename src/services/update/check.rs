//! 版本清单拉取与启动检查：查询 GitHub Release 列表、解析 latest.json 资产清单，
//! 并提供启动后的按策略更新检查（off 不发任何请求）。

use super::{compare_versions, download_and_stage, matching_asset, staged_dir, REPO};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub sha256: String,
    pub platform: String,
    pub architecture: String,
    pub variant: String,
    pub download_url: String,
    pub checksum_name: String,
    pub checksum_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LatestManifest {
    version: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    draft: bool,
    assets: Vec<GithubAsset>,
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("bulibuli/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .context("创建更新检查 HTTP 客户端失败")
}

/// latest.json 中 `download_url` 的信任域白名单：
/// 仅允许 GitHub Release 资产域名，防止清单被篡改后把客户端引向任意下载源。
pub(super) fn validate_download_url(raw: &str) -> Result<()> {
    let host = url::Url::parse(raw)
        .ok()
        .filter(|parsed| parsed.scheme() == "https")
        .and_then(|parsed| parsed.host_str().map(|host| host.to_ascii_lowercase()));
    let allowed = matches!(
        host.as_deref(),
        Some("github.com")
            | Some("objects.githubusercontent.com")
            | Some("release-assets.githubusercontent.com")
    );
    if !allowed {
        bail!("更新清单 download_url 不在受信任的 GitHub Release 域内: {raw}");
    }
    Ok(())
}

/// 拉取最新版本号与资产清单：遍历 release 列表，取第一个含 latest.json 的
/// 非 draft release 并下载解析。
pub async fn fetch_latest(repo: &str) -> Result<(String, Vec<ReleaseAsset>)> {
    let client = http_client()?;
    let releases: Vec<GithubRelease> = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases?per_page=10"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("查询 GitHub Release 列表失败")?
        .error_for_status()
        .context("GitHub Release 列表请求失败")?
        .json()
        .await
        .context("解析 GitHub Release 列表失败")?;
    for release in releases.iter().filter(|release| !release.draft) {
        let Some(manifest_asset) = release
            .assets
            .iter()
            .find(|asset| asset.name == "latest.json")
        else {
            continue;
        };
        let manifest: LatestManifest = client
            .get(&manifest_asset.browser_download_url)
            .send()
            .await
            .context("下载 latest.json 失败")?
            .error_for_status()
            .context("latest.json 请求失败")?
            .json()
            .await
            .context("解析 latest.json 失败")?;
        return Ok((manifest.version, manifest.assets));
    }
    bail!("Release 列表中没有可用的 latest.json 资产")
}

/// 启动后按策略执行一次更新检查（off 不发任何请求）。
/// - manual：仅记录 latest_version（设置页显示"有新版本"）。
/// - auto：检测到新版本时自动下载校验暂存（不自动重启、不替换）。
///
/// 替换只发生在用户点"立即更新"（apply）时。
pub async fn startup_check(state: &crate::state::SharedState) -> Result<()> {
    let settings = state.infra.settings_service.current();
    if settings.update.policy == "off" {
        return Ok(());
    }
    let (latest, assets) = fetch_latest(REPO).await?;
    let has_update = compare_versions(&latest, env!("CARGO_PKG_VERSION")) == Ordering::Greater;
    let mut updated = (*settings).clone();
    updated.update.latest_version = Some(latest.clone());
    updated.update.last_checked_at = Some(chrono::Utc::now().timestamp());
    state.infra.settings_service.save(updated).await?;
    if has_update && settings.update.policy == "auto" {
        if let Some(asset) = matching_asset(&assets) {
            if staged_dir(&state.infra.paths, &latest).exists() {
                return Ok(());
            }
            if let Err(error) = download_and_stage(&state.infra.paths, asset, &latest).await {
                tracing::warn!(%error, "自动下载更新暂存失败，保留提示不重试");
                return Ok(());
            }
            tracing::info!(version = %latest, "新版本已自动下载并校验暂存（不自动替换，重启不生效，需手动点击立即更新）");
        }
    }
    Ok(())
}
