//! SSRF 防护:纯粹的 URL 与 IP 校验。
//!
//! 实现 `docs/security-design.md` 的规则。本模块不涉网络、且用表驱动充分测试;真正的固定 IP
//! 拉取在 `fetch.rs`,它对每个 URL、每一跳重定向都调用这里。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;

use ipnet::IpNet;
use url::{Host, Url};

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsrfError {
    /// scheme 不是 http/https。
    Scheme,
    /// 缺失或为空的 host。
    Host,
    /// URL 中内嵌了凭据。
    Credentials,
    /// 主机名是回环名(如 localhost)。
    BlockedHost,
    /// 解析出的或字面 IP 落在被阻止的范围内。
    BlockedIp,
}

/// 绝不能作为连接目标的 IPv4 范围(见 security-design.md)。
const BLOCKED_V4: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.88.99.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

/// 直接阻止的 IPv6 范围。IPv4 内嵌范围(IPv4 映射、NAT64、6to4)单独通过解包内嵌 IPv4 处理。
const BLOCKED_V6: &[&str] = &["::/128", "::1/128", "fc00::/7", "fe80::/10", "ff00::/8"];

fn blocked_nets() -> &'static Vec<IpNet> {
    static NETS: OnceLock<Vec<IpNet>> = OnceLock::new();
    NETS.get_or_init(|| {
        BLOCKED_V4
            .iter()
            .chain(BLOCKED_V6.iter())
            .map(|c| c.parse().expect("valid CIDR literal"))
            .collect()
    })
}

/// 校验 URL 的静态部分(scheme、凭据、主机名)。IP 可达性在解析后经 [`is_blocked_ip`] 单独检查。
pub fn validate_url(url: &Url) -> Result<(), SsrfError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SsrfError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SsrfError::Credentials);
    }
    match url.host() {
        None => Err(SsrfError::Host),
        Some(Host::Domain("")) => Err(SsrfError::Host),
        Some(Host::Domain(d)) => {
            let h = d.to_ascii_lowercase();
            if h == "localhost" || h.ends_with(".localhost") {
                Err(SsrfError::BlockedHost)
            } else {
                Ok(())
            }
        }
        // 裸 IP 主机在此校验,使被阻止的字面 IP 永不进入解析。
        Some(Host::Ipv4(ip)) => reject_if_blocked(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => reject_if_blocked(IpAddr::V6(ip)),
    }
}

/// 写入时校验一个将由面板出站拉取的 URL(机场订阅、缓存托管的远程规则集),`field` 为报错里的字段名。
/// 这是纵深防御,也带来更好的错误体验——权威的 SSRF 检查仍在拉取时带 DNS 解析与 IP 固定地运行
/// (`src/fetch.rs`)。这里检查静态部分:scheme、内嵌凭据、回环名、被阻止的字面 IP。仅含主机名的 URL
/// 通过(写入时不做 DNS 查找)。消息按错误种类泛化,故原始 URL 永不回显。
pub fn validate_write_url(raw: &str, field: &str) -> ApiResult<()> {
    let url =
        Url::parse(raw).map_err(|_| ApiError::BadRequest(format!("{field} is not a valid URL")))?;
    validate_url(&url).map_err(|e| {
        let msg = match e {
            SsrfError::Scheme => "must use http or https",
            SsrfError::Host => "is missing a host",
            SsrfError::Credentials => "must not embed credentials",
            SsrfError::BlockedHost | SsrfError::BlockedIp => {
                "points to a disallowed (local/private) address"
            }
        };
        ApiError::BadRequest(format!("{field} {msg}"))
    })
}

fn reject_if_blocked(ip: IpAddr) -> Result<(), SsrfError> {
    if is_blocked_ip(ip) {
        Err(SsrfError::BlockedIp)
    } else {
        Ok(())
    }
}

