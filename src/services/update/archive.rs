//! 更新包解压与归档预检：zip（Windows 用 PowerShell / Unix 兜底 unzip、python3）
//! 与 tar.gz（tar），解压前逐条校验归档条目名，防 zip-slip / tar 穿越。

use anyhow::{bail, Context, Result};
use std::path::Path;

/// PowerShell 单引号字符串转义：' → ''（与 spawn_windows_deferred_swap 同规则）。
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// 解压 zip（Windows 用 PowerShell）/ tar.gz（Unix 用 tar），零新增 Rust 依赖。
///
/// 两条路径都先过 `precheck_archive`：条目名越界的包在解压前就被拒绝。
pub(super) fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".zip") {
        precheck_archive(archive, dest)?;
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
        precheck_archive(archive, dest)?;
        let archive = archive.to_string_lossy().to_string();
        let dest = dest.to_string_lossy().to_string();
        return run_command("tar", &["-xzf", &archive, "-C", &dest]).context("tar 解压更新包失败");
    }
    bail!("不支持的更新包格式: {name}")
}

/// 解压前预检：列出归档条目名并逐个做边界校验。
///
/// 读不到条目清单时按"拒绝解压"处理——宁可更新失败并给出明确原因，
/// 也不放行一个没被校验过的归档。
fn precheck_archive(archive: &Path, dest: &Path) -> Result<()> {
    let entries = list_archive_entries(archive)?;
    if entries.is_empty() {
        bail!("更新包内没有可校验的归档条目，拒绝解压");
    }
    validate_archive_entries(dest, &entries)
}

/// 归档条目名的越界校验（zip-slip / tar 穿越防护）。
///
/// Expand-Archive、unzip、tar 各自都带穿越防护，但那是**版本相关的外部行为**，
/// 实测同一形态三种处理方式并存：GNU tar 剥掉前导 `../` 后继续解、bsdtar 与 Info-ZIP
/// 各自改写或跳过、in-box PowerShell 5.1（Microsoft.PowerShell.Archive 1.0.1.0）的
/// Expand-Archive 跳过越界条目后**仍以 0 退出**（调用方看不出包被截断）。
/// 更新包最终覆盖 app_root 下的程序文件，越界条目则能写到暂存目录之外，
/// 因此改用仓内既有的 `file_safety` 边界校验自己把关：任一条目越界即整包拒绝。
fn validate_archive_entries(dest: &Path, entries: &[String]) -> Result<()> {
    for entry in entries {
        // zip 规范用 `/`，但部分打包器与 Windows 产物会写 `\`：两种都按分隔符处理。
        let relative = entry.replace('\\', "/");
        // 绝对路径（`/etc/x`、`C:/x`、UNC）不做"改写成相对"的宽容处理：
        // 拼到 dest 上仍会落到目录之外，直接由 ensure_within_root 判负。
        crate::services::file_safety::ensure_within_root(dest, &dest.join(&relative))
            .map_err(|_| anyhow::anyhow!("更新包含越界的归档条目，已拒绝解压: {entry}"))?;
    }
    Ok(())
}

/// python3 列条目用的内联脚本（与 extract_archive 的 zipfile 兜底同款工具链）。
#[cfg_attr(windows, allow(dead_code))]
const LIST_ZIP_WITH_PYTHON: &str =
    "import sys,zipfile; [print(name) for name in zipfile.ZipFile(sys.argv[1]).namelist()]";

/// 列出归档内的条目名（只列不解压）：沿用解压同款外部工具，零新增 Rust 依赖。
fn list_archive_entries(archive: &Path) -> Result<Vec<String>> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let archive_text = archive.to_string_lossy().to_string();
    let raw = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        run_command_capture("tar", &["-tzf", &archive_text]).context("列出 tar 更新包条目失败")?
    } else {
        #[cfg(windows)]
        {
            // 直接读 zip 中央目录：拿到的是未经 Expand-Archive 改写的原始条目名。
            let script = format!(
                "Add-Type -AssemblyName System.IO.Compression.FileSystem\n\
                 $zip=[System.IO.Compression.ZipFile]::OpenRead({})\n\
                 try {{ $zip.Entries | ForEach-Object {{ $_.FullName }} }} finally {{ $zip.Dispose() }}\n",
                ps_quote(archive)
            );
            run_command_capture(
                "powershell",
                &["-NoProfile", "-NonInteractive", "-Command", &script],
            )
            .context("列出 zip 更新包条目失败")?
        }
        #[cfg(not(windows))]
        {
            // 与解压分支保持同一工具顺序：unzip 优先，python3 兜底。
            run_command_capture("unzip", &["-Z1", &archive_text])
                .or_else(|_| {
                    run_command_capture("python3", &["-c", LIST_ZIP_WITH_PYTHON, &archive_text])
                })
                .context("列出 zip 更新包条目失败")?
        }
    };
    Ok(raw
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect())
}

fn run_command_capture(program: &str, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("启动 {program} 失败"))?;
    if !output.status.success() {
        bail!("{program} 退出码异常: {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_quote_escapes_single_quotes() {
        assert_eq!(ps_quote(Path::new("C:/ok.zip")), "'C:/ok.zip'");
        assert_eq!(ps_quote(Path::new("C:/it's.zip")), "'C:/it''s.zip'");
    }

    /// 归档条目越界预检（纯函数，不起子进程）：正常条目放行，`../` 与绝对路径拒绝。
    #[test]
    fn archive_entries_are_confined_to_destination() {
        #[cfg(windows)]
        let dest = Path::new(r"C:\app\data\updates\2.0.1");
        #[cfg(not(windows))]
        let dest = Path::new("/app/data/updates/2.0.1");
        let reject = |entry: &str| {
            assert!(
                validate_archive_entries(dest, &[entry.to_string()]).is_err(),
                "应拒绝条目: {entry}"
            );
        };

        // 正常条目：单文件、多级目录、目录条目，以及以反斜杠分隔的产物都放行。
        assert!(validate_archive_entries(
            dest,
            &[
                "bulibuli-windows-x86_64-portable/bulibuli-core.exe".to_string(),
                "bulibuli-windows-x86_64-portable/static/app/index.html".to_string(),
                "bulibuli-windows-x86_64-portable/resources/".to_string(),
                "bulibuli-linux-x86_64-portable\\resources\\ffmpeg.exe".to_string(),
            ]
        )
        .is_ok());
        // 包内自省的 `./` 与不越界的 `..` 仍留在 dest 内，不该误伤。
        assert!(validate_archive_entries(
            dest,
            &["./pkg/a.txt".to_string(), "pkg/../b.txt".to_string()]
        )
        .is_ok());

        // `../` 穿越：前导式、内嵌式、反斜杠式，以及裸 `..`。
        for entry in [
            "../evil.txt",
            "pkg/../../evil.txt",
            "pkg\\..\\..\\evil.txt",
            "..",
        ] {
            reject(entry);
        }
        // 绝对路径：Unix 根路径在所有平台都拒绝（Windows 上会被判为盘根相对路径）。
        for entry in ["/etc/passwd", "/absolute/inside/../../out"] {
            reject(entry);
        }
        // 盘符与 UNC 只在 Windows 语义下才是绝对路径（Unix 上 `C:` 是合法文件名字符）。
        #[cfg(windows)]
        for entry in [
            "C:/Windows/system.ini",
            r"\\server\share\x.exe",
            r"D:\data\evil.exe",
        ] {
            reject(entry);
        }
    }
}
