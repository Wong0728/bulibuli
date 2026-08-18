//! 应用内更新机制（一期）：检查 GitHub Release 最新版本、下载校验暂存、替换程序文件。
//!
//! 版本源与三平台安装器一致：从 GitHub API 找第一个带 `latest.json` 资产的 release，
//! 下载并解析其中的资产清单（`releases/latest/download/latest.json` 对全部为
//! pre-release 的仓库不解析，不能作为版本源）。
//!
//! 替换策略：只替换程序文件（可执行文件 / static / resources / 包清单），
//! 永不触碰 `data/`。Unix 上可直接替换（运行中进程持有旧 inode，重启后生效）；
//! Windows 运行中无法替换自己的 exe，改为写 `data/update-staged.json` 标记，
//! 下次启动早期钩子（main.rs）派一个分离的辅助进程在程序退出后完成替换。

use crate::config::AppPaths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "Wong0728/bulibuli";
pub const STAGED_MARKER: &str = "update-staged.json";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedMarker {
    pub version: String,
    pub staged_dir: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 已完成替换（重启后生效）。
    Applied,
    /// 程序运行中无法替换，已暂存，退出/下次启动后完成（仅 Windows 需要）。
    #[cfg(windows)]
    Staged,
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("bulibuli/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .context("创建更新检查 HTTP 客户端失败")
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

/// 版本比较：v2.0.0-alpha.9 按 (major, minor, patch, 预发布类型, 预发布号) 比较，
/// 正式版大于任何预发布版。
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => {
            let core = x.0.cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2));
            match (x.3, y.3) {
                (None, None) => core,
                (None, Some(_)) => core.then(Ordering::Greater),
                (Some(_), None) => core.then(Ordering::Less),
                (Some(px), Some(py)) => core.then(px.cmp(&py)),
            }
        }
        _ => a.cmp(b),
    }
}

/// 解析后的版本号：核心三元组 + 可选预发布（类型序号 alpha<beta<rc，发布号）。
type ParsedVersion = (u64, u64, u64, Option<(u8, u64)>);

fn parse_version(version: &str) -> Option<ParsedVersion> {
    let version = version.trim().trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut nums = core.split('.');
    let major = nums.next()?.parse().ok()?;
    let minor = nums.next()?.parse().ok()?;
    let patch = nums.next().unwrap_or("0").parse().ok()?;
    let pre = pre.and_then(|pre| {
        let (kind, number) = pre.split_once('.')?;
        let rank = match kind {
            "alpha" => 1,
            "beta" => 2,
            "rc" => 3,
            _ => return None,
        };
        Some((rank, number.parse().ok()?))
    });
    Some((major, minor, patch, pre))
}

/// 当前平台的 (platform, architecture, variant)，与 latest.json 资产字段对齐。
pub fn current_platform() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_arch = "x86_64")]
    let arch = "x86_64";
    #[cfg(target_arch = "aarch64")]
    let arch = "arm64";
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!("bulibuli 更新机制暂不支持当前 CPU 架构");

    #[cfg(target_os = "windows")]
    {
        ("windows", arch, "portable")
    }
    #[cfg(target_os = "macos")]
    {
        ("macos", arch, "portable")
    }
    #[cfg(target_os = "android")]
    {
        ("termux", "arm64", "portable")
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        ("linux", arch, "portable")
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "android",
        all(unix, not(target_os = "macos"), not(target_os = "android"))
    )))]
    {
        ("unknown", arch, "portable")
    }
}

/// 从资产清单里选当前平台匹配的 portable 包。
pub fn matching_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    let (platform, architecture, variant) = current_platform();
    assets.iter().find(|asset| {
        asset.platform == platform && asset.architecture == architecture && asset.variant == variant
    })
}

/// 暂存目录：data/updates/<version>（不含 v 前缀）。
pub fn staged_dir(paths: &AppPaths, version: &str) -> PathBuf {
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
) -> Result<PathBuf> {
    let client = http_client()?;
    let stage = staged_dir(paths, version);
    if stage.exists() {
        std::fs::remove_dir_all(&stage)
            .with_context(|| format!("清理旧暂存目录失败: {}", stage.display()))?;
    }
    std::fs::create_dir_all(&stage)
        .with_context(|| format!("创建暂存目录失败: {}", stage.display()))?;

    let bytes = client
        .get(&asset.download_url)
        .send()
        .await
        .context("下载更新包失败")?
        .error_for_status()
        .context("更新包请求失败")?
        .bytes()
        .await
        .context("读取更新包失败")?;
    let digest = hex::encode(Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        bail!(
            "更新包 SHA-256 校验失败（期望 {}，实际 {digest}）",
            asset.sha256
        );
    }

    let archive = stage.join(&asset.name);
    std::fs::write(&archive, &bytes).context("写入更新包失败")?;
    extract_archive(&archive, &stage)?;
    std::fs::remove_file(&archive).ok();
    locate_package_root(&stage)
}

