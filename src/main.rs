//! 二进制入口:解析环境变量、初始化数据库与 `AppState`、组装路由并启动 HTTP 服务。
//! 环境变量的权威说明见 `docs/deploy.md`。

use std::{
    error::Error,
    io,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use ipnet::IpNet;
use mihomo_subscription::{
    app::{build_router, AppState},
    auth::{AdminAuth, SessionStore, SESSION_IDLE},
    db,
    fetch::{HttpFetcher, DEFAULT_USER_AGENT},
    keyed_lock::KeyedLock,
    rate_limit::RateLimiter,
};

type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::main]
async fn main() -> AppResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mihomo_subscription=info,tower_http=info".into()),
        )
        .init();

    // 管理员凭据是必需的;缺失则拒绝启动。
    let admin_username = require_env("ADMIN_USERNAME")?;
    let admin_password = require_env("ADMIN_PASSWORD")?;

    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".to_string());
    std::fs::create_dir_all(&data_dir)?;
    let db_path = format!("{data_dir}/mihomo-subscription.db");

    let pool = db::init(&db_path).await?;
    let public_path_prefix =
        db::ensure_public_path_prefix(&pool, std::env::var("PUBLIC_PATH_PREFIX").ok()).await?;
    tracing::info!("Database ready at {db_path}");

    let public_base_url = std::env::var("PUBLIC_BASE_URL").unwrap_or_default();
    // 设置 Secure cookie 属性。`SECURE_COOKIES` 是显式覆盖项;未设时由 HTTPS 公共源推断。在
    // TLS 终止反向代理后,应用走纯 HTTP,故若无此覆盖,缺失或 http 的 `PUBLIC_BASE_URL` 会静默
    // 签发不带 `Secure` 的会话 cookie,使其暴露于明文传输。
    let secure_cookies = env_bool("SECURE_COOKIES", public_base_url.starts_with("https://"));
    if !secure_cookies {
        tracing::warn!(
            "session cookies will be issued WITHOUT the Secure attribute; set \
             SECURE_COOKIES=true (or an https:// PUBLIC_BASE_URL) when serving over HTTPS"
        );
    }
    let web_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web/dist".to_string());

    let fetch_timeout = Duration::from_secs(env_u64("FETCH_TIMEOUT_SECONDS", 15));
    let max_bytes = env_u64("MAX_SUBSCRIPTION_SIZE_MB", 8) as usize * 1024 * 1024;
    let cache_ttl = Duration::from_secs(env_u64("CACHE_TTL_MINUTES", 15) * 60);
    let public_refresh_min_interval =
        Duration::from_secs(env_u64("PUBLIC_REFRESH_MIN_SECONDS", 30));
    let fetch_user_agent = std::env::var("FETCH_USER_AGENT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
    let trusted_proxy_hops = env_u64("TRUSTED_PROXY_HOPS", 0) as usize;
    let trusted_proxy_cidrs = trusted_proxy_cidrs()?;
    if trusted_proxy_hops > 0 && trusted_proxy_cidrs.is_empty() {
        tracing::warn!(
            "TRUSTED_PROXY_HOPS is set but TRUSTED_PROXY_CIDRS is empty; ignoring \
             X-Forwarded-For and using the TCP peer for rate limiting"
        );
    }

    let state = Arc::new(AppState {
        db: pool,
        public_base_url,
        public_path_prefix: Arc::new(RwLock::new(public_path_prefix)),
        admin: AdminAuth::new(&admin_username, &admin_password),
        sessions: SessionStore::new(SESSION_IDLE),
        secure_cookies,
        web_dir,
        fetcher: Arc::new(HttpFetcher {
            timeout: fetch_timeout,
            max_bytes,
            user_agent: fetch_user_agent,
        }),
        cache_ttl,
        public_refresh_min_interval,
        keyed_lock: KeyedLock::new(),
        trusted_proxy_hops,
        trusted_proxy_cidrs,
        login_limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(60))),
        download_limiter: Arc::new(RateLimiter::new(120, Duration::from_secs(60))),
    });

    let app = build_router(state);

    // 非法 PORT 拒绝启动(与 TRUSTED_PROXY_CIDRS 同一严格度),而非静默回退 8080。
    let port: u16 = match std::env::var("PORT") {
        Ok(raw) => raw.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("PORT is not a valid port number: {raw}"),
            )
        })?,
        Err(_) => 8080,
    };
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Listening on http://{}", listener.local_addr()?);
    // Connect info 暴露 TCP 对端地址,用于客户端 IP 推导。
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// 解析数值环境变量;未设或非法时回退到 `default`。安全相关的项(如 `TRUSTED_PROXY_HOPS`)
/// 也走这里,因为其默认值即最安全取值(hops 回退 0 = 完全不信任 XFF)。
fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// 解析布尔环境变量(`true`/`false`/`1`/`0`,不区分大小写);未设或无法识别时回退到 `default`。
fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

/// 解析 `TRUSTED_PROXY_CIDRS`(逗号分隔的 CIDR 列表)。任一条目非法即拒绝启动:静默跳过
/// 会让管理员误以为 XFF 信任边界已收窄,实际却放开了伪造来源(见 `src/net.rs`)。
fn trusted_proxy_cidrs() -> io::Result<Vec<IpNet>> {
    let Some(raw) = std::env::var("TRUSTED_PROXY_CIDRS")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(Vec::new());
    };

    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<IpNet>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("TRUSTED_PROXY_CIDRS contains an invalid CIDR: {s}"),
                )
            })
        })
        .collect()
}

/// 读取必填环境变量;缺失或为空即拒绝启动,不给默认值(避免弱凭据上线)。
fn require_env(key: &str) -> io::Result<String> {
    let value = std::env::var(key).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{key} must be set (configured via the 1Panel install form)"),
        )
    })?;
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{key} must not be empty"),
        ));
    }
    Ok(value)
}
