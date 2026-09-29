//! 内存令牌桶限流,以及 login/download 中间件。
//!
//! 令牌桶(而非固定窗口)能平滑流量,避免固定窗口在边界处允许的 ~2 倍突发。每个 key 持有 `max`
//! 个令牌,以每秒 `max / window` 的速率持续补充;至少有一个完整令牌时才放行请求。
//!
//! 单实例自托管 MVP 用内存限流即可(见 `docs/architecture.md`「安全设计」);只有应用日后多实例运行时
//! 才需要共享存储。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::app::AppState;
use crate::net;

/// 以任意字符串为 key 的令牌桶限流器。
pub struct RateLimiter {
    inner: Mutex<HashMap<String, Bucket>>,
    /// 桶容量(突发大小,也是每窗口的稳态上限)。
    capacity: f64,
    /// 每秒补充的令牌数。
    refill_per_sec: f64,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    /// 每 `window` 允许 `max` 个请求(也是突发容量)。
    pub fn new(max: u32, window: Duration) -> Self {
        let capacity = max as f64;
        let secs = window.as_secs_f64().max(f64::MIN_POSITIVE);
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity,
            refill_per_sec: capacity / secs,
        }
    }

    /// 为 `key` 记一次命中;有可用令牌则返回 `true`。
    pub fn try_acquire(&self, key: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();

        // 机会性清理,以在大量不同 key 下限制内存。已完全补满的桶与新建桶无法区分,故任何空闲
        // 满一个窗口的桶都可丢弃而不影响限流。
        if map.len() > 10_000 {
            let window_secs = self.capacity / self.refill_per_sec;
            map.retain(|_, b| now.duration_since(b.last).as_secs_f64() < window_secs);
        }

        let bucket = map.entry(key.to_string()).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        // 按流逝时间补充令牌,上限为容量。
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// 用应用的受信代理配置推导 `req` 的限流 key(客户端 IP 字符串,推导失败时为 `"unknown"`)。
fn client_ip_key(state: &AppState, req: &Request) -> String {
    let xff = req
        .headers()
        .get(header::HeaderName::from_static("x-forwarded-for"))
        .and_then(|v| v.to_str().ok());
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    net::client_ip_string(
        xff,
        peer,
        state.trusted_proxy_hops,
        &state.trusted_proxy_cidrs,
    )
}

/// 按客户端 IP 限制登录尝试(暴力破解防护)。
pub async fn login(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let key = format!("login:{}", client_ip_key(&state, &req));
    if !state.login_limiter.try_acquire(&key) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    next.run(req).await
}

/// 按客户端 IP 限制公开订阅请求,与路径中的 token 无关。只按 IP(而非 IP+路径)做 key,意味着
/// 一个客户端猜很多不同 token 时共享同一预算,故限流器真正能抑制 token 枚举/扫描——且在处理器
/// 之前运行,故 404 也计数。
pub async fn download(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let key = format!("dl:{}", client_ip_key(&state, &req));
    if !state.download_limiter.try_acquire(&key) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_burst_up_to_capacity_then_denies() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        assert!(rl.try_acquire("k"));
        assert!(rl.try_acquire("k"));
        assert!(rl.try_acquire("k"));
        assert!(
            !rl.try_acquire("k"),
            "4th hit over the burst capacity is denied"
        );
        // A different key has its own bucket.
        assert!(rl.try_acquire("other"));
    }

    #[test]
    fn tokens_refill_over_time() {
        let rl = RateLimiter::new(1, Duration::from_millis(20));
        assert!(rl.try_acquire("k"));
        assert!(!rl.try_acquire("k"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            rl.try_acquire("k"),
            "token refilled after the window elapsed"
        );
    }

    #[test]
    fn partial_refill_grants_partial_budget() {
        // 10 tokens / 100ms = 1 token per 10ms. Drain, wait ~50ms, expect a
        // partial (not full) budget back — the hallmark of a token bucket vs a
        // fixed window, which would hand back the whole budget at the boundary.
        let rl = RateLimiter::new(10, Duration::from_millis(100));
        for _ in 0..10 {
            assert!(rl.try_acquire("k"));
        }
        assert!(!rl.try_acquire("k"), "bucket drained");

        std::thread::sleep(Duration::from_millis(50));
        let granted = (0..10).filter(|_| rl.try_acquire("k")).count();
        assert!(
            (3..=7).contains(&granted),
            "expected a partial refill (~5), got {granted}"
        );
    }
}
