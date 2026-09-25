//! 暂存更新包的应用（替换程序文件）：exe、static/、resources/ 与包内其余文件，
//! 全程备份-换入-恢复，永不触碰 `data/`。Windows 运行中 exe 被占用时整体推迟，
//! 由下次启动钩子派分离的 PowerShell 辅助进程完成替换。

use super::STAGED_MARKER;
use crate::config::AppPaths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 暂存更新标记：记录待替换的版本与暂存目录，供下次启动钩子消费。
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

fn exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "bulibuli-core.exe"
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
        // 尽力清理：old.exe 可能仍被占用，暂存目录可能残留只读文件，
        // 清理失败只影响磁盘残留，不影响"更新已生效"的结论（启动钩子会再兜底）。
        std::fs::remove_file(&old_exe).ok();
        std::fs::remove_dir_all(staged).ok();
        Ok(ApplyOutcome::Applied)
    }

    #[cfg(not(windows))]
    {
        // POSIX rename 原子替换：运行中进程继续持有旧 inode，重启后自然使用新二进制。
        if old_exe.exists() {
            // 尽力清理上次替换残留的 .old 文件；被占用时 rename 会自行报错。
            std::fs::remove_file(&old_exe).ok();
        }
        std::fs::rename(&exe, &old_exe)
            .with_context(|| format!("重命名旧可执行文件失败: {}", exe.display()))?;
        if let Err(error) = std::fs::rename(staged.join(exe_name()), &exe) {
            if let Err(restore_error) = std::fs::rename(&old_exe, &exe) {
                // 恢复失败值得留痕：旧 exe 卡在 .old 上，程序可能无法启动。
                tracing::error!(
                    %restore_error,
                    old_exe = %old_exe.display(),
                    "换入新可执行文件失败，且恢复旧版本失败"
                );
            }
            return Err(error.into());
        }
        replace_package_files(app_root, staged)?;
        // 尽力清理替换残留（同 Windows 分支，失败只影响磁盘残留）。
        std::fs::remove_file(&old_exe).ok();
        std::fs::remove_dir_all(staged).ok();
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
        // 尽力清理上次异常中断留下的旧备份；失败时下方 rename 备份会自行报错。
        std::fs::remove_dir_all(&backup).ok();
    }
    if target.exists() {
        std::fs::rename(target, &backup)
            .with_context(|| format!("备份旧 {label} 目录失败: {}", target.display()))?;
    }
    if let Err(error) = std::fs::rename(source, target) {
        // 恢复旧目录：宁可保持旧版本完整，也不留下半新半旧的混合状态。
        if backup.exists() {
            if let Err(restore_error) = std::fs::rename(&backup, target) {
                tracing::error!(
                    %restore_error,
                    backup = %backup.display(),
                    "替换 {label} 失败，且恢复备份目录失败，可能残留半新半旧的混合状态"
                );
            }
        }
        return Err(anyhow::Error::new(error)
            .context(format!("替换 {label} 目录失败: {}", target.display())));
    }
    if backup.exists() {
        // 尽力清理备份目录：残留只占磁盘，下次启动钩子会再兜底清理。
        std::fs::remove_dir_all(&backup).ok();
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
    // 标记已消费，尽力删除；删除失败只是下次启动重复应用一次，无副作用。
    std::fs::remove_file(&path).ok();
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
            // 尽力清理：启动早期无法可靠上报，残留由下次启动继续清理。
            std::fs::remove_dir_all(&path).ok();
        }
    }
    // 同理清理上次替换可能残留的旧可执行文件：直接替换路径的 <exe>.old 与
    // PowerShell 延迟替换路径的 bulibuli-core.old.exe（Windows 运行中删除失败时
    // 会残留，启动时进程尚未被自己锁定之外的方式占用，删除更可能成功）。
    // 删除失败只影响磁盘残留，后续替换流程可正常进行。
    std::fs::remove_file(paths.app_root.join(format!("{}.old", exe_name()))).ok();
    #[cfg(windows)]
    std::fs::remove_file(paths.app_root.join("bulibuli-core.old.exe")).ok();
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
    // 轮询等待 bulibuli-core.exe 解锁（程序退出）后替换；10 分钟内未解锁则放弃，
    // 标记已由启动钩子消费，暂存包保留给下一次手动 apply 复用。
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $app='{app_root}'\n\
         $staged='{staged_str}'\n\
         $exe=Join-Path $app 'bulibuli-core.exe'\n\
         $old=Join-Path $app 'bulibuli-core.old.exe'\n\
         $renamed=$false\n\
         for($i=0;$i -lt 600;$i++){{\n\
           try {{ Rename-Item -LiteralPath $exe -NewName 'bulibuli-core.old.exe' -Force -ErrorAction Stop; $renamed=$true; break }} catch {{ Start-Sleep -Seconds 1 }}\n\
         }}\n\
         if(-not $renamed){{ exit 0 }}\n\
         try {{ Move-Item -LiteralPath (Join-Path $staged 'bulibuli-core.exe') -Destination $exe -Force -ErrorAction Stop }} catch {{\n\
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
         # The launcher can still be locked briefly while it waits for PowerShell/Core.\n\
         # Retry it separately; leave the staged directory and marker for the next boot\n\
         # if the outer process does not release the image within the retry window.\n\
         $launcherSource=Join-Path $staged 'bulibuli.exe'\n\
         if(Test-Path -LiteralPath $launcherSource){{\n\
           $launcher=Join-Path $app 'bulibuli.exe'\n\
           $launcherCopied=$false\n\
           for($i=0;$i -lt 600;$i++){{\n\
             try {{ Copy-Item -LiteralPath $launcherSource -Destination $launcher -Force -ErrorAction Stop; $launcherCopied=$true; break }} catch {{ Start-Sleep -Seconds 1 }}\n\
           }}\n\
           if(-not $launcherCopied){{ exit 1 }}\n\
         }}\n\
         Get-ChildItem -LiteralPath $staged -Force | Where-Object {{ $_.Name -ne 'data' -and $_.Name -ne 'bulibuli.exe' -and $_.Name -ne 'bulibuli-core.exe' -and $_.Name -ne 'static' -and $_.Name -ne 'resources' }} | ForEach-Object {{\n\
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
