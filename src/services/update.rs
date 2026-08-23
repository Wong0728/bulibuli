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

/// latest.json 中 `download_url` 的信任域白名单：
/// 仅允许 GitHub Release 资产域名，防止清单被篡改后把客户端引向任意下载源。
fn validate_download_url(raw: &str) -> Result<()> {
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
    let pre = pre.map(|pre| {
        // 无法识别的预发布后缀（nightly、dev、无序号 alpha 等）保守按最低档
        // （rank 0，早于 alpha）处理：宁可被当作"更旧的预发布"，也不当作
        // 正式版——否则 latest.json 里出现陌生后缀时会误判为正式发布，
        // 触发更新提示/自动下载的错误决策。
        let (kind, number) = pre.split_once('.').unwrap_or((pre, ""));
        let rank = match kind {
            "alpha" => 1,
            "beta" => 2,
            "rc" => 3,
            _ => 0,
        };
        (rank, number.parse().unwrap_or(0))
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

/// PowerShell 单引号字符串转义：' → ''（与 spawn_windows_deferred_swap 同规则）。
#[cfg_attr(not(windows), allow(dead_code))]
fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// 下载当前平台匹配资产 → 校验 sha256 → 解压到暂存目录，返回包根目录。
pub async fn download_and_stage(
    paths: &AppPaths,
    asset: &ReleaseAsset,
    version: &str,
) -> Result<PathBuf> {
    validate_remote_component(version.trim_start_matches('v'))?;
    validate_remote_component(&asset.name)?;
    validate_download_url(&asset.download_url)?;
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
        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_file(archive.with_extension("part"));
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

/// 解压 zip（Windows 用 PowerShell）/ tar.gz（Unix 用 tar），零新增 Rust 依赖。
fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".zip") {
        #[cfg(windows)]
        {
            // 路径拼入脚本前必须转义单引号（与 spawn_windows_deferred_swap 一致），
            // 否则 latest.json 中的恶意资产名可闭合引号注入任意 PowerShell 命令。
            let script = format!(
                "Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
                ps_quote(archive),
                ps_quote(dest)
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
            // 路径通过 argv 传入而非内插进脚本，避免任意代码注入。
            let archive_str = archive.to_string_lossy().to_string();
            let dest_str = dest.to_string_lossy().to_string();
            run_command(
                "python3",
                &[
                    "-c",
                    "import sys,zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])",
                    &archive_str,
                    &dest_str,
                ],
            )
            .context("解压 zip 更新包失败")?;
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
                    // 换入失败：先把旧 exe 挪回原位（保留原版）；恢复也失败时
                    // old.exe 会残留，明确写进错误信息（README 口径：可手动删）。
                    if let Err(restore_error) = std::fs::rename(&old_exe, &exe) {
                        return Err(anyhow::Error::new(error).context(format!(
                            "换入新可执行文件失败，且恢复旧版本失败（{restore_error}）；目录下可能残留 {}，确认程序未运行后可手动删除",
                            old_exe.display()
                        )));
                    }
                    return Err(error.into());
                }
            }
            Err(_) => {
                write_staged_marker(paths, staged)?;
                return Ok(ApplyOutcome::Staged);
            }
        }
        // 附属文件替换失败：新 exe 已就位，尽力清理 old.exe（可能因占用失败），
        // 清理失败时明确指出残留位置，与 README"替换失败可能残留 old.exe"一致。
        if let Err(error) = replace_package_files(app_root, staged) {
            if std::fs::remove_file(&old_exe).is_ok() {
                return Err(error);
            }
            return Err(error.context(format!(
                "更新附属文件替换失败，且旧可执行文件 {} 清理失败，可手动删除",
                old_exe.display()
            )));
        }
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
///
/// 目录替换采用"先备份再换入"：旧目录先 rename 到 `<name>.old`，换入失败时原样恢复，
/// 避免出现"新 exe + 缺失 static/"的混合状态。
fn replace_package_files(app_root: &Path, staged: &Path) -> Result<()> {
    for dir in ["static", "resources"] {
        let source = staged.join(dir);
        let target = app_root.join(dir);
        if !source.is_dir() {
            continue;
        }
        replace_dir_with_backup(&source, &target, dir)?;
    }
    for entry in std::fs::read_dir(staged).context("读取暂存包失败")? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "data" || name == exe_name() || name == "static" || name == "resources" {
            continue;
        }
        let target = app_root.join(&name);
        if entry.path().is_dir() {
            replace_dir_with_backup(&entry.path(), &target, &name.to_string_lossy())?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("替换文件失败: {}", target.display()))?;
        }
    }
    Ok(())
}

/// 用 `source` 目录替换 `target`：旧目录先挪到 `<target>.old` 备份位，
/// 换入失败时恢复备份；成功后清理备份。`label` 仅用于错误信息。
fn replace_dir_with_backup(source: &Path, target: &Path, label: &str) -> Result<()> {
    let mut backup_name = target
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    backup_name.push(".old");
    let backup = target.with_file_name(backup_name);
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }
    if target.exists() {
        std::fs::rename(target, &backup)
            .with_context(|| format!("备份旧 {label} 目录失败: {}", target.display()))?;
    }
    if let Err(error) = std::fs::rename(source, target) {
        // 恢复旧目录：宁可保持旧版本完整，也不留下半新半旧的混合状态。
        if backup.exists() {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(anyhow::Error::new(error)
            .context(format!("替换 {label} 目录失败: {}", target.display())));
    }
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
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
    // 清理上次更新可能残留的目录备份（换入成功但备份未及清理的异常中断兜底）。
    for leftover in ["static.old", "resources.old"] {
        let path = paths.app_root.join(leftover);
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    // 同理清理上次替换可能残留的旧可执行文件：直接替换路径的 <exe>.old 与
    // PowerShell 延迟替换路径的 bulibuli.old.exe（Windows 运行中删除失败时
    // 会残留，启动时进程尚未被自己锁定之外的方式占用，删除更可能成功）。
    let _ = std::fs::remove_file(paths.app_root.join(format!("{}.old", exe_name())));
    #[cfg(windows)]
    let _ = std::fs::remove_file(paths.app_root.join("bulibuli.old.exe"));
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
         try {{ Move-Item -LiteralPath (Join-Path $staged 'bulibuli.exe') -Destination $exe -Force -ErrorAction Stop }} catch {{\n\
           # 新 exe 就位失败时把旧 exe 改回原名，避免程序卡在 .old 上无法启动。\n\
           Move-Item -LiteralPath $old -Destination $exe -Force\n\
           exit 1\n\
         }}\n\
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

    #[test]
    fn unrecognized_prerelease_suffixes_are_never_treated_as_release() {
        // 陌生后缀（nightly/dev/无序号 alpha）不得被当作正式版：与同核心版本
        // 的正式版比较必须更旧，与已知预发布比较也更旧（保守降档到 rank 0）。
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-nightly"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-alpha"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-dev.3"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0-alpha.1", "v2.0.0-nightly"),
            Ordering::Greater
        );
        // 语义不变：正式版之间、已知预发布之间照常比较。
        assert_eq!(compare_versions("v2.0.0", "v2.0.0"), Ordering::Equal);
        assert_eq!(
            compare_versions("v2.0.0-beta.1", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
    }

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

    #[test]
    fn powershell_quote_escapes_single_quotes() {
        assert_eq!(ps_quote(Path::new("C:/ok.zip")), "'C:/ok.zip'");
        assert_eq!(ps_quote(Path::new("C:/it's.zip")), "'C:/it''s.zip'");
    }
}
