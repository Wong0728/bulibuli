//! aria2c 子进程生命周期：构造、启动、定位可执行文件与优雅关停。

use crate::config::{AppConfig, AppPaths};
use crate::services::settings::{Aria2BasicSettings, RuntimeSettings};
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::{rpc_endpoint, Aria2Inner, Aria2Manager, Aria2Mode, DEFAULT_ARIA2_PORT};

impl Aria2Manager {
    pub fn new(paths: Arc<AppPaths>, _config: &AppConfig) -> Result<Self> {
        Ok(Self {
            // Aria2 RPC 通信发生在本机或用户指定的外部主机，必须严格校验 TLS，
            // 防止 RPC secret 与管理指令在传输中被中间人截获/篡改。
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .context("创建 Aria2 HTTP 客户端失败")?,
            paths,
            inner: Arc::new(Mutex::new(Aria2Inner {
                port: DEFAULT_ARIA2_PORT,
                secret: String::new(),
                mode: Aria2Mode::Embedded,
                child: None,
                rpc_url: format!("http://localhost:{DEFAULT_ARIA2_PORT}/jsonrpc"),
                available_cache: None,
                started_at: None,
                ready: false,
                last_error: None,
                #[cfg(windows)]
                job: None,
            })),
        })
    }

    pub async fn init(&self, settings: &RuntimeSettings) -> Result<()> {
        let has_managed_child = self.inner.lock().await.child.is_some();
        if has_managed_child {
            self.stop().await?;
        }
        let mode = Aria2Mode::from_str(&settings.download_mode.mode);
        let host = &settings.aria2_rpc.host;
        let port = settings.aria2_rpc.port;
        let configured_secret = settings.aria2_rpc.secret.clone();
        // 内置/系统模式默认生成仅本次进程使用的 RPC 密钥，避免无鉴权警告；
        // 外部模式必须严格使用用户提供的密钥。
        let secret = if matches!(mode, Aria2Mode::Embedded | Aria2Mode::System)
            && configured_secret.is_empty()
        {
            uuid::Uuid::new_v4().simple().to_string()
        } else {
            configured_secret
        };

        // 外部 RPC 模式：用户提供的 secret 若太短则给出安全警告（不阻断，避免破坏现有配置）。
        if matches!(mode, Aria2Mode::External) && !secret.is_empty() && secret.len() < 16 {
            warn!(
                "外部 Aria2 RPC secret 长度仅 {}，建议至少 16 位以降低暴力破解风险",
                secret.len()
            );
        }

        let endpoint = rpc_endpoint(host, port)?;
        {
            let mut inner = self.inner.lock().await;
            inner.mode = mode;
            inner.port = port;
            inner.secret = secret.clone();
            inner.rpc_url = endpoint;
            inner.available_cache = None;
            inner.started_at = Some(std::time::Instant::now());
            inner.ready = false;
            inner.last_error = None;
        }

        match mode {
            Aria2Mode::Embedded | Aria2Mode::System => {
                info!("使用 {} 模式启动 Aria2...", mode.as_str());
                if let Err(error) = self
                    .start_aria2c(
                        port,
                        &settings.aria2c_basic,
                        settings.parallel_download.max_parallel,
                        &secret,
                    )
                    .await
                {
                    let mut inner = self.inner.lock().await;
                    inner.last_error = Some(error.to_string());
                    return Err(error);
                }
            }
            Aria2Mode::External => {
                info!("RPC 模式：检查外部 Aria2 服务...");
            }
        }
        Ok(())
    }

