//! 受 SSRF 保护的出站拉取:机场订阅与规则集远程镜像。
//!
//! 对每个 URL——以及每一跳重定向——都会校验 URL、解析主机、把解析出的 IP 对照阻止列表检查,
//! 并把 reqwest 固定到那个已校验的确切 IP,使后来的 DNS 应答无法改向连接(DNS 重绑定安全)。
//! 响应体按流式字节上限读取,而非信任 `Content-Length`。见 `docs/architecture.md`「安全设计」。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::redirect::Policy;
use tokio::net::lookup_host;
use url::{Host, Url};

use crate::ssrf::{self, SsrfError};

const MAX_REDIRECTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// 机场拉取的默认 `User-Agent`。许多机场面板(SSPanel、V2board…)以 Clash 家族 UA 作为订阅门槛,
/// 对未知客户端返回 `403`/`401`——或一个非 YAML 页面。`clash.meta` 既匹配常见的 `/clash/i` 检查,
/// 又表明 Meta 支持,使面板下发 Mihomo YAML。可经 `FETCH_USER_AGENT` 覆盖。
pub const DEFAULT_USER_AGENT: &str = "clash.meta/1.0";

/// 对出站拉取(机场订阅与规则集远程镜像)的抽象,使 generate/public 路径无需真实网络即可测试。
/// 生产用 [`HttpFetcher`]。
#[async_trait::async_trait]
pub trait RemoteFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<Fetched, FetchError>;

    /// 拉取原始字节(**不**强制 UTF-8),用于规则集远程镜像——含二进制 `mrs` 规则集。默认实现复用
    /// `fetch` 的文本体字节(够测试 mock 用);生产 [`HttpFetcher`] 覆盖为真正的字节管线。
    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        Ok(self.fetch(url).await?.body.into_bytes())
    }
}

/// 真实的 SSRF 保护拉取器,带配置好的超时与大小上限。
pub struct HttpFetcher {
    pub timeout: Duration,
    pub max_bytes: usize,
    /// 机场请求发送的 `User-Agent`(见 [`DEFAULT_USER_AGENT`])。
    pub user_agent: String,
}

#[async_trait::async_trait]
impl RemoteFetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> Result<Fetched, FetchError> {
        fetch_subscription(url, self.timeout, self.max_bytes, &self.user_agent).await
    }

    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let (bytes, _) = fetch_raw(url, self.timeout, self.max_bytes, &self.user_agent).await?;
        Ok(bytes)
    }
}

/// 一次成功的机场拉取。
#[derive(Debug, Clone)]
pub struct Fetched {
    pub body: String,
    /// 已清洗的 `subscription-userinfo` 头(存在且格式良好时)。
    pub subscription_userinfo: Option<String>,
}

#[derive(Debug)]
pub enum FetchError {
    Ssrf(SsrfError),
    Dns,
    Timeout,
    TooLarge,
    TooManyRedirects,
    Http(u16),
    BadContentType,
    Network,
}

impl FetchError {
    /// 映射为 `last_fetch_status` 标签(见 `docs/architecture.md`「数据模型」)。
    pub fn status_label(&self) -> String {
        match self {
            FetchError::Ssrf(_) => "ssrf_rejected".to_string(),
            FetchError::Dns => "dns_error".to_string(),
            FetchError::Timeout => "timeout".to_string(),
            FetchError::TooLarge => "too_large".to_string(),
            FetchError::TooManyRedirects => "too_many_redirects".to_string(),
            FetchError::Http(code) => format!("http_error:{code}"),
            FetchError::BadContentType => "bad_content_type".to_string(),
            FetchError::Network => "network_error".to_string(),
        }
    }
}

impl From<SsrfError> for FetchError {
    fn from(e: SsrfError) -> Self {
        FetchError::Ssrf(e)
    }
}

/// 带完整 SSRF 保护与限制地拉取一个机场订阅(要求 UTF-8 文本体)。
///
/// `total_timeout` 限制整个拉取(含 DNS 解析、全部重定向跳与响应体读取);`max_bytes` 限制流式读取的
/// 响应体。
pub async fn fetch_subscription(
    raw_url: &str,
    total_timeout: Duration,
    max_bytes: usize,
    user_agent: &str,
) -> Result<Fetched, FetchError> {
    let (bytes, subscription_userinfo) =
        fetch_raw(raw_url, total_timeout, max_bytes, user_agent).await?;
    let body = String::from_utf8(bytes).map_err(|_| FetchError::BadContentType)?;
    Ok(Fetched {
        body,
        subscription_userinfo,
    })
}

