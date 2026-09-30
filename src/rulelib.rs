//! 规则集库共享逻辑。
//!
//! ② 全局「规则托管」库(`src/rule_sets.rs`)与 ③ per-profile 自包含规则库
//! (`src/profile_rule_sets.rs`)共用同一套请求体、校验/归一化与托管渲染助手。两者结构一致,仅
//! 「作用域」(全局 vs 按 profile)与「是否对外托管」不同,故把无 DB 依赖的纯逻辑集中于此,避免复制。

use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use crate::error::{ApiError, ApiResult};

/// 规则集名长度上限(同时是 URL 路径段与 `RULE-SET` 引用名)。
pub const MAX_NAME_LEN: usize = 128;
pub const BEHAVIORS: &[&str] = &["domain", "ipcidr", "classical"];
pub const MANUAL_FORMATS: &[&str] = &["yaml", "text"];
pub const REMOTE_FORMATS: &[&str] = &["yaml", "text", "mrs"];

/// 规则集 CRUD 的请求体(② 与 ③ 共用,字段一致)。
#[derive(Deserialize)]
pub struct RuleSetBody {
    pub name: String,
    pub behavior: String,
    pub format: String,
    pub source: Option<String>,
    pub content: Option<String>,
    pub url: Option<String>,
    pub interval_hours: Option<i64>,
    pub cache: Option<bool>,
    pub enabled: Option<bool>,
}

/// 归一化后的可入库字段。
pub struct Normalized {
    pub source: String,
    pub content: String,
    pub url: Option<String>,
    pub interval_hours: i64,
    pub cache: bool,
    pub rule_count: i64,
}

/// 校验并归一化请求体。name 入 URL 路径 + 作 `RULE-SET` 引用名,故限定安全字符集;manual/remote
/// 各有合法的 format 集合,remote 必须给 http(s) URL,由面板代为拉取(cache=true)时还须通过 SSRF 静态校验。`existing_url` 是更新时已存的远程
/// URL:remote 编辑留空则沿用它(URL 已脱敏不回显,改地址需重填)。
pub fn normalize(body: &RuleSetBody, existing_url: Option<&str>) -> ApiResult<Normalized> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(ApiError::BadRequest(format!(
            "name is required and must be at most {MAX_NAME_LEN} bytes"
        )));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(ApiError::BadRequest(
            "name may only contain letters, digits, '.', '_', '-'".into(),
        ));
    }
    if !BEHAVIORS.contains(&body.behavior.as_str()) {
        return Err(ApiError::BadRequest(
            "behavior must be one of domain, ipcidr, classical".into(),
        ));
    }

    let source = body.source.as_deref().unwrap_or("manual");
    match source {
        "manual" => {
            if !MANUAL_FORMATS.contains(&body.format.as_str()) {
                return Err(ApiError::BadRequest(
                    "manual format must be yaml or text".into(),
                ));
            }
            let content = body.content.clone().unwrap_or_default();
            Ok(Normalized {
                source: "manual".into(),
                rule_count: payload_count(&content),
                content,
                url: None,
                interval_hours: 24,
                cache: true,
            })
        }
        "remote" => {
            if !REMOTE_FORMATS.contains(&body.format.as_str()) {
                return Err(ApiError::BadRequest(
                    "remote format must be yaml, text or mrs".into(),
                ));
            }
            // 优先用本次填写的 URL;留空则沿用已存的(编辑场景)。
            let provided = body.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
            let url = match provided {
                Some(u) => {
                    if !(u.starts_with("http://") || u.starts_with("https://")) {
                        return Err(ApiError::BadRequest(
                            "remote source requires an http(s) url".into(),
                        ));
                    }
                    u.to_string()
                }
                None => existing_url
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        ApiError::BadRequest("remote source requires an http(s) url".into())
                    })?,
            };
            let interval_hours = body.interval_hours.unwrap_or(24);
            if interval_hours < 1 {
                return Err(ApiError::BadRequest("interval_hours must be >= 1".into()));
            }
            // cache=true 时由面板代为拉取,按 SSRF 规则做写入期静态校验(含沿用的旧 URL);cache=false 时
            // URL 原样写进客户端配置、由客户端拉取,面板不出站,故允许局域网等规则源。
            let cache = body.cache.unwrap_or(true);
            if cache {
                crate::ssrf::validate_write_url(&url, "url")?;
            }
            Ok(Normalized {
                source: "remote".into(),
                content: String::new(),
                url: Some(url),
                interval_hours,
                cache,
                rule_count: 0, // 首次成功镜像后回填
            })
        }
        _ => Err(ApiError::BadRequest(
            "source must be manual or remote".into(),
        )),
    }
}

/// 有效 payload 行:非空、非 `#` 注释。供计数与手动托管渲染共用。
pub fn payload_lines(content: &str) -> impl Iterator<Item = &str> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

pub fn payload_count(content: &str) -> i64 {
    payload_lines(content).count() as i64
}

/// 远程镜像体的近似规则数:文本格式按有效行数,`mrs`(二进制)为 0。
pub fn body_count(bytes: &[u8], format: &str) -> i64 {
    if format == "mrs" {
        0
    } else {
        payload_count(&String::from_utf8_lossy(bytes))
    }
}

/// 渲染手动规则集:`yaml` → Mihomo rule-provider 的 `payload:` 列表;`text` → 逐行原样。
pub fn render_manual(content: &str, format: &str) -> String {
    if format == "yaml" {
        let payload: Vec<Value> = payload_lines(content).map(Value::from).collect();
        let mut map = Mapping::new();
        map.insert(Value::from("payload"), Value::Sequence(payload));
        serde_yaml::to_string(&Value::Mapping(map)).unwrap_or_default()
    } else {
        let mut s = payload_lines(content).collect::<Vec<_>>().join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        s
    }
}

/// 按 format 选 content-type 返回字节体(`mrs` 二进制 → octet-stream)。
pub fn serve_bytes(format: &str, bytes: Vec<u8>) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    let ct = if format == "mrs" {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    };
    ([(header::CONTENT_TYPE, ct)], bytes).into_response()
}
