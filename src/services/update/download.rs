//! 更新包下载与暂存：流式下载到 `<dest>.part` 再原子 rename，SHA-256 校验
//! 失败自动重试一次，解压后落到 `data/updates/<version>` 暂存目录。

use super::archive::extract_archive;
use super::check::ReleaseAsset;
use crate::config::AppPaths;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

/// 下载专用客户端：不设整体超时（大包下载耗时不可预估），
/// 改为连接超时 + 流式读取时逐块超时（见 download_to_file）。
fn download_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("bulibuli/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .context("创建更新下载 HTTP 客户端失败")
}

/// 流式下载到目标文件：先写 `<dest>.part` 临时文件，全部落盘后原子 rename，
/// 避免全量读入内存，也避免中断留下半截"成品"文件。返回响应体 SHA-256。
/// 每个分块读取有 60s 超时，防止连接僵死导致下载永久挂起。
/// 写盘走 tokio::fs 异步接口，避免阻塞运行时 worker 线程。
async fn download_to_file(client: &reqwest::Client, url: &str, dest: &Path) -> Result<String> {
    use tokio::io::AsyncWriteExt;
    let response = client
        .get(url)
        .send()
        .await
        .context("下载更新包失败")?
        .error_for_status()
        .context("更新包请求失败")?;
    let temp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&temp)
        .await
        .with_context(|| format!("创建临时下载文件失败: {}", temp.display()))?;
    let mut hasher = Sha256::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::time::timeout(
        Duration::from_secs(60),
        futures::StreamExt::next(&mut stream),
    )
    .await
    .context("下载更新包超时（单块 60s 无数据）")?
    {
        let chunk = chunk.context("读取更新包数据失败")?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.context("写入更新包数据失败")?;
    }
    file.sync_all().await.context("刷盘更新包数据失败")?;
    drop(file);
    tokio::fs::rename(&temp, dest)
        .await
        .with_context(|| format!("重命名更新包失败: {} → {}", temp.display(), dest.display()))?;
    Ok(hex::encode(hasher.finalize()))
}

/// 校验来自远端 latest.json 的路径片段（资产名 / 版本号）：
/// 只允许单一的 Normal 组件，拒绝路径分隔符（含跨平台反斜杠）、".."、盘符等注入或穿越形态。
fn validate_remote_component(value: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    let valid = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
        && !value.contains(':')
        && !value.contains('\\')
        && !value.starts_with('.');
    if !valid {
        bail!("更新清单字段含非法路径片段: {value}");
    }
    Ok(())
}

/// 暂存目录：data/updates/<version>（不含 v 前缀）。
pub fn staged_dir(paths: &AppPaths, version: &str) -> std::path::PathBuf {
    paths
        .data_dir
        .join("updates")
        .join(version.trim_start_matches('v'))
}

/// 下载当前平台匹配资产 → 校验 sha256 → 解压到暂存目录，返回包根目录。
pub async fn download_and_stage(
    paths: &AppPaths,
    asset: &ReleaseAsset,
    version: &str,
) -> Result<std::path::PathBuf> {
    validate_remote_component(version.trim_start_matches('v'))?;
    validate_remote_component(&asset.name)?;
    super::check::validate_download_url(&asset.download_url)?;
    let client = download_client()?;
    let stage = staged_dir(paths, version);
    if stage.exists() {
        std::fs::remove_dir_all(&stage)
            .with_context(|| format!("清理旧暂存目录失败: {}", stage.display()))?;
    }
    std::fs::create_dir_all(&stage)
        .with_context(|| format!("创建暂存目录失败: {}", stage.display()))?;

    // 下载 + 校验，失败重试 1 次（网络抖动/CDN 瞬断常见）。
    let archive = stage.join(&asset.name);
    let mut digest: Option<String> = None;
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..=1 {
        match download_to_file(&client, &asset.download_url, &archive).await {
            Ok(downloaded) => {
                if downloaded.eq_ignore_ascii_case(&asset.sha256) {
                    digest = Some(downloaded);
                    break;
                }
                last_error = Some(anyhow::anyhow!(
                    "更新包 SHA-256 校验失败（期望 {}，实际 {downloaded}）",
                    asset.sha256
                ));
            }
            Err(error) => last_error = Some(error),
        }
        // 尽力清理失败尝试留下的半截文件；删除失败不影响重试与错误上报。
        std::fs::remove_file(&archive).ok();
        std::fs::remove_file(archive.with_extension("part")).ok();
        if attempt == 0 {
            tracing::warn!(
                error = %last_error.as_ref().expect("首次尝试后必有错误"),
                "更新包下载/校验失败，1s 后重试一次"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let Some(_) = digest else {
        return Err(last_error.expect("下载失败时必有错误记录"));
    };

    extract_archive(&archive, &stage)?;
    std::fs::remove_file(&archive).ok();
    locate_package_root(&stage)
}

/// 归档可能把包放在嵌套目录里（与安装器的 Get-PackageRoot 行为一致），
/// 找到含 bulibuli.package.json 的目录作为包根。
fn locate_package_root(dest: &Path) -> Result<std::path::PathBuf> {
    if dest.join("bulibuli.package.json").is_file() {
        return Ok(dest.to_path_buf());
    }
    for entry in std::fs::read_dir(dest).context("读取解压目录失败")? {
        let entry = entry?;
        if entry.path().join("bulibuli.package.json").is_file() {
            return Ok(entry.path());
        }
    }
    bail!("解压后的更新包缺少 bulibuli.package.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_components_reject_path_injection() {
        assert!(validate_remote_component("2.0.1").is_ok());
        assert!(validate_remote_component("bulibuli-windows-x86_64.zip").is_ok());
        // 路径分隔符 / 穿越片段 / 盘符均拒绝（含 Unix 平台的 "C:evil" 形态）。
        for bad in ["../evil", "a/b", "a\\b", "C:evil", "..", ".hidden", ""] {
            assert!(validate_remote_component(bad).is_err(), "应拒绝: {bad}");
        }
        // 引号本身是合法文件名字符，注入风险由 ps_quote 转义消除（另有单测）。
        assert!(validate_remote_component("a'b").is_ok());
    }
}