/// 解压 zip（Windows 用 PowerShell）/ tar.gz（Unix 用 tar），零新增 Rust 依赖。
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".zip") {
        #[cfg(windows)]
        {
            let script = format!(
                "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
                archive.display(),
                dest.display()
            );
            run_command(
                "powershell",
                &["-NoProfile", "-NonInteractive", "-Command", &script],
            )
            .context("PowerShell 解压更新包失败")?;
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            // Windows 包不会出现在 Unix 平台；兜底尝试 unzip / python zipfile。
            if std::process::Command::new("unzip")
                .arg("-q")
                .arg("-o")
                .arg(archive)
                .arg("-d")
                .arg(dest)
                .status()
                .is_ok_and(|status| status.success())
            {
                return Ok(());
            }
            let script = format!(
                "import zipfile,sys; zipfile.ZipFile(r'{}').extractall(r'{}')",
                archive.display(),
                dest.display()
            );
            run_command("python3", &["-c", &script]).context("解压 zip 更新包失败")?;
            return Ok(());
        }
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let archive = archive.to_string_lossy().to_string();
        let dest = dest.to_string_lossy().to_string();
        return run_command("tar", &["-xzf", &archive, "-C", &dest]).context("tar 解压更新包失败");
    }
    bail!("不支持的更新包格式: {name}")
}

fn run_command(program: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("启动 {program} 失败"))?;
    if !status.success() {
        bail!("{program} 退出码异常: {status}");
    }
    Ok(())
}

/// 归档可能把包放在嵌套目录里（与安装器的 Get-PackageRoot 行为一致），
/// 找到含 bulibuli.package.json 的目录作为包根。
fn locate_package_root(dest: &Path) -> Result<PathBuf> {
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

fn exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "bulibuli.exe"
    }
    #[cfg(not(windows))]
    {
        "bulibuli"
    }
}

/// 把暂存包内容替换到程序目录（跳过 data/）。返回替换结果。
/// Windows 运行中 exe 被占用时整体推迟：写 data/update-staged.json，由下次启动钩子
/// 派分离的辅助进程在程序退出后完成替换。
pub fn apply_staged(paths: &AppPaths, staged: &Path) -> Result<ApplyOutcome> {
    let app_root = &paths.app_root;
    if !staged.join(exe_name()).is_file() {
        bail!("暂存更新包缺少可执行文件 {}", exe_name());
    }
    let exe = app_root.join(exe_name());
    let old_exe = app_root.join(format!("{}.old", exe_name()));

    #[cfg(windows)]
    {
        // 先尝试直接替换：成功（程序未运行）即完成；失败（运行中锁定）则整体暂存。
        match std::fs::rename(&exe, &old_exe) {
            Ok(()) => {
                if let Err(error) = std::fs::rename(staged.join(exe_name()), &exe) {
                    let _ = std::fs::rename(&old_exe, &exe);
                    return Err(error.into());
                }
            }
            Err(_) => {
                write_staged_marker(paths, staged)?;
                return Ok(ApplyOutcome::Staged);
            }
        }
        replace_package_files(app_root, staged)?;
        let _ = std::fs::remove_file(&old_exe);
        let _ = std::fs::remove_dir_all(staged);
        Ok(ApplyOutcome::Applied)
    }

    #[cfg(not(windows))]
    {
        // POSIX rename 原子替换：运行中进程继续持有旧 inode，重启后自然使用新二进制。
        if old_exe.exists() {
            let _ = std::fs::remove_file(&old_exe);
        }
        std::fs::rename(&exe, &old_exe)
            .with_context(|| format!("重命名旧可执行文件失败: {}", exe.display()))?;
        if let Err(error) = std::fs::rename(staged.join(exe_name()), &exe) {
            let _ = std::fs::rename(&old_exe, &exe);
            return Err(error.into());
        }
        replace_package_files(app_root, staged)?;
        let _ = std::fs::remove_file(&old_exe);
        let _ = std::fs::remove_dir_all(staged);
        Ok(ApplyOutcome::Applied)
    }
}

