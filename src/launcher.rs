#![cfg_attr(windows, windows_subsystem = "windows")]

//! Windows public entry point.
//!
//! The TUI must be started as a child of PowerShell so Explorer launches get
//! the same console initialization path as the known-good terminal launch.
//! The actual application remains `bulibuli-core.exe` beside this launcher.

#[cfg(windows)]
use anyhow::Context;

#[cfg(windows)]
use std::{env, path::PathBuf, process::Command};

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("bulibuli 启动器失败: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("bulibuli-launcher 仅用于 Windows；请直接运行 bulibuli");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> anyhow::Result<()> {
    let launcher = env::current_exe().context("获取启动器路径失败")?;
    let app_root = launcher
        .parent()
        .context("启动器路径没有父目录")?
        .to_path_buf();
    let core = app_root.join("bulibuli-core.exe");
    let script = app_root.join("bulibuli-launch.ps1");
    anyhow::ensure!(core.is_file(), "缺少 Core 程序: {}", core.display());
    anyhow::ensure!(
        script.is_file(),
        "缺少 PowerShell 启动脚本: {}",
        script.display()
    );

    let system_root = env::var_os("SystemRoot").context("Windows SystemRoot 环境变量缺失")?;
    let powershell = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    anyhow::ensure!(
        powershell.is_file(),
        "找不到 Windows PowerShell: {}",
        powershell.display()
    );

    let mut command = Command::new(powershell);
    command
        .current_dir(&app_root)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg(&core);
    command.args(env::args_os().skip(1));

    let status = command.status().context("启动 PowerShell 失败")?;
    std::process::exit(status.code().unwrap_or(1));
}
