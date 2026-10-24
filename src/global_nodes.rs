//! 全局自定义节点:单一跨订阅的自定义代理节点池,自动追加到每条 profile 的输出(模型 C)。
//! 提供增删改 + 全局排序。契约见 `docs/architecture.md`「API 设计」;转换时由 `src/generate.rs` 读取本池。

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
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::util::{now, MAX_ORDER_ENTRIES, MAX_ORDER_NAME_LEN};
use crate::yaml;

#[derive(FromRow)]
struct GlobalNodeRow {
    id: String,
    name: String,
    node_type: String,
    content: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize)]
pub struct GlobalNodeResponse {
    id: String,
    name: String,
    node_type: String,
    content: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

fn node_response(row: GlobalNodeRow) -> GlobalNodeResponse {
    GlobalNodeResponse {
        id: row.id,
        name: row.name,
        node_type: row.node_type,
        content: row.content,
        enabled: row.enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

#[derive(Deserialize)]
pub struct NodeBody {
    name: String,
    node_type: String,
    content: String,
    /// 可省略,缺省视为 true(与写入时的 `unwrap_or(true)` 对应)。
    enabled: Option<bool>,
}

fn validate_node(body: &NodeBody) -> ApiResult<()> {
    if body.name.trim().is_empty() || body.node_type.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "name and node_type are required".into(),
        ));
    }
    // 内置保留名(DIRECT/REJECT/…):占用会让 Mihomo 以 duplicate name 拒绝加载整个输出。
    if crate::converter::RESERVED_NODE_NAMES.contains(&body.name.trim()) {
        return Err(ApiError::BadRequest(format!(
            "name `{}` conflicts with a reserved built-in name",
            body.name.trim()
        )));
    }
    // 节点 content 必须是单个合法的 Mihomo proxy 映射。
    yaml::parse_mapping(&body.content)
        .map_err(|_| ApiError::BadRequest("content must be a valid YAML proxy mapping".into()))?;
    Ok(())
}

/// 按自定义块顺序(`position`,`name` 作确定性兜底)读取全部全局节点。
async fn fetch_all(state: &AppState) -> ApiResult<Vec<GlobalNodeRow>> {
    Ok(sqlx::query_as::<_, GlobalNodeRow>(
        "SELECT id, name, node_type, content, enabled, created_at, updated_at
         FROM global_nodes ORDER BY position ASC, name ASC",
    )
    .fetch_all(&state.db)
    .await?)
}

async fn fetch_one(state: &AppState, id: &str) -> ApiResult<GlobalNodeRow> {
    sqlx::query_as::<_, GlobalNodeRow>(
        "SELECT id, name, node_type, content, enabled, created_at, updated_at
         FROM global_nodes WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /api/global-nodes` —— 按自定义块顺序列出全局节点池。
pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<impl IntoResponse> {
    let nodes: Vec<GlobalNodeResponse> = fetch_all(&state)
        .await?
        .into_iter()
        .map(node_response)
        .collect();
    Ok(Json(nodes))
}

/// `POST /api/global-nodes` —— 新增节点,落在顺序末尾。
pub async fn create(
    State(state): State<Arc<AppState>>,
    ApiJson(body): ApiJson<NodeBody>,
) -> ApiResult<impl IntoResponse> {
    validate_node(&body)?;
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    // 新节点落在自定义块末尾(max position + 1);立即事务避免并发创建占用同一位置。
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let position =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(position), -1) + 1 FROM global_nodes")
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(
        "INSERT INTO global_nodes (id, name, node_type, content, enabled, position, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(body.name.trim())
    .bind(&body.node_type)
    .bind(&body.content)
    .bind(body.enabled.unwrap_or(true))
    .bind(position)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let row = fetch_one(&state, &id).await?;
    Ok((StatusCode::CREATED, Json(node_response(row))))
}

/// `PUT /api/global-nodes/:id` —— 更新节点(name/type/content/enabled)。
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NodeBody>,
) -> ApiResult<impl IntoResponse> {
    let _ = fetch_one(&state, &id).await?; // 先确认存在:不存在直接 404,不进入校验
    validate_node(&body)?;
    sqlx::query(
        "UPDATE global_nodes SET name = ?, node_type = ?, content = ?, enabled = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(body.name.trim())
    .bind(&body.node_type)
    .bind(&body.content)
    .bind(body.enabled.unwrap_or(true))
    .bind(now())
    .bind(&id)
    .execute(&state.db)
    .await?;
    let row = fetch_one(&state, &id).await?;
    Ok(Json(node_response(row)))
}

/// `DELETE /api/global-nodes/:id` —— 从池中删除节点。
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let result = sqlx::query("DELETE FROM global_nodes WHERE id = ?")
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
    /// 期望的自定义块顺序(节点名)。不在池中的名字忽略;池中未列出的名字保持原有相对顺序、
    /// 排在已列出的之后。
    order: Vec<String>,
}

/// `PUT /api/global-nodes/order` —— 设置全局自定义块顺序。列出的名字按给定次序在前,未列出的
/// 保持在末尾;`position` 重写为连续的 `0..n-1`。改动即时应用到每条 profile 的缓存(尽力而为地
/// 离线重生成,不重拉机场)。
pub async fn set_order(
    State(state): State<Arc<AppState>>,
    ApiJson(body): ApiJson<ReorderBody>,
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

    // 立即事务固定名称快照;按不可变 ID 更新,避免并发改名导致漏行。
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let mut rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM global_nodes ORDER BY position ASC, name ASC")
            .fetch_all(&mut *tx)
            .await?;
    crate::converter::reorder_by_name(&mut rows, |(_, name)| Some(name.as_str()), &body.order);

    for (position, (id, _)) in rows.iter().enumerate() {
        sqlx::query("UPDATE global_nodes SET position = ? WHERE id = ?")
            .bind(position as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    // 用各 profile 缓存的机场原文离线重生成,新顺序立即生效、不重拉机场。
    crate::generate::regenerate_all_from_cache(&state).await;
    Ok(StatusCode::NO_CONTENT)
}
