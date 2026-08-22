//! API 契约集成测试：组装完整 AppState + 安全中间件 Router，
//! 监听回环随机端口，用真实 HTTP 客户端验证认证流程、RBAC 与错误信封。
//!
//! 与 `app::security_server::role_tests`（纯函数级 RBAC 单测）互补：
//! 这里覆盖中间件 → 路由 → handler → 序列化的完整链路。

use super::{validate_bili_id, validate_fnval, MAX_BILI_ID};

use crate::config::{AppConfig, AppPaths};
use crate::db::init_database;
use crate::state::{AppState, SharedState};
use axum::Router;
use reqwest::header::{COOKIE, ORIGIN, SET_COOKIE};
use reqwest::{Client, StatusCode};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::OnceLock;
use tempfile::TempDir;

/// 保留原 mod.rs 内联测试：参数校验器的边界行为。
#[test]
fn validates_positive_ids_and_fnval() {
    assert!(validate_bili_id("UID", 1).is_ok());
    assert!(validate_bili_id("UID", 0).is_err());
    assert!(validate_bili_id("UID", MAX_BILI_ID + 1).is_err());
    assert!(validate_fnval(4048).is_ok());
    assert!(validate_fnval(-1).is_err());
}

/// reqwest 的 rustls-no-provider 需要进程内安装一次 CryptoProvider。
fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

struct TestServer {
    _temp: TempDir,
    state: SharedState,
    base_url: String,
    /// 首次启动自动生成的 Owner 配对码（10 分钟有效，一次性）。
    initial_pair_code: String,
}

/// 组装与 `main.rs` 相同的完整状态链（Infra → Bili → Media → Business），
/// 但保留首次配对码供测试配对 Owner 会话（`AppState::new` 会丢弃它）。
async fn build_state(temp: &TempDir) -> anyhow::Result<(SharedState, String)> {
    let config = AppConfig::default();
    let data_dir = temp.path().join("data");
    let paths = AppPaths {
        app_root: temp.path().to_path_buf(),
        data_dir: data_dir.clone(),
        database_dir: data_dir.join("database"),
        download_dir: data_dir.join("downloads"),
    };
    // 生产路径由 load_config 创建子目录；测试需自行准备。
    std::fs::create_dir_all(&paths.database_dir).expect("create database dir");
    std::fs::create_dir_all(&paths.download_dir).expect("create download dir");
    let db: DatabaseConnection = init_database(&paths, &config).await?;
    let (infra, secret_store) =
        crate::state::infra::InfraState::build(config.clone(), paths.clone(), db.clone(), false)
            .await?;
    let (bili, initial_pair_code) =
        crate::state::bili::BiliState::build(&infra, secret_store).await?;
    let media = crate::state::media::MediaState::build(&infra, &bili).await?;
    let business = crate::state::business::BusinessState::build(&infra, &bili, &media).await?;
    let initial_pair_code =
        initial_pair_code.expect("全新数据库 bootstrap 必须生成首次 Owner 配对码");
    let state = std::sync::Arc::new(AppState {
        infra: std::sync::Arc::new(infra),
        bili: std::sync::Arc::new(bili),
        media: std::sync::Arc::new(media),
        business: std::sync::Arc::new(business),
    });
    Ok((state, initial_pair_code))
}

async fn spawn_server() -> TestServer {
    install_crypto_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    // build_router 会读取 static/app/index.html 作为主界面产物。
    let static_app = temp.path().join("static").join("app");
    std::fs::create_dir_all(&static_app).expect("create static/app");
    std::fs::write(
        static_app.join("index.html"),
        "<html><body>test</body></html>",
    )
    .expect("write index.html");

    let (state, initial_pair_code) = build_state(&temp).await.expect("build app state");
    let router: Router = crate::app::server::build_router(state.clone())
        .await
        .expect("build router");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve test router");
    });
    TestServer {
        _temp: temp,
        state,
        base_url: format!("http://127.0.0.1:{port}"),
        initial_pair_code,
    }
}

