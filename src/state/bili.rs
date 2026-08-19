//! B 站 API 领域状态：BiliApi、资源代理客户端、认证、安全配置、校验服务。

use crate::services::auth::AuthService;
use crate::services::bili_api::BiliApi;
use crate::services::cookie_manager::CookieManager;
use crate::services::security_config::SecurityConfigService;
use crate::services::verify::VerifyService;
use crate::state::infra::InfraState;
use anyhow::Context;
use std::sync::Arc;

/// B 站 API 领域：封装与 B 站交互所需的全部服务。
#[derive(Clone)]
pub struct BiliState {
    pub(crate) bili_api: Arc<BiliApi>,
    /// CookieManager 的 clone（内部 Arc，零开销），供 MediaState 构建 DanmakuService 使用。
    pub(crate) cookie_manager: Arc<CookieManager>,
    pub(crate) resource_client: reqwest::Client,
    pub(crate) auth: Arc<AuthService>,
    pub(crate) security: Arc<SecurityConfigService>,
    pub(crate) verify_service: Arc<VerifyService>,
}

impl BiliState {
    /// 构建 B 站 API 领域。
    ///
    /// 依赖 `InfraState` 提供的 config / paths / db / ws / settings_service / cancellation，
    /// 以及上游产出的 `secret_store`。
    pub(crate) async fn build(
        infra: &InfraState,
        secret_store: Arc<crate::services::secret_store::SecretStore>,
    ) -> anyhow::Result<Self> {
        let security = Arc::new(
            SecurityConfigService::load(&infra.paths.data_dir, &infra.paths.app_root)
                .context("初始化安全配置失败")?,
        );

        let (auth_service, initial_pair_code) = AuthService::new(
            infra.db.clone(),
            security.clone(),
            infra.config.login_rate_limit_global,
        )
        .await
        .context("初始化认证服务失败")?;
        let auth = Arc::new(auth_service);

        if let Some(code) = initial_pair_code {
            // 配对码仅在终端打印；如果 nohup/systemd 等场景下终端不可见，需重启程序重新生成。
            crate::app::tui::console_line(format!(
                "首次设备配对码：{}-{}（10 分钟内有效，仅可使用一次）",
                &code[..4],
                &code[4..]
            ));
        }

        let cookie_manager = Arc::new(
            CookieManager::new(
                secret_store,
                infra.config.user_agent.clone(),
                infra.config.referer.clone(),
                infra.config.bili_api_timeout,
            )
            .context("初始化 CookieManager 失败")?,
        );

        let bili_api = Arc::new(
            BiliApi::new(
                infra.config.clone(),
                cookie_manager.clone(),
                infra.ws.clone(),
            )
            .context("初始化 BiliApi 失败")?,
        );

        let resource_client = reqwest::Client::builder()
            // 资源代理在 authenticated=true 时会携带用户 Cookie，
            // 必须严格校验 TLS，防止凭据在传输中被中间人窃取。
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .danger_accept_invalid_certs(false)
            .build()
            .context("初始化资源代理 HTTP 客户端失败")?;

        let verify_service = Arc::new(VerifyService::new(
            infra.db.clone(),
            infra.settings_service.clone(),
            infra.cancellation.child_token(),
        ));

        Ok(Self {
            bili_api,
            cookie_manager,
            resource_client,
            auth,
            security,
            verify_service,
        })
    }
}
