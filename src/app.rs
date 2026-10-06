//! 应用状态与 HTTP 路由组装,供二进制与集成测试共用。

use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderValue},
    middleware,
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use ipnet::IpNet;
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::auth::{self, AdminAuth, SessionStore};
use crate::fetch::RemoteFetcher;
use crate::keyed_lock::KeyedLock;
use crate::rate_limit::{self, RateLimiter};
use crate::{generate, global_nodes, profile_rule_sets, profiles, rule_sets, settings};

/// 管理请求体大小上限(见 `docs/architecture.md`「API 设计」)。
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    /// 外部可达的源,用于拼装托管链接。
    pub public_base_url: String,
    /// 全局公共路径前缀。用锁持有,因为 reset-public-path 会在运行时更新它
    /// (见 `docs/architecture.md`「安全设计」)。
    pub public_path_prefix: Arc<RwLock<String>>,
    pub admin: AdminAuth,
    pub sessions: SessionStore,
    /// 是否设置 `Secure` cookie 属性(HTTPS 提供时为 true)。
    pub secure_cookies: bool,
    /// 要提供的已构建 SPA 资产目录。
    pub web_dir: String,
    /// 机场获取器(生产中是真实的 SSRF 保护客户端)。
    pub fetcher: Arc<dyn RemoteFetcher>,
    /// 生成缓存的 TTL。
    pub cache_ttl: Duration,
    /// 公开订阅端点两次回源尝试之间的最小间隔(从完成时间算,成功/失败均计)。间隔内复用
    /// 最近缓存,无缓存则 503,避免泄露 token 后被单个客户端高频拉取放大为机场请求压力。
    pub public_refresh_min_interval: Duration,
    /// per-profile 的刷新合并。
    pub keyed_lock: KeyedLock,
    /// 推导客户端 IP 时信任的反向代理跳数。
    pub trusted_proxy_hops: usize,
    /// 允许提供可信 `X-Forwarded-For` 的直接 TCP 对端网段。
    pub trusted_proxy_cidrs: Vec<IpNet>,
    /// 登录尝试限流器(按客户端 IP)。
    pub login_limiter: Arc<RateLimiter>,
    /// 公开下载限流器(按客户端 IP;抑制枚举)。
    pub download_limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn current_prefix(&self) -> String {
        self.public_path_prefix.read().unwrap().clone()
    }

    /// 公开端点门:恒定时间比较 `candidate` 与当前公开路径前缀,且无论是否匹配都执行
    /// `lookup`、再合并判断,使响应时序无法单独确认路径前缀(见 `docs/architecture.md`)。
    pub async fn public_gate<T>(
        &self,
        candidate: &str,
        lookup: impl Future<Output = Option<T>>,
    ) -> Option<T> {
        let prefix_ok: bool = candidate
            .as_bytes()
            .ct_eq(self.current_prefix().as_bytes())
            .into();
        let found = lookup.await;
        // 查询会让出执行权,轮换可能发生在其间;返回前再做恒定时间比较。
        let still_ok: bool = candidate
            .as_bytes()
            .ct_eq(self.current_prefix().as_bytes())
            .into();
        found.filter(|_| prefix_ok & still_ok)
    }

    pub fn set_prefix(&self, prefix: String) {
        *self.public_path_prefix.write().unwrap() = prefix;
    }

    /// 拼装一条 profile 的永久订阅 URL。
    pub fn subscription_url(&self, token: &str) -> String {
        format!(
            "{}/{}/api/sub/{}",
            self.public_base_url.trim_end_matches('/'),
            self.current_prefix(),
            token
        )
    }

    /// 拼装某订阅自有规则集(③)的永久托管 URL:按订阅 token 隔离,与订阅共用 public_path_prefix。
    /// 不同订阅可复用同名而不冲突。
    pub fn profile_rule_set_url(
        &self,
        token: &str,
        name: &str,
        behavior: &str,
        format: &str,
    ) -> String {
        format!(
            "{}/{}/api/sub/{}/r/{}/{}.{}",
            self.public_base_url.trim_end_matches('/'),
            self.current_prefix(),
            token,
            name,
            behavior,
            format
        )
    }
}

async fn health() -> impl IntoResponse {
    // 有意保持最小:不含版本,避免未认证泄露。
    Json(serde_json::json!({"status": "ok"}))
}

