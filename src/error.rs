//! API 错误类型,映射到文档约定的错误信封与状态码(见 `docs/architecture.md`「API 设计」)。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    /// 请求不合法;消息面向客户端,构造时不得携带秘密。
    BadRequest(String),
    /// 逐条列举的校验失败(如生成时的规则检查)。
    Validation(Vec<String>),
    NotFound,
    /// 与现有资源冲突(如重名)。
    Conflict(String),
    /// 上游机场拉取/解析失败;携带一个安全的状态标签。
    Upstream(String),
    /// 未预期的服务端错误;细节只记日志,客户端只见固定文案。
    Internal,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String, Option<Vec<String>>) {
        match self {
            ApiError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, "bad_request", msg.clone(), None)
            }
            ApiError::Validation(details) => (
                StatusCode::BAD_REQUEST,
                "validation_failed",
                "Validation failed".to_string(),
                Some(details.clone()),
            ),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "Not found".to_string(),
                None,
            ),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg.clone(), None),
            ApiError::Upstream(label) => (
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!("Provider fetch failed: {label}"),
                None,
            ),
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Internal server error".to_string(),
                None,
            ),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Vec<String>>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, details) = self.parts();
        let body = ErrorBody {
            error: ErrorDetail {
                code,
                message,
                details,
            },
        };
        (status, Json(body)).into_response()
    }
}

/// 把 SQLx 错误映射为 `ApiError`:UNIQUE 冲突暴露为 `409`,其余一律脱敏为 `500`
/// (原始错误记日志,不返回客户端)。
impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        if is_unique_violation(&err) {
            return ApiError::Conflict("A resource with the same name already exists".to_string());
        }
        // 记日志是安全的:`sqlx::Error` 的 Display 只携带驱动消息(如约束/列名),从不含
        // 绑定的参数值,故机场 URL 或 token 不会从这里泄露。返回客户端的是脱敏的 `500`,
        // 细节留在服务端。切勿改成记录「query + 参数」。
        tracing::error!("database error: {err}");
        ApiError::Internal
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = err {
        // SQLite:2067 = SQLITE_CONSTRAINT_UNIQUE,1555 = PRIMARY KEY。
        return matches!(db.code().as_deref(), Some("2067") | Some("1555"));
    }
    false
}

pub type ApiResult<T> = Result<T, ApiError>;
