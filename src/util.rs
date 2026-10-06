//! 共享小工具:时间戳,以及由 CSPRNG 支撑的随机标识符。

use std::time::Duration;

use base64::Engine;
use rand::Rng;

/// 排序请求的上界(条目数 / 名字字节数),所有排序端点(per-profile、全局节点、规则库)共享,
/// 使持久化的顺序保持得小、请求校验便宜。现实中条目数远低于此。
pub const MAX_ORDER_ENTRIES: usize = 5_000;
pub const MAX_ORDER_NAME_LEN: usize = 256;

/// 当前时间,RFC 3339 UTC 字符串(本项目的时间戳格式)。
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// RFC 3339 时间戳 `at` 是否仍在 `ttl` 新鲜窗口内。无法解析或落在未来(负年龄)都算不新鲜。
pub fn is_fresh(at: &str, ttl: Duration) -> bool {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(at.with_timezone(&chrono::Utc));
    age.to_std().map(|a| a < ttl).unwrap_or(false)
}

/// per-profile 的订阅 token:32 随机字节,URL-safe(≥256 位),见 `docs/architecture.md`「安全设计」。
pub fn random_token() -> String {
    random_b64(32)
}

/// 随机公共路径前缀:16 随机字节,URL-safe(~22 字符,落在 `docs/architecture.md`「安全设计」
/// 推荐的 16-24 字符区间内)。
pub fn random_path_prefix() -> String {
    random_b64(16)
}

fn random_b64(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 同批等待请求共享最近完成的刷新结果(包括失败),即使最小间隔设为零。
pub fn completed_since(completed_at: &str, arrived: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(completed_at),
        chrono::DateTime::parse_from_rfc3339(arrived),
    ) {
        (Ok(completed), Ok(arrived)) => completed >= arrived,
        _ => false,
    }
}