/// SSRF 保护的核心拉取循环,返回**原始字节** + 清洗后的 `subscription-userinfo`。文本订阅经
/// [`fetch_subscription`] 再做 UTF-8 校验;规则集远程镜像(含二进制 `mrs`)用 [`RemoteFetcher::fetch_bytes`]
/// 直接取字节。
async fn fetch_raw(
    raw_url: &str,
    total_timeout: Duration,
    max_bytes: usize,
    user_agent: &str,
) -> Result<(Vec<u8>, Option<String>), FetchError> {
    let mut url = Url::parse(raw_url).map_err(|_| FetchError::Ssrf(SsrfError::Host))?;
    // 整个拉取共用一个截止时间:DNS 解析与每一跳请求只拿剩余时长,而非每跳各拿一份完整超时。
    let deadline = Instant::now() + total_timeout;

    // 客户端跨跳复用:仅在钉定的 (host, addr) 变化时(重定向到新主机)才重建,无重定向的常规
    // 路径整个循环只构建一次。IP 钉定仍按跳传递(`.resolve(host, addr)`)。
    let mut cached: Option<((String, SocketAddr), reqwest::Client)> = None;

    for _ in 0..=MAX_REDIRECTS {
        ssrf::validate_url(&url)?;
        // 系统解析器本身可能久等不返回,故解析也受截止时间约束。
        let addr = tokio::time::timeout(remaining(deadline)?, resolve_validated(&url))
            .await
            .map_err(|_| FetchError::Timeout)??;

        let host = url.host_str().ok_or(FetchError::Ssrf(SsrfError::Host))?;
        let pin = (host.to_string(), addr);
        let entry = match cached.take() {
            Some(entry) if entry.0 == pin => entry,
            _ => (
                pin,
                reqwest::Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    // 不走系统/环境变量代理:代理会自行解析域名,绕过下面的 IP 钉定。
                    .no_proxy()
                    .user_agent(user_agent) // 许多面板以 Clash 家族 UA 作为订阅门槛
                    .redirect(Policy::none()) // 手动跟随重定向,以便逐跳重新校验
                    .resolve(host, addr) // 固定到已校验的 IP
                    .build()
                    .map_err(|_| FetchError::Network)?,
            ),
        };
        let client = &cached.insert(entry).1;

        let resp = client
            .get(url.clone())
            .header(ACCEPT, "text/yaml, text/plain, application/x-yaml, */*")
            // 请求级超时覆盖连接到响应体读完,取剩余时长。
            .timeout(remaining(deadline)?)
            .send()
            .await
            .map_err(classify_reqwest)?;

        let status = resp.status();

        if status.is_redirection() {
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(FetchError::Network)?;
            // 相对重定向按当前 URL 解析,然后回到循环重新校验新目标。
            url = url
                .join(location)
                .map_err(|_| FetchError::Ssrf(SsrfError::Host))?;
            continue;
        }

        if !status.is_success() {
            return Err(FetchError::Http(status.as_u16()));
        }

        reject_media_content_type(&resp)?;
        let subscription_userinfo = resp
            .headers()
            .get("subscription-userinfo")
            .and_then(|v| v.to_str().ok())
            .and_then(validate_userinfo);

        let body = read_limited_bytes(resp, max_bytes).await?;
        return Ok((body, subscription_userinfo));
    }

    Err(FetchError::TooManyRedirects)
}

/// 距截止时间的剩余时长;已到期视为超时。
fn remaining(deadline: Instant) -> Result<Duration, FetchError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(FetchError::Timeout)
}

