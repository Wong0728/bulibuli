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
//!
//! 子模块分工：`check` 拉取版本清单与启动检查，`version` 版本比较与平台资产匹配，
//! `download` 下载校验暂存，`archive` 解压与归档预检，`apply` 替换程序文件。

mod apply;
mod archive;
mod check;
mod download;
mod version;

pub use apply::{apply_staged, startup_apply_staged, ApplyOutcome};
pub use check::{fetch_latest, startup_check};
pub use download::{download_and_stage, staged_dir};
pub use version::{compare_versions, matching_asset};

/// 更新检查所针对的 GitHub 仓库。
pub const REPO: &str = "Wong0728/bulibuli";

/// Windows 延迟替换标记文件名（data/ 下），见 `apply::write_staged_marker`。
pub const STAGED_MARKER: &str = "update-staged.json";
