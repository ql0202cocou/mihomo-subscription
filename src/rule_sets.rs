//! 全局规则集库(「规则托管」,② 用户规则库 / 导入源)。
//!
//! 跨订阅复用的命名规则集模板库。**0.4 起 ② 仅作导入源**:它不再参与生成、也不再公开托管;订阅的
//! 规则集改由各自的 per-profile 库(③,`src/profile_rule_sets.rs`)持有并托管。管理员在此维护模板
//! (手动 payload 或远程来源),再在订阅的「规则」里用「导入托管规则」把它们复制进 ③。
//!
//! 提供增删改 + 库内显示排序。请求体/校验复用 `src/rulelib.rs`。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use crate::rulelib::{self, RuleSetBody};
use crate::util::{MAX_ORDER_ENTRIES, MAX_ORDER_NAME_LEN};
use crate::{mask, util::now};

// ─── 行 / 响应 ──────────────────────────────────────────────────────────────────

/// 列表 / CRUD 用的元信息行(不含 BLOB `cached_body`;② 不再镜像,该列恒为 NULL)。
#[derive(FromRow)]
struct RuleSetRow {
    id: String,
    name: String,
    behavior: String,
    format: String,
    source: String,
    content: String,
    rule_count: i64,
    url: Option<String>,
    interval_hours: i64,
    cache: bool,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize)]
pub struct RuleSetResponse {
    id: String,
    name: String,
    behavior: String,
    format: String,
    source: String,
    content: String,
    enabled: bool,
    /// 规则条数(manual=payload 行数;remote 模板为 0,导入到订阅后由 ③ 镜像回填)。
    count: i64,
    /// 远程来源 URL(已脱敏);manual 为 null。
    remote_url_masked: Option<String>,
    interval_hours: i64,
    cache: bool,
    created_at: String,
    updated_at: String,
}

const META_COLS: &str = "id, name, behavior, format, source, content, rule_count, url, \
     interval_hours, cache, enabled, created_at, updated_at";

fn rule_set_response(row: RuleSetRow) -> RuleSetResponse {
    RuleSetResponse {
        // 来源 URL 属秘密,响应中只输出脱敏形式,绝不回传明文。
        remote_url_masked: row.url.as_deref().map(mask::mask_url),
        count: row.rule_count,
        id: row.id,
        name: row.name,
        behavior: row.behavior,
        format: row.format,
        source: row.source,
        content: row.content,
        enabled: row.enabled,
        interval_hours: row.interval_hours,
        cache: row.cache,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// ─── CRUD ──────────────────────────────────────────────────────────────────────

async fn fetch_all(state: &AppState) -> ApiResult<Vec<RuleSetRow>> {
    Ok(sqlx::query_as::<_, RuleSetRow>(&format!(
        "SELECT {META_COLS} FROM rule_sets ORDER BY position ASC, name ASC"
    ))
    .fetch_all(&state.db)
    .await?)
}

async fn fetch_one(state: &AppState, id: &str) -> ApiResult<RuleSetRow> {
    sqlx::query_as::<_, RuleSetRow>(&format!("SELECT {META_COLS} FROM rule_sets WHERE id = ?"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `GET /api/rule-sets` —— 按库内顺序列出全部规则集。
pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<impl IntoResponse> {
    let sets: Vec<RuleSetResponse> = fetch_all(&state)
        .await?
        .into_iter()
        .map(rule_set_response)
        .collect();
    Ok(Json(sets))
}

/// `POST /api/rule-sets` —— 新建规则集,落在库末尾。name 唯一冲突 → 409。
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RuleSetBody>,
) -> ApiResult<impl IntoResponse> {
    let n = rulelib::normalize(&body, None)?;
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let position =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(position), -1) + 1 FROM rule_sets")
            .fetch_one(&state.db)
            .await?;
    sqlx::query(
        "INSERT INTO rule_sets (id, name, behavior, format, source, content, rule_count, url, \
         interval_hours, cache, enabled, position, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(body.name.trim())
    .bind(&body.behavior)
    .bind(&body.format)
    .bind(&n.source)
    .bind(&n.content)
    .bind(n.rule_count)
    .bind(&n.url)
    .bind(n.interval_hours)
    .bind(n.cache)
    .bind(body.enabled.unwrap_or(true))
    .bind(position)
    .bind(&ts)
    .bind(&ts)
    .execute(&state.db)
    .await?;
    let row = fetch_one(&state, &id).await?;
    Ok((StatusCode::CREATED, Json(rule_set_response(row))))
}

/// `PUT /api/rule-sets/:id` —— 更新规则集。
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<RuleSetBody>,
) -> ApiResult<impl IntoResponse> {
    let existing = fetch_one(&state, &id).await?;
    // 传入已存 URL:remote 编辑时前端拿不到明文 URL(只见过脱敏形式),留空即沿用原值。
    let n = rulelib::normalize(&body, existing.url.as_deref())?;
    sqlx::query(
        "UPDATE rule_sets SET name = ?, behavior = ?, format = ?, source = ?, content = ?, \
         rule_count = ?, url = ?, interval_hours = ?, cache = ?, enabled = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(body.name.trim())
    .bind(&body.behavior)
    .bind(&body.format)
    .bind(&n.source)
    .bind(&n.content)
    .bind(n.rule_count)
    .bind(&n.url)
    .bind(n.interval_hours)
    .bind(n.cache)
    .bind(body.enabled.unwrap_or(true))
    .bind(now())
    .bind(&id)
    .execute(&state.db)
    .await?;
    let row = fetch_one(&state, &id).await?;
    Ok(Json(rule_set_response(row)))
}

/// `DELETE /api/rule-sets/:id` —— 从库中删除规则集。
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let result = sqlx::query("DELETE FROM rule_sets WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ReorderBody {
    /// 期望的库内顺序(规则集名)。不在库中的名字忽略;库中未列出的保持原有相对顺序、排在已列出之后。
    order: Vec<String>,
}

/// `PUT /api/rule-sets/order` —— 设置库内显示顺序(仅影响「规则托管」页展示,不影响任何订阅输出)。
pub async fn set_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReorderBody>,
) -> ApiResult<impl IntoResponse> {
    if body.order.len() > MAX_ORDER_ENTRIES {
        return Err(ApiError::BadRequest(format!(
            "order must have at most {MAX_ORDER_ENTRIES} entries"
        )));
    }
    if body.order.iter().any(|n| n.len() > MAX_ORDER_NAME_LEN) {
        return Err(ApiError::BadRequest(format!(
            "names must be at most {MAX_ORDER_NAME_LEN} bytes"
        )));
    }

    let mut names: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT name FROM rule_sets ORDER BY position ASC, name ASC",
    )
    .fetch_all(&state.db)
    .await?;
    crate::converter::reorder_by_name(&mut names, |s| Some(s.as_str()), &body.order);

    let mut tx = state.db.begin().await?;
    for (position, name) in names.iter().enumerate() {
        sqlx::query("UPDATE rule_sets SET position = ? WHERE name = ?")
            .bind(position as i64)
            .bind(name)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