/// 把 URL 主机解析为单个已校验、可连接的 socket 地址。
async fn resolve_validated(url: &Url) -> Result<SocketAddr, FetchError> {
    let port = url.port_or_known_default().unwrap_or(80);
    match url.host().ok_or(FetchError::Ssrf(SsrfError::Host))? {
        Host::Ipv4(ip) => ok_if_allowed((ip, port).into()),
        Host::Ipv6(ip) => ok_if_allowed((ip, port).into()),
        Host::Domain(domain) => {
            let candidates = lookup_host((domain, port))
                .await
                .map_err(|_| FetchError::Dns)?;
            let mut saw_any = false;
            for addr in candidates {
                saw_any = true;
                if !ssrf::is_blocked_ip(addr.ip()) {
                    return Ok(addr);
                }
            }
            if saw_any {
                Err(FetchError::Ssrf(SsrfError::BlockedIp))
            } else {
                Err(FetchError::Dns)
            }
        }
    }
}

fn ok_if_allowed(addr: SocketAddr) -> Result<SocketAddr, FetchError> {
    if ssrf::is_blocked_ip(addr.ip()) {
        Err(FetchError::Ssrf(SsrfError::BlockedIp))
    } else {
        Ok(addr)
    }
}

fn reject_media_content_type(resp: &reqwest::Response) -> Result<(), FetchError> {
    if let Some(ct) = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let major = ct
            .split('/')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if matches!(major.as_str(), "image" | "audio" | "video" | "font") {
            return Err(FetchError::BadContentType);
        }
    }
    Ok(())
}

async fn read_limited_bytes(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, FetchError> {
    let mut resp = resp;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(classify_reqwest)? {
        if buf.len() + chunk.len() > max_bytes {
            return Err(FetchError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// 仅当 `subscription-userinfo` 值是单行且不含控制字符(头注入安全)时才接受,
/// 见 `docs/architecture.md`「安全设计」。
fn validate_userinfo(value: &str) -> Option<String> {
    if value.is_empty() || value.chars().any(|c| c.is_control()) {
        None
    } else {
        Some(value.to_string())
    }
}

fn classify_reqwest(err: reqwest::Error) -> FetchError {
    if err.is_timeout() {
        FetchError::Timeout
    } else {
        FetchError::Network
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_budget_shrinks_and_expires_as_timeout() {
        let left = remaining(Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(left <= Duration::from_secs(60) && left > Duration::from_secs(59));
        assert!(matches!(
            remaining(Instant::now()),
            Err(FetchError::Timeout)
        ));
        let past = Instant::now() - Duration::from_secs(1);
        assert!(matches!(remaining(past), Err(FetchError::Timeout)));
    }

    #[tokio::test]
    async fn exhausted_budget_times_out_before_any_network() {
        // 预算为零:在 DNS 解析与建连之前即以超时失败(域名为 RFC 2606 保留名,不会真正出网)。
        let err = fetch_subscription("https://provider.example/sub", Duration::ZERO, 1024, "ua")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Timeout), "{err:?}");
    }

    #[test]
    fn default_user_agent_is_clash_compatible() {
        // 面板常以不区分大小写的 `clash` 匹配作门槛;保持如此。
        assert!(DEFAULT_USER_AGENT.to_ascii_lowercase().contains("clash"));
    }

    #[test]
    fn sanitize_rejects_control_characters() {
        assert_eq!(
            validate_userinfo("upload=1; download=2; total=3"),
            Some("upload=1; download=2; total=3".to_string())
        );
        assert_eq!(validate_userinfo("inject\r\nSet-Cookie: x"), None);
        assert_eq!(validate_userinfo(""), None);
    }

    #[tokio::test]
    async fn rejects_blocked_host_before_any_connection() {
        let err = fetch_subscription("http://127.0.0.1/x", Duration::from_secs(5), 1024, "ua")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Ssrf(SsrfError::BlockedIp)));
        assert_eq!(err.status_label(), "ssrf_rejected");
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let err = fetch_subscription("file:///etc/passwd", Duration::from_secs(5), 1024, "ua")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Ssrf(SsrfError::Scheme)));
    }

    #[tokio::test]
    async fn rejects_ipv4_mapped_ipv6_loopback() {
        let err = fetch_subscription(
            "http://[::ffff:127.0.0.1]/x",
            Duration::from_secs(5),
            1024,
            "ua",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FetchError::Ssrf(SsrfError::BlockedIp)));
    }
}