/// 替换 static/、resources/ 与包根下其余文件；跳过 data/ 与可执行文件（已单独替换）。
fn replace_package_files(app_root: &Path, staged: &Path) -> Result<()> {
    for dir in ["static", "resources"] {
        let source = staged.join(dir);
        let target = app_root.join(dir);
        if !source.is_dir() {
            continue;
        }
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("清理旧 {dir} 目录失败: {}", target.display()))?;
        }
        std::fs::rename(&source, &target)
            .with_context(|| format!("替换 {dir} 目录失败: {}", target.display()))?;
    }
    for entry in std::fs::read_dir(staged).context("读取暂存包失败")? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "data" || name == exe_name() || name == "static" || name == "resources" {
            continue;
        }
        let target = app_root.join(&name);
        if entry.path().is_dir() {
            if target.exists() {
                std::fs::remove_dir_all(&target).ok();
            }
            std::fs::rename(entry.path(), &target)
                .with_context(|| format!("替换目录失败: {}", target.display()))?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("替换文件失败: {}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_staged_marker(paths: &AppPaths, staged: &Path) -> Result<()> {
    let version = staged
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let marker = StagedMarker {
        version,
        staged_dir: staged.to_string_lossy().to_string(),
    };
    let path = paths.data_dir.join(STAGED_MARKER);
    std::fs::write(&path, serde_json::to_string_pretty(&marker)?)
        .with_context(|| format!("写入暂存标记失败: {}", path.display()))?;
    Ok(())
}

fn read_staged_marker(paths: &AppPaths) -> Option<StagedMarker> {
    let path = paths.data_dir.join(STAGED_MARKER);
    let raw = std::fs::read_to_string(&path).ok()?;
    let marker: StagedMarker = serde_json::from_str(&raw).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(marker)
}

/// 启动早期钩子（main.rs 调用）：处理上次运行遗留的暂存更新。
/// Unix：直接替换；Windows：尝试替换，仍被占用时派分离的 PowerShell 辅助进程，
/// 等待程序退出后完成替换（下次启动即新版本）。
pub fn startup_apply_staged(paths: &AppPaths) -> Result<()> {
    let Some(marker) = read_staged_marker(paths) else {
        return Ok(());
    };
    let staged = PathBuf::from(&marker.staged_dir);
    if !staged.join(exe_name()).is_file() {
        // 暂存包不完整（上次中断）：丢弃标记，下次 apply 会重新下载。
        return Ok(());
    }
    match apply_staged(paths, &staged) {
        Ok(ApplyOutcome::Applied) => Ok(()),
        #[cfg(windows)]
        Ok(ApplyOutcome::Staged) => spawn_windows_deferred_swap(paths, &staged),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn spawn_windows_deferred_swap(paths: &AppPaths, staged: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    let app_root = paths.app_root.to_string_lossy().replace('\'', "''");
    let staged_str = staged.to_string_lossy().replace('\'', "''");
    let data_dir = paths.data_dir.to_string_lossy().replace('\'', "''");
    let marker = data_dir.clone() + r"\update-staged.json";
    // 轮询等待 bulibuli.exe 解锁（程序退出）后替换；10 分钟内未解锁则放弃，
    // 标记已由启动钩子消费，暂存包保留给下一次手动 apply 复用。
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $app='{app_root}'\n\
         $staged='{staged_str}'\n\
         $exe=Join-Path $app 'bulibuli.exe'\n\
         $old=Join-Path $app 'bulibuli.old.exe'\n\
         $renamed=$false\n\
         for($i=0;$i -lt 600;$i++){{\n\
           try {{ Rename-Item -LiteralPath $exe -NewName 'bulibuli.old.exe' -Force -ErrorAction Stop; $renamed=$true; break }} catch {{ Start-Sleep -Seconds 1 }}\n\
         }}\n\
         if(-not $renamed){{ exit 0 }}\n\
         Move-Item -LiteralPath (Join-Path $staged 'bulibuli.exe') -Destination $exe -Force\n\
         foreach($dir in @('static','resources')){{\n\
           $src=Join-Path $staged $dir\n\
           if(Test-Path -LiteralPath $src){{\n\
             $dst=Join-Path $app $dir\n\
             if(Test-Path -LiteralPath $dst){{ Remove-Item -LiteralPath $dst -Recurse -Force }}\n\
             Move-Item -LiteralPath $src -Destination $dst -Force\n\
           }}\n\
         }}\n\
         Get-ChildItem -LiteralPath $staged -Force | Where-Object {{ $_.Name -ne 'data' -and $_.Name -ne 'bulibuli.exe' -and $_.Name -ne 'static' -and $_.Name -ne 'resources' }} | ForEach-Object {{\n\
           $target=Join-Path $app $_.Name\n\
           if($_.PSIsContainer){{ if(Test-Path -LiteralPath $target){{ Remove-Item -LiteralPath $target -Recurse -Force }}; Move-Item -LiteralPath $_.FullName -Destination $target -Force }}\n\
           else {{ Copy-Item -LiteralPath $_.FullName -Destination $target -Force }}\n\
         }}\n\
         Remove-Item -LiteralPath $staged -Recurse -Force -ErrorAction SilentlyContinue\n\
         Remove-Item -LiteralPath '{marker}' -Force -ErrorAction SilentlyContinue\n\
         Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue\n"
    );
    let script_path = paths.data_dir.join("update-swap.ps1");
    std::fs::write(&script_path, script)
        .with_context(|| format!("写入更新替换脚本失败: {}", script_path.display()))?;
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
            script_path.to_str().unwrap_or_default(),
        ])
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .context("启动更新替换辅助进程失败")?;
    Ok(())
}

#[cfg(not(windows))]
fn spawn_windows_deferred_swap(_paths: &AppPaths, _staged: &Path) -> Result<()> {
    unreachable!("非 Windows 平台不会走到延迟替换分支")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_orders_prereleases_and_releases() {
        assert_eq!(
            compare_versions("v2.0.0-alpha.9", "v2.0.0-alpha.8"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0-alpha.10", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0-beta.1", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.1-alpha.1", "v2.0.0"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("v2.0.0", "v2.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("v1.9.9", "v2.0.0"), Ordering::Less);
    }

    #[test]
    fn version_comparison_tolerates_unparseable_input() {
        // 无法解析时回退字符串比较，不会 panic。
        let _ = compare_versions("latest", "v2.0.0");
    }
}
