//! 启动流程 Onboarding：首次启动打印 Setup URL 并自动打开浏览器，后续启动显示状态摘要。
//!
//! 终端与用户的交互仅限于：看到 URL → 打开浏览器 → 完事。
//! 所有配置走网页 Setup 向导，终端不再有任何交互式向导步骤。
//!
//! `startup_state.json` 保存 onboarding 完成状态、AI 开关、终端模式；
//! 网络模式以 `security.toml` 为唯一真相源。

use crate::config::AppPaths;
use crate::services::credential::Credential;
use crate::state::SharedState;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const STARTUP_STATE_FILE: &str = "startup_state.json";
static STARTUP_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 终端模式：Web 模式（默认，日志 + 专家命令）或终端模式（完整交互式命令）。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TerminalMode {
    /// 默认：日志滚动 + 专家命令，输入 > 前缀命令
    #[default]
    Web,
    /// 完整交互式命令，显示帮助面板
    Terminal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartupState {
    pub onboarding_completed: bool,
    pub ai_skill_enabled: bool,
    pub bili_logged_in_uid: Option<i64>,
    pub last_modified: String,
    /// 终端模式：Web（默认）或 Terminal。持久化到 startup_state.json。
    #[serde(default)]
    pub terminal_mode: TerminalMode,
    /// 进程内瞬态：onboarding 步骤 3 选"现在扫码"时置 true，主流程据此触发 run_qr_login。不落盘。
    #[serde(skip)]
    pub scan_now_requested: bool,
}

impl Default for StartupState {
    fn default() -> Self {
        Self {
            onboarding_completed: false,
            ai_skill_enabled: false,
            bili_logged_in_uid: None,
            last_modified: Utc::now().to_rfc3339(),
            terminal_mode: TerminalMode::Web,
            scan_now_requested: false,
        }
    }
}

impl StartupState {
    fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join(STARTUP_STATE_FILE)
    }

    /// 读取 `startup_state.json`；不存在或解析失败时返回默认值（不视为错误，让向导重新跑）。
    pub fn load(data_dir: &Path) -> StartupState {
        let _guard = STARTUP_STATE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        Self::load_unlocked(data_dir)
    }

    fn load_unlocked(data_dir: &Path) -> StartupState {
        let path = Self::path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str::<StartupState>(&raw).unwrap_or_else(|error| {
                tracing::warn!(%error, file = %path.display(), "startup_state.json 解析失败，使用默认值");
                StartupState::default()
            }),
            Err(_) => StartupState::default(),
        }
    }

    fn save(&self, data_dir: &Path) -> Result<()> {
        let _guard = STARTUP_STATE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        self.save_unlocked(data_dir)
    }

    fn save_unlocked(&self, data_dir: &Path) -> Result<()> {
        let mut next = self.clone();
        next.last_modified = Utc::now().to_rfc3339();
        let raw = serde_json::to_string_pretty(&next)?;
        let path = Self::path(data_dir);
        let temp = data_dir.join(format!(
            ".startup_state.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&temp, raw)
            .with_context(|| format!("写入 startup_state.json 失败: {}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("重命名 startup_state.json 失败: {}", path.display()))?;
        Ok(())
    }

    /// 仅更新 AI 开关（供 `ai on|off` 命令调用，避免重写整份向导状态）。
    pub fn save_ai_flag(data_dir: &Path, enabled: bool) -> Result<()> {
        Self::update(data_dir, |state| state.ai_skill_enabled = enabled)
    }

    fn update(data_dir: &Path, update: impl FnOnce(&mut StartupState)) -> Result<()> {
        let _guard = STARTUP_STATE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let mut state = Self::load_unlocked(data_dir);
        update(&mut state);
        state.save_unlocked(data_dir)
    }

    pub fn save_terminal_mode(data_dir: &Path, mode: TerminalMode) -> Result<()> {
        Self::update(data_dir, |state| state.terminal_mode = mode)
    }

    pub fn mark_completed(data_dir: &Path) -> Result<()> {
        Self::update(data_dir, |state| state.onboarding_completed = true)
    }
}

/*
 * Keep all startup-state read/modify/write operations under one process lock.
 * ponytail: a global lock is sufficient because this file is tiny and writes are rare.
 */
/*
        let mut state = Self::load(data_dir);
        state.ai_skill_enabled = enabled;
        state.save(data_dir)
    }

    /// 仅更新终端模式（供 `mode` 命令调用）。
    pub fn save_terminal_mode(data_dir: &Path, mode: TerminalMode) -> Result<()> {
        let mut state = Self::load(data_dir);
        state.terminal_mode = mode;
        state.save(data_dir)
    }

    /// 标记 onboarding 完成（供 Web Setup 向导 API 调用）。
    pub fn mark_completed(data_dir: &Path) -> Result<()> {
        let mut state = Self::load(data_dir);
        state.onboarding_completed = true;
        state.save(data_dir)
    }
*/

/// 运行 onboarding。首次启动打印 Setup URL 并自动打开浏览器，后续启动显示状态摘要并自动打开浏览器。
///
/// 返回的 `StartupState` 带瞬态 `scan_now_requested`（始终为 false，扫码登录改由 Web 端处理）。
/// `setup_port` 和 `main_port` 由调用方在服务器启动后传入，确保显示和打开浏览器的端口准确。
pub async fn run(
    paths: &AppPaths,
    interactive: bool,
    setup_port: u16,
    _main_port: u16,
) -> Result<StartupState> {
    let state = StartupState::load(&paths.data_dir);
    if !interactive {
        return Ok(state);
    }

    if !state.onboarding_completed {
        // 首次启动：打印 Setup URL + 自动打开浏览器
        print_first_launch(setup_port);
        open_browser_safe(&format!("http://127.0.0.1:{setup_port}"));
    }
    // 后续启动：Banner 和浏览器打开由 main.rs 在端口绑定成功后执行，
    // 确保浏览器打开时服务器已就绪。

    Ok(state)
}

/// 首次启动终端输出：打印 Setup URL + 提示。
fn print_first_launch(port: u16) {
    use crate::app::term_style;
    let setup_url = format!("http://127.0.0.1:{port}");
    println!("═══════════════════════════════════════════════════════");
    println!("  补哩补哩 bulibuli v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("  请在浏览器中完成初始设置：");
    println!("  → {}", term_style::url(&setup_url));
    println!();
    println!("  {}", term_style::dim("正在自动打开浏览器..."));
    println!(
        "  {}",
        term_style::dim("如果未能自动打开，请手动复制上面的地址到浏览器")
    );
    println!();
    println!("  {}", term_style::dim("Ctrl+C 停止服务"));
    println!("═══════════════════════════════════════════════════════");
}

/// 自动打开浏览器；无图形桌面或打开失败时静默降级（终端仍显示 URL）。
pub fn open_browser_safe(url: &str) {
    if !browser_available() {
        return;
    }
    if let Err(error) = open::that(url) {
        tracing::debug!(%error, "自动打开浏览器失败，用户需手动复制 URL");
    }
}

fn browser_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// 主流程在 BiliApi 就绪后调用：若用户在向导里选了"现在扫码"，则拉取二维码 URL 并轮询直到完成/超时。
///
/// 失败/超时不阻塞启动（符合规划：登录失败仍允许启动，AI 调用时返回 BILI_NOT_LOGGED_IN）。
/// 成功后回写 `startup_state.json` 的 `bili_logged_in_uid`，供下次启动摘要展示。
pub async fn run_qr_login(state: &SharedState, paths: &AppPaths) {
    if let Err(error) = run_qr_login_inner(state, paths).await {
        eprintln!(
            "{}",
            crate::app::term_style::error(&format!(
                "扫码登录失败：{error:#}（不阻塞启动，可稍后用前端网页登录）"
            ))
        );
    }
}

async fn run_qr_login_inner(state: &SharedState, paths: &AppPaths) -> Result<()> {
    use crate::app::term_style;
    let qrcode = state
        .bili
        .bili_api
        .get_qrcode_url()
        .await
        .context("获取扫码登录二维码失败")?;
    println!("\n—— B 站扫码登录 ——");
    println!("请用 B 站 App 扫描以下 URL 对应的二维码（在浏览器打开该链接即可显示二维码图片）：");
    println!("  {}", term_style::url(&qrcode.url));
    println!("（或启动完成后，用前端网页 /settings 扫码）");
    println!("正在等待扫码确认...（最多 180 秒）\n");

    let key = qrcode.qrcode_key.clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        if std::time::Instant::now() >= deadline {
            println!(
                "{}",
                term_style::warn("扫码登录超时，已跳过。可稍后用前端网页登录。")
            );
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let poll = match state.bili.bili_api.check_qrcode_status(&key).await {
            Ok(p) => p,
            Err(error) => {
                tracing::warn!(%error, "轮询扫码状态失败，稍后重试");
                continue;
            }
        };
        match poll.code {
            0 => {
                if let Some(cookies) = poll.cookies.as_deref().filter(|c| !c.trim().is_empty()) {
                    let nav = state
                        .bili
                        .bili_api
                        .get_nav_info(cookies)
                        .await
                        .context("扫码凭证校验失败")?;
                    if !nav.is_login {
                        anyhow::bail!("扫码凭证未通过 B 站登录校验");
                    }
                    state
                        .infra
                        .settings_service
                        .save_cookie_header(cookies)
                        .await
                        .context("保存扫码登录凭证失败")?;
                    state.bili.bili_api.invalidate_session_caches().await;
                }
                println!("{}", term_style::ok("✓ 扫码登录成功"));
                refresh_bili_uid(state, paths).await;
                return Ok(());
            }
            86090 => println!("{}", term_style::ok("已扫描，请在 B 站 App 确认登录...")),
            86038 => println!(
                "{}",
                term_style::warn("二维码已失效，请稍后用前端网页重新扫码。")
            ),
            code if code < 0 => {
                println!(
                    "{}",
                    term_style::error(&format!("扫码登录失败：{}", poll.message))
                );
                return Ok(());
            }
            _ => {}
        }
    }
}

/// 登录成功后查询当前 UID 并回写 startup_state.json，供下次启动摘要展示。
async fn refresh_bili_uid(state: &SharedState, paths: &AppPaths) {
    let cookies = state
        .infra
        .settings_service
        .cookie_header()
        .await
        .unwrap_or_default();
    if !Credential::from_cookie_header(&cookies).is_logged_in() {
        return;
    }
    if let Ok(nav) = state.bili.bili_api.get_nav_info(&cookies).await {
        if nav.is_login && nav.mid > 0 {
            let mut s = StartupState::load(&paths.data_dir);
            s.bili_logged_in_uid = Some(nav.mid);
            let _ = s.save(&paths.data_dir);
        }
    }
}

/// 主流程在 AppState 就绪后调用：刷新 B 站登录 UID 到 startup_state.json（无论是否扫码，确保摘要准确）。
pub async fn sync_bili_uid(state: &SharedState, paths: &AppPaths) {
    let cookies = state
        .infra
        .settings_service
        .cookie_header()
        .await
        .unwrap_or_default();
    if !Credential::from_cookie_header(&cookies).is_logged_in() {
        return;
    }
    if let Ok(nav) = state.bili.bili_api.get_nav_info(&cookies).await {
        if nav.is_login && nav.mid > 0 {
            let mut s = StartupState::load(&paths.data_dir);
            if s.bili_logged_in_uid != Some(nav.mid) {
                s.bili_logged_in_uid = Some(nav.mid);
                let _ = s.save(&paths.data_dir);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_not_onboarded() {
        let s = StartupState::default();
        assert!(!s.onboarding_completed);
        assert!(!s.ai_skill_enabled);
        assert!(s.bili_logged_in_uid.is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = StartupState {
            onboarding_completed: true,
            ai_skill_enabled: true,
            bili_logged_in_uid: Some(12345),
            ..StartupState::default()
        };
        state.save(dir.path()).unwrap();
        let loaded = StartupState::load(dir.path());
        assert!(loaded.onboarding_completed);
        assert!(loaded.ai_skill_enabled);
        assert_eq!(loaded.bili_logged_in_uid, Some(12345));
    }

    #[test]
    fn save_ai_flag_preserves_other_fields() {
        let dir = tempfile::tempdir().unwrap();
        let state = StartupState {
            onboarding_completed: true,
            bili_logged_in_uid: Some(99),
            ..StartupState::default()
        };
        state.save(dir.path()).unwrap();
        StartupState::save_ai_flag(dir.path(), true).unwrap();
        let loaded = StartupState::load(dir.path());
        assert!(loaded.onboarding_completed);
        assert!(loaded.ai_skill_enabled);
        assert_eq!(loaded.bili_logged_in_uid, Some(99));
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = StartupState::load(dir.path());
        assert!(!loaded.onboarding_completed);
    }

    #[test]
    fn scan_now_requested_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let state = StartupState {
            onboarding_completed: true,
            scan_now_requested: true,
            ..StartupState::default()
        };
        state.save(dir.path()).unwrap();
        let loaded = StartupState::load(dir.path());
        assert!(!loaded.scan_now_requested);
    }
}