    async fn start_aria2c(
        &self,
        port: u16,
        basic: &Aria2BasicSettings,
        max_parallel: usize,
        secret: &str,
    ) -> Result<()> {
        let aria2c = self.find_aria2c()?;
        std::fs::create_dir_all(&self.paths.download_dir)?;
        std::fs::create_dir_all(&self.paths.data_dir)?;
        let log = self.paths.data_dir.join("aria2.log");
        // 不再启用 aria2 的 session 持久化（--save-session / --input-file）：
        // session 文件会把下载项的 header（含用户 Cookie）每 30s 明文落盘，
        // 与 DB 侧 AES-GCM 加密 Cookie 的威胁模型不一致。删掉旧版残留的
        // session 文件（内含明文 Cookie）。重启后的任务恢复由
        // DownloadManager::resume_pending_tasks 完成：GID 失效时用当前登录态
        // 重新解析 URL 并重加任务（CDN URL 本就约 2h 过期，重解析比复用
        // session 中的旧 URL 更可靠），字节级断点续传仍由下载文件旁的
        // .aria2 控制文件（--continue）保证，与 session 文件无关。
        let legacy_session = self.paths.data_dir.join("aria2.session");
        if legacy_session.exists() {
            if let Err(e) = std::fs::remove_file(&legacy_session) {
                warn!("清理旧 aria2.session（含明文 Cookie）失败: {e}");
            }
        }

        let mut cmd = Command::new(aria2c);
        cmd.arg("--enable-rpc")
            .arg(format!("--rpc-listen-port={port}"))
            .arg("--rpc-allow-origin-all=false")
            .arg("--rpc-listen-all=false")
            .arg(format!(
                "--dir={}",
                self.paths.download_dir.to_string_lossy()
            ))
            .arg(format!("--log={}", log.to_string_lossy()))
            .arg("--log-level=warn")
            // aria2 可能携带用户 Cookie，凭据下载始终严格校验证书。
            .arg("--check-certificate=true")
            // `--timeout` 是单连接停滞超时，不是整个文件的下载时限。
            .arg("--timeout=30")
            .arg("--connect-timeout=20")
            .arg(format!("--max-tries={}", basic.max_tries))
            .arg(format!("--retry-wait={}", basic.retry_wait))
            .arg("--lowest-speed-limit=1K")
            // 并发控制
            .arg(format!("--max-concurrent-downloads={max_parallel}"))
            .arg("--continue=true")
            .arg("--auto-file-renaming=false")
            .arg("--allow-overwrite=true")
            // 使用系统 DNS，避免硬编码公共 DNS 在中国网络、企业网络或离线环境中失效。
            .arg("--async-dns=true")
            // 分片策略优化
            .arg("--stream-piece-selector=geom")
            .arg(format!("--split={}", basic.split))
            .arg(format!(
                "--max-connection-per-server={}",
                basic.max_connection_per_server
            ))
            .arg(format!("--min-split-size={}", basic.min_split_size));
        if !secret.is_empty() {
            cmd.arg(format!("--rpc-secret={secret}"));
        }
        // 全局下载限速（"0" 表示不限速）
        if !basic.max_overall_download_limit.is_empty() && basic.max_overall_download_limit != "0" {
            cmd.arg(format!(
                "--max-overall-download-limit={}",
                basic.max_overall_download_limit
            ));
        }
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        configure_child_lifetime(&mut cmd);

        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let child = cmd.spawn().context("启动 aria2c 失败")?;

        // Windows：把 aria2c 绑到 kill-on-close 的 Job Object。
        // 这样本进程无论正常退出还是被强杀（IDE 停止/任务管理器/关窗口），
        // 系统都会连带结束 aria2c，避免其变孤儿继续占用下载目录/会话文件。
        #[cfg(windows)]
        {
            use super::win_job;
            let mut inner = self.inner.lock().await;
            if inner.job.is_none() {
                inner.job = win_job::create();
            }
            if let Some(job) = inner.job.as_ref() {
                if let Some(h) = child.raw_handle() {
                    if win_job::assign(job, h) {
                        info!("aria2c 已绑定到 Job Object（父进程退出即随之结束）");
                    } else {
                        warn!("将 aria2c 分配到 Job Object 失败，将依赖优雅退出清理");
                    }
                }
            }
            inner.child = Some(child);
            inner.started_at = Some(std::time::Instant::now());
        }
        #[cfg(not(windows))]
        {
            let mut inner = self.inner.lock().await;
            inner.child = Some(child);
            inner.started_at = Some(std::time::Instant::now());
        }

        // 轮询 RPC 最多 6 秒；就绪后立即返回。
        const POLL_INTERVAL: Duration = Duration::from_millis(200);
        const MAX_POLL_ATTEMPTS: u32 = 30;
        for attempt in 0..MAX_POLL_ATTEMPTS {
            // 启动探测必须绕过可用性缓存。
            if self.is_available_uncached().await {
                let mut inner = self.inner.lock().await;
                inner.ready = true;
                inner.last_error = None;
                inner.available_cache = Some((true, std::time::Instant::now()));
                info!("Aria2 已就绪 (端口 {port}, 等待 {} 次轮询)", attempt + 1);
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        // 轮询超时：只提示相对日志位置，避免把用户机器绝对路径写入日志/API。
        let message = format!(
            "Aria2 启动超时（{} 次轮询未就绪），请检查 data/{}",
            MAX_POLL_ATTEMPTS,
            log.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("aria2.log")
        );
        let child = {
            let mut inner = self.inner.lock().await;
            inner.last_error = Some(message.clone());
            inner.child.take()
        };
        if let Some(mut child) = child {
            if let Err(error) = child.kill().await {
                warn!("清理启动超时的 Aria2 进程失败: {error}");
            }
            if let Err(error) = child.wait().await {
                warn!("等待已终止 Aria2 进程失败: {error}");
            }
        }
        Err(anyhow!(message))
    }

    fn find_aria2c(&self) -> Result<PathBuf> {
        // 1. resources 目录（完整包优先使用包内版本）
        let binary_name = if cfg!(windows) {
            "aria2c.exe"
        } else {
            "aria2c"
        };
        let embedded = self.paths.app_root.join("resources").join(binary_name);
        if embedded.is_file() {
            return Ok(embedded);
        }
        // 2. 环境变量 ARIA2C_PATH
        if let Ok(val) = std::env::var("ARIA2C_PATH") {
            let p = PathBuf::from(&val);
            if p.is_file() {
                return Ok(p);
            }
            let candidate = p.join(binary_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        // 3. PATH
        if let Ok(path) = which::which("aria2c") {
            return Ok(path);
        }
        Err(anyhow!("找不到 aria2c 可执行文件"))
    }

    pub async fn stop(&self) -> Result<()> {
        // 先尝试通过 RPC 发送 aria2.shutdown 让 aria2c 优雅关闭
        // （保存会话、释放文件锁），而非直接 kill 导致文件残留占用
        // 外部 RPC 不由本应用管理，断开/重配时绝不能关闭用户的远端服务。
        let managed_process = self.inner.lock().await.mode != Aria2Mode::External;
        if managed_process && self.is_available_uncached().await {
            info!("向 Aria2 发送 shutdown RPC...");
            if let Err(error) = self.call("aria2.shutdown", vec![]).await {
                warn!("请求 aria2 优雅关闭失败: {error}");
            }
        }

        let child = {
            let mut inner = self.inner.lock().await;
            inner.child.take()
        };
        if let Some(mut child) = child {
            // 等待 aria2c 进程退出，最多 5 秒
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => info!("Aria2 进程已退出 (status={status})"),
                Ok(Err(e)) => warn!("等待 Aria2 进程退出失败: {e}"),
                Err(_) => {
                    // 超时，强制 kill
                    warn!("Aria2 优雅关闭超时，强制终止");
                    if let Err(error) = child.kill().await {
                        warn!("终止 aria2 子进程失败: {error}");
                    }
                    if let Err(error) = child.wait().await {
                        warn!("回收 aria2 子进程失败: {error}");
                    }
                }
            }
        }
        let mut inner = self.inner.lock().await;
        inner.available_cache = None;
        inner.started_at = None;
        inner.ready = false;
        Ok(())
    }
}

/// Linux 上设置父进程死亡信号。这样即使服务被 SIGKILL，aria2 也会收到 SIGTERM，
/// 不会在手工运行、nohup 或容器场景中长期遗留为孤儿进程。
#[cfg(target_os = "linux")]
fn configure_child_lifetime(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.as_std_mut().pre_exec(|| {
            unsafe extern "C" {
                fn prctl(option: i32, ...) -> i32;
            }
            const PR_SET_PDEATHSIG: i32 = 1;
            const SIGTERM: i32 = 15;
            if prctl(PR_SET_PDEATHSIG, SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_child_lifetime(_command: &mut Command) {}