/// 断言响应体为标准错误信封 {code, message, data} 且 code 与 HTTP 状态一致。
#[track_caller]
fn assert_error_envelope(status: StatusCode, body: &Value) {
    assert_eq!(
        body.get("code").and_then(Value::as_i64),
        Some(status.as_u16() as i64),
        "信封 code 应与 HTTP 状态一致: {body}"
    );
    assert!(
        body.get("message").and_then(Value::as_str).is_some(),
        "信封缺少 message 字符串: {body}"
    );
    assert!(
        body.get("data").is_some(),
        "信封缺少 data 字段（错误时为 null）: {body}"
    );
}

/// 用给定配对码走真实 HTTP 配对流程，返回会话 Cookie 头值。
async fn pair_via_http(client: &Client, server: &TestServer, code: &str) -> String {
    let response = client
        .post(format!("{}/api/auth/pair", server.base_url))
        .header(ORIGIN, &server.base_url)
        .json(&json!({"code": code, "device_name": "contract-test"}))
        .send()
        .await
        .expect("pair request");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "配对应成功: {}",
        response.text().await.expect("body")
    );
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| {
            let raw = value.to_str().expect("set-cookie header");
            raw.split(';').next().map(str::to_string)
        })
        .expect("配对响应应携带会话 Cookie")
}

/// 直接向 auth_sessions 插入指定角色的会话行（用于构造 Viewer 会话，
/// 服务层只暴露 Owner/Operator 邀请入口）。
async fn insert_session_with_role(db: &DatabaseConnection, token: &str, role: &str) {
    use sha2::{Digest, Sha256};
    let now = chrono::Utc::now().timestamp();
    let hash: Vec<u8> = Sha256::digest(token.as_bytes()).to_vec();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO auth_sessions (
           id, token_hash, csrf_token, device_name, created_at, expires_at,
           absolute_expires_at, last_used_at, last_rotated_at, last_ip,
           user_agent_summary, role
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            .to_string(),
        [
            sea_orm::Value::from(format!("test-{role}")),
            sea_orm::Value::from(hash),
            sea_orm::Value::from(format!("csrf-{role}")),
            sea_orm::Value::from("contract-test"),
            sea_orm::Value::from(now),
            sea_orm::Value::from(now + 3600),
            sea_orm::Value::from(now + 3600),
            sea_orm::Value::from(now),
            sea_orm::Value::from(now),
            sea_orm::Value::from("127.0.0.1"),
            sea_orm::Value::from("contract-test"),
            sea_orm::Value::from(role),
        ],
    ))
    .await
    .expect("insert viewer session");
}

#[tokio::test]
async fn auth_flow_contract() {
    let server = spawn_server().await;
    let client = Client::new();

    // 未登录：/api/auth/state 公开且不含任何 csrf_token。
    let response = client
        .get(format!("{}/api/auth/state", server.base_url))
        .send()
        .await
        .expect("auth state");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["code"], 0, "成功信封 code=0: {body}");
    assert_eq!(body["data"]["authenticated"], false);
    assert_eq!(body["data"]["role"], Value::Null);
    assert!(
        !body.to_string().to_lowercase().contains("csrf_token"),
        "公开的 /api/auth/state 不得暴露 csrf_token: {body}"
    );

    // 未登录访问需会话的 /api/auth/csrf：401 + 标准错误信封。
    let response = client
        .get(format!("{}/api/auth/csrf", server.base_url))
        .send()
        .await
        .expect("auth csrf");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.expect("json body");
    assert_error_envelope(StatusCode::UNAUTHORIZED, &body);

    // 用首次配对码完成 Owner 配对，拿到会话 Cookie。
    let cookie = pair_via_http(&client, &server, &server.initial_pair_code).await;
    assert!(
        cookie.starts_with("bili-session=") && cookie.len() > "bili-session=".len(),
        "会话 Cookie 格式异常: {cookie}"
    );

    // 登录后：state 显示 owner，csrf 端点返回非空 token。
    let response = client
        .get(format!("{}/api/auth/state", server.base_url))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("auth state");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["data"]["authenticated"], true);
    assert_eq!(body["data"]["role"], "owner");

    let response = client
        .get(format!("{}/api/auth/csrf", server.base_url))
        .header(COOKIE, &cookie)
        .send()
        .await
        .expect("auth csrf");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["code"], 0);
    let csrf = body["data"]["csrf_token"].as_str().expect("csrf token");
    assert!(!csrf.is_empty());

    // 未登录访问非公开业务路径：401。
    let response = client
        .get(format!("{}/api/bloggers", server.base_url))
        .send()
        .await
        .expect("bloggers");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.expect("json body");
    assert_error_envelope(StatusCode::UNAUTHORIZED, &body);
}