/// 连接到 `ip` 是否被禁止。对内嵌 IPv4 的 IPv6,会提取内嵌 IPv4 并对照 IPv4 阻止列表检查
/// (堵住 `::ffff:127.0.0.1` / NAT64 / 6to4 绕过)。
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_ipv4(v6) {
                return is_blocked_v4(v4);
            }
            blocked_nets()
                .iter()
                .any(|net| net.contains(&IpAddr::V6(v6)))
        }
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    blocked_nets()
        .iter()
        .any(|net| net.contains(&IpAddr::V4(ip)))
}

/// 从 IPv4 映射(`::ffff:0:0/96`)、NAT64(`64:ff9b::/96`)或 6to4(`2002::/16`)的 IPv6
/// 地址中提取内嵌的 IPv4 地址。
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let o = ip.octets();
    // NAT64 周知前缀 64:ff9b::/96。
    if o[0] == 0x00 && o[1] == 0x64 && o[2] == 0xff && o[3] == 0x9b && o[4..12] == [0u8; 8] {
        return Some(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    // 6to4 2002::/16 在第 2..6 字节携带 IPv4。
    if o[0] == 0x20 && o[1] == 0x02 {
        return Some(Ipv4Addr::new(o[2], o[3], o[4], o[5]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_every_documented_ipv4_range() {
        for (addr, blocked) in [
            ("0.0.0.0", true),
            ("10.1.2.3", true),
            ("100.64.0.1", true),
            ("127.0.0.1", true),
            ("169.254.1.1", true),
            ("172.16.5.4", true),
            ("192.0.2.5", true),
            ("192.88.99.1", true),
            ("192.168.1.1", true),
            ("198.18.0.1", true),
            ("198.51.100.7", true),
            ("203.0.113.9", true),
            ("224.0.0.1", true),
            ("240.0.0.1", true),
            // Public addresses must be allowed.
            ("1.1.1.1", false),
            ("8.8.8.8", false),
            ("93.184.216.34", false),
        ] {
            assert_eq!(is_blocked_ip(ip(addr)), blocked, "{addr}");
        }
    }

    #[test]
    fn blocks_ipv6_ranges_and_public_is_allowed() {
        assert!(is_blocked_ip(ip("::1")));
        assert!(is_blocked_ip(ip("::")));
        assert!(is_blocked_ip(ip("fc00::1")));
        assert!(is_blocked_ip(ip("fe80::1")));
        assert!(is_blocked_ip(ip("ff02::1")));
        assert!(!is_blocked_ip(ip("2606:4700:4700::1111")));
    }

    #[test]
    fn unwraps_ipv4_embedding_bypasses() {
        // IPv4-mapped loopback.
        assert!(is_blocked_ip(ip("::ffff:127.0.0.1")));
        // IPv4-mapped private.
        assert!(is_blocked_ip(ip("::ffff:10.0.0.1")));
        // IPv4-mapped public is allowed.
        assert!(!is_blocked_ip(ip("::ffff:8.8.8.8")));
        // NAT64 wrapping a private address.
        assert!(is_blocked_ip(ip("64:ff9b::192.168.0.1")));
        // 6to4 wrapping loopback (2002:7f00:0001::).
        assert!(is_blocked_ip("2002:7f00:0001::".parse().unwrap()));
    }

    #[test]
    fn url_validation_rules() {
        assert_eq!(
            validate_url(&Url::parse("ftp://x/").unwrap()),
            Err(SsrfError::Scheme)
        );
        assert_eq!(
            validate_url(&Url::parse("https://user:pass@example.com/").unwrap()),
            Err(SsrfError::Credentials)
        );
        assert_eq!(
            validate_url(&Url::parse("http://localhost/").unwrap()),
            Err(SsrfError::BlockedHost)
        );
        assert_eq!(
            validate_url(&Url::parse("http://127.0.0.1/").unwrap()),
            Err(SsrfError::BlockedIp)
        );
        assert_eq!(
            validate_url(&Url::parse("http://[::ffff:169.254.0.1]/").unwrap()),
            Err(SsrfError::BlockedIp)
        );
        assert!(validate_url(&Url::parse("https://example.com/sub?token=x").unwrap()).is_ok());
    }
}