/// 构建完整的应用路由。
///
/// 分层:`/health` 与登录端点公开;其余每条 `/api` 路由都需要有效会话。管理 API 无 CORS 层
/// (它同源),但对状态变更请求强制 `Origin` 校验,并限制请求体大小。未匹配的路径落到 SPA,
/// 以 `index.html` 兜底。
pub fn build_router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/auth/logout", post(auth::logout))
        .route("/auth/session", get(auth::session))
        .route("/profiles", get(profiles::list).post(profiles::create))
        .route(
            "/profiles/{id}",
            get(profiles::get)
                .put(profiles::update)
                .delete(profiles::delete),
        )
        .route("/profiles/{id}/reset-token", post(profiles::reset_token))
        .route("/profiles/{id}/generate", post(generate::generate))
        .route("/profiles/{id}/preview", get(generate::preview))
        .route(
            "/profiles/{id}/provider-rules",
            get(generate::provider_rules),
        )
        .route("/profiles/{id}/rules", put(profiles::put_rules))
        .route(
            "/profiles/{id}/proxies",
            get(profiles::list_proxies_and_groups),
        )
        .route(
            "/profiles/{id}/node-section-order",
            put(profiles::set_node_section_order),
        )
        .route("/profiles/{id}/group-order", put(profiles::set_group_order))
        // 全局自定义节点(跨订阅池):增删改 + 排序。
        .route(
            "/global-nodes",
            get(global_nodes::list).post(global_nodes::create),
        )
        .route("/global-nodes/order", put(global_nodes::set_order))
        .route(
            "/global-nodes/{id}",
            put(global_nodes::update).delete(global_nodes::delete),
        )
        // 全局规则集库(「规则托管」,② 用户库 / 导入源):增删改 + 排序。
        .route("/rule-sets", get(rule_sets::list).post(rule_sets::create))
        .route("/rule-sets/order", put(rule_sets::set_order))
        .route(
            "/rule-sets/{id}",
            put(rule_sets::update).delete(rule_sets::delete),
        )
        // 订阅自有规则集(③ 托管规则库):增删改 + 从 ② 导入。
        .route(
            "/profiles/{id}/rule-sets",
            get(profile_rule_sets::list).post(profile_rule_sets::create),
        )
        .route(
            "/profiles/{id}/rule-sets/import",
            post(profile_rule_sets::import),
        )
        .route(
            "/profiles/{id}/rule-sets/{rsid}",
            put(profile_rule_sets::update).delete(profile_rule_sets::delete),
        )
        .route(
            "/profiles/{id}/groups",
            get(profiles::list_groups).post(profiles::create_group),
        )
        .route(
            "/profiles/{id}/import-provider-groups",
            post(profiles::import_provider_groups),
        )
        .route(
            "/profiles/{id}/groups/{group_id}",
            put(profiles::update_group).delete(profiles::delete_group),
        )
        .route("/settings", get(settings::get))
        .route(
            "/settings/reset-public-path",
            post(settings::reset_public_path),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    let login_route = post(auth::login).layer(middleware::from_fn_with_state(
        state.clone(),
        rate_limit::login,
    ));

    let api = Router::new()
        .route("/auth/login", login_route)
        .merge(protected)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::check_origin,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    let spa = ServeDir::new(&state.web_dir)
        .fallback(ServeFile::new(format!("{}/index.html", state.web_dir)));

    Router::new()
        .route("/health", get(health))
        .nest("/api", api)
        // 公开订阅下载:无鉴权,路径前缀 + token,但按客户端 IP 限流。
        .route(
            "/{public_path_prefix}/api/sub/{token}",
            get(generate::public_sub).layer(middleware::from_fn_with_state(
                state.clone(),
                rate_limit::download,
            )),
        )
        // 公开规则集托管(③ 按订阅 token 隔离):无鉴权,前缀 + token + 名 + `<behavior>.<format>`,
        // 同样按客户端 IP 限流。② 全局库不再公开托管。
        .route(
            "/{public_path_prefix}/api/sub/{token}/r/{name}/{file}",
            get(profile_rule_sets::public_serve).layer(middleware::from_fn_with_state(
                state.clone(),
                rate_limit::download,
            )),
        )
        .fallback_service(spa)
        .layer(middleware::from_fn(frame_protection))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::debug_span!(
                    "request",
                    method = %request.method(),
                    path = %redacted_trace_path(request.uri().path()),
                    version = ?request.version(),
                )
            }),
        )
        .with_state(state)
}

/// 所有响应统一禁止嵌入,覆盖 SPA 深链、静态文件及 304；不限制脚本/样式或订阅下载。
async fn frame_protection(req: Request, next: middleware::Next) -> axum::response::Response {
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response
}

fn redacted_trace_path(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    for i in 0..parts.len().saturating_sub(2) {
        if parts[i] == "api" && parts[i + 1] == "sub" {
            if i > 0 && !parts[i - 1].is_empty() {
                parts[i - 1] = "<prefix>";
            }
            parts[i + 2] = "<token>";
            break;
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::redacted_trace_path;

    #[test]
    fn trace_path_redacts_public_subscription_secrets() {
        assert_eq!(
            redacted_trace_path("/abc123/api/sub/token-value"),
            "/<prefix>/api/sub/<token>"
        );
        assert_eq!(
            redacted_trace_path("/abc123/api/sub/token-value/r/ads/domain.yaml"),
            "/<prefix>/api/sub/<token>/r/ads/domain.yaml"
        );
        assert_eq!(redacted_trace_path("/api/profiles"), "/api/profiles");
    }
}