#[tokio::test]
async fn backup_is_owner_only_and_rbac_enforced() {
    let server = spawn_server().await;
    let client = Client::new();

    // Owner 配对并取 CSRF。
    let owner_cookie = pair_via_http(&client, &server, &server.initial_pair_code).await;
    let csrf: String = {
        let body: Value = client
            .get(format!("{}/api/auth/csrf", server.base_url))
            .header(COOKIE, &owner_cookie)
            .send()
            .await
            .expect("csrf")
            .json()
            .await
            .expect("json");
        body["data"]["csrf_token"].as_str().expect("csrf").into()
    };

    // Owner POST /api/backup：成功并真实产出备份文件。
    let response = client
        .post(format!("{}/api/backup", server.base_url))
        .header(COOKIE, &owner_cookie)
        .header(ORIGIN, &server.base_url)
        .header("x-csrf-token", &csrf)
        .send()
        .await
        .expect("backup");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("json body");
    assert_eq!(body["code"], 0, "{body}");
    let file = body["data"]["file"].as_str().expect("backup file name");
    assert!(file.starts_with("bulibuli-backup-") && file.ends_with(".db"));
    assert!(
        server
            .state
            .infra
            .paths
            .data_dir
            .join("backups")
            .join(file)
            .is_file(),
        "备份文件应真实存在"
    );

    // Operator 会话：经 Owner 邀请 + HTTP 配对创建。
    let invitation = server.state.bili.auth.open_operator_invitation().await;
    let operator_cookie = pair_via_http(&client, &server, &invitation).await;
    let response = client
        .post(format!("{}/api/backup", server.base_url))
        .header(COOKIE, &operator_cookie)
        .header(ORIGIN, &server.base_url)
        .header("x-csrf-token", "unused-owner-only")
        .send()
        .await
        .expect("backup as operator");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = response.json().await.expect("json body");
    assert_error_envelope(StatusCode::FORBIDDEN, &body);

    // Viewer 会话：直接插入会话行。写操作即使 CSRF 正确也被 RBAC 拒绝。
    let viewer_token = "viewer-session-token-for-contract-test";
    insert_session_with_role(&server.state.infra.db, viewer_token, "viewer").await;
    let viewer_cookie = format!("bili-session={viewer_token}");
    let response = client
        .post(format!("{}/api/download/pause", server.base_url))
        .header(COOKIE, &viewer_cookie)
        .header(ORIGIN, &server.base_url)
        .header("x-csrf-token", "csrf-viewer")
        .send()
        .await
        .expect("pause as viewer");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = response.json().await.expect("json body");
    assert_error_envelope(StatusCode::FORBIDDEN, &body);

    // 对照：Operator 对同一业务写路径放行（到达 handler；无任务时不 500 即可）。
    let operator_csrf: String = {
        let body: Value = client
            .get(format!("{}/api/auth/csrf", server.base_url))
            .header(COOKIE, &operator_cookie)
            .send()
            .await
            .expect("operator csrf")
            .json()
            .await
            .expect("json");
        body["data"]["csrf_token"].as_str().expect("csrf").into()
    };
    let response = client
        .post(format!("{}/api/download/pause", server.base_url))
        .header(COOKIE, &operator_cookie)
        .header(ORIGIN, &server.base_url)
        .header("x-csrf-token", &operator_csrf)
        .send()
        .await
        .expect("pause as operator");
    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Operator 不应被 RBAC 拦截"
    );
}
