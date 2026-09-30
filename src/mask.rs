//! 确定性的机场 URL 脱敏,见 `docs/architecture.md`「安全设计」。
//!
//! 保留 scheme、host 与 path;把每个查询参数值替换为 `***`;丢弃任何 userinfo,使凭据从不被
//! 回显。机场 URL 可能出现的任何地方(响应、日志、错误)都套用同一规则。

/// 脱敏一个机场订阅 URL。无法解析的输入直接坍缩为 `***`,而非泄露原始串。
pub fn mask_url(raw: &str) -> String {
    let Ok(url) = url::Url::parse(raw) else {
        return "***".to_string();
    };

    let mut out = String::new();
    out.push_str(url.scheme());
    out.push_str("://");
    if let Some(host) = url.host_str() {
        out.push_str(host);
    }
    if let Some(port) = url.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    out.push_str(url.path());

    if let Some(query) = url.query() {
        // 逐对重建 query:保留参数名便于排查问题,只隐藏值。
        let masked = query
            .split('&')
            .map(|pair| {
                let key = pair.split('=').next().unwrap_or("");
                format!("{key}=***")
            })
            .collect::<Vec<_>>()
            .join("&");
        out.push('?');
        out.push_str(&masked);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::mask_url;

    #[test]
    fn masks_query_values_and_drops_userinfo() {
        assert_eq!(
            mask_url("https://example.com/api/sub?token=abcdef"),
            "https://example.com/api/sub?token=***"
        );
        assert_eq!(
            mask_url("https://u:p@example.com:8443/x?a=1&b=2"),
            "https://example.com:8443/x?a=***&b=***"
        );
        assert_eq!(mask_url("not a url"), "***");
    }
}
