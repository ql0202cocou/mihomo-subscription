//! 配置管理:profile CRUD,加规则与自定义分组。转换(generate/preview/公开端点)单独实现。
//! 契约遵循 `docs/architecture.md`「API 设计」。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::FromRow;

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use crate::generate::{regenerate_from_cache, Regenerated};
use crate::mask::mask_url;
use crate::ssrf;
use crate::util::{now, random_token, MAX_ORDER_ENTRIES, MAX_ORDER_NAME_LEN};
use crate::yaml;

// Mihomo 支持的代理分组类型白名单;手动创建与机场导入共用同一过滤。
const GROUP_TYPES: [&str; 5] = ["select", "url-test", "fallback", "load-balance", "relay"];

// ─── DB 行类型 ─────────────────────────────────────────────────────────────────

#[derive(FromRow)]
struct ProfileRow {
    id: String,
    name: String,
    source_url: String,
    output_type: String,
    token: String,
    last_fetch_at: Option<String>,
    last_fetch_status: Option<String>,
    last_generated_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct NodeRow {
    id: String,
    name: String,
    node_type: String,
    content: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct GroupRow {
    id: String,
    name: String,
    group_type: String,
    members: String,
    options: Option<String>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct RulesetRow {
    content: String,
    updated_at: String,
}

// ─── 响应类型 ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ProfileSummary {
    id: String,
    name: String,
    source_url_masked: String,
    output_type: String,
    subscription_url: String,
    last_fetch_at: Option<String>,
    last_fetch_status: Option<String>,
    last_generated_at: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize)]
pub struct ProfileDetail {
    #[serde(flatten)]
    summary: ProfileSummary,
    rules: Option<Ruleset>,
    nodes: Vec<NodeResponse>,
    groups: Vec<GroupResponse>,
}

#[derive(Serialize)]
struct Ruleset {
    content: String,
    updated_at: String,
}

#[derive(Serialize)]
struct NodeResponse {
    id: String,
    name: String,
    node_type: String,
    content: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Serialize)]
struct GroupResponse {
    id: String,
    name: String,
    group_type: String,
    members: Vec<String>,
    options: Option<JsonValue>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

fn summary(state: &AppState, row: ProfileRow) -> ProfileSummary {
    ProfileSummary {
        subscription_url: state.subscription_url(&row.token),
        source_url_masked: mask_url(&row.source_url),
        id: row.id,
        name: row.name,
        output_type: row.output_type,
        last_fetch_at: row.last_fetch_at,
        last_fetch_status: row.last_fetch_status,
        last_generated_at: row.last_generated_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn node_response(row: NodeRow) -> NodeResponse {
    NodeResponse {
        id: row.id,
        name: row.name,
        node_type: row.node_type,
        content: row.content,
        enabled: row.enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// members/options 在库中是 JSON 文本;解析失败兜底为空,不让单条坏数据打挂整个详情响应。
fn group_response(row: GroupRow) -> GroupResponse {
    GroupResponse {
        members: serde_json::from_str(&row.members).unwrap_or_default(),
        options: row.options.and_then(|o| serde_json::from_str(&o).ok()),
        id: row.id,
        name: row.name,
        group_type: row.group_type,
        enabled: row.enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// ─── Profile CRUD ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateProfile {
    name: String,
    source_url: String,
}

#[derive(Deserialize)]
pub struct UpdateProfile {
    name: Option<String>,
    /// 只写:缺失或为空则保持已存 URL 不变。
    source_url: Option<String>,
}

/// 读取 profile 行,顺带从 1—1 的 `generated_cache` 子查询出最近生成时间(无缓存则为 NULL)。
async fn load_profile_row(state: &AppState, id: &str) -> ApiResult<ProfileRow> {
    sqlx::query_as::<_, ProfileRow>(
        "SELECT p.*, (SELECT generated_at FROM generated_cache WHERE profile_id = p.id) \
         AS last_generated_at FROM profiles p WHERE p.id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query_as::<_, ProfileRow>(
        "SELECT p.*, (SELECT generated_at FROM generated_cache WHERE profile_id = p.id) \
         AS last_generated_at FROM profiles p ORDER BY p.created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    let out: Vec<ProfileSummary> = rows.into_iter().map(|r| summary(&state, r)).collect();
    Ok(Json(out))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateProfile>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    if body.source_url.trim().is_empty() {
        return Err(ApiError::BadRequest("source_url is required".into()));
    }
    ssrf::validate_write_url(body.source_url.trim(), "source_url")?;

    let id = uuid::Uuid::new_v4().to_string();
    let token = random_token();
    let ts = now();

    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO profiles (id, name, source_url, token, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(body.name.trim())
    .bind(body.source_url.trim())
    .bind(&token)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut *tx)
    .await?;

    // 每个 profile 以空规则集起步(1—1,经 PUT 替换)。
    sqlx::query("INSERT INTO rulesets (id, profile_id, content, updated_at) VALUES (?, ?, '', ?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .bind(&ts)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    // 新建后自动拉取一次,使列表/详情立即反映真实 last_fetch_status(无「未拉取」中间态)。
    // 尽力而为,拉取失败只记状态、不影响创建结果。
    crate::generate::generate_best_effort(&state, &id).await;

    let row = load_profile_row(&state, &id).await?;
    Ok((StatusCode::CREATED, Json(detail(&state, row).await?)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let row = load_profile_row(&state, &id).await?;
    Ok(Json(detail(&state, row).await?))
}

async fn detail(state: &AppState, row: ProfileRow) -> ApiResult<ProfileDetail> {
    let profile_id = row.id.clone();

    let rules = sqlx::query_as::<_, RulesetRow>(
        "SELECT content, updated_at FROM rulesets WHERE profile_id = ?",
    )
    .bind(&profile_id)
    .fetch_optional(&state.db)
    .await?
    .map(|r| Ruleset {
        content: r.content,
        updated_at: r.updated_at,
    });

    // 自定义节点是单一全局池(模型 C),追加到每条 profile 的输出;此处只读暴露(各 profile
    // 一致),让详情页与分组/规则建议仍能看到节点名。编辑与排序在全局 `/api/global-nodes` 端点。
    let nodes = sqlx::query_as::<_, NodeRow>(
        "SELECT id, name, node_type, content, enabled, created_at, updated_at
         FROM global_nodes ORDER BY position ASC, name ASC",
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(node_response)
    .collect();

    let groups = sqlx::query_as::<_, GroupRow>(
        "SELECT id, name, group_type, members, options, enabled, created_at, updated_at
         FROM custom_groups WHERE profile_id = ? ORDER BY created_at ASC",
    )
    .bind(&profile_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(group_response)
    .collect();

    Ok(ProfileDetail {
        summary: summary(state, row),
        rules,
        nodes,
        groups,
    })
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProfile>,
) -> ApiResult<impl IntoResponse> {
    let existing = load_profile_row(&state, &id).await?;

    // 与创建一致:去除首尾空白,空名拒绝。
    let name = match body.name {
        Some(n) if n.trim().is_empty() => {
            return Err(ApiError::BadRequest("name is required".into()))
        }
        Some(n) => n.trim().to_string(),
        None => existing.name,
    };
    // 只写 URL:除非提供非空值,否则保持已存值。
    let source_url = match body.source_url {
        Some(u) if !u.trim().is_empty() => {
            let trimmed = u.trim();
            ssrf::validate_write_url(trimmed, "source_url")?;
            trimmed.to_string()
        }
        _ => existing.source_url,
    };
    sqlx::query(
        "UPDATE profiles SET name = ?, source_url = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(&name)
    .bind(&source_url)
    .bind(now())
    .bind(&id)
    .execute(&state.db)
    .await?;

    let row = load_profile_row(&state, &id).await?;
    Ok(Json(detail(&state, row).await?))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    // 依赖 `foreign_keys=ON` 的 ON DELETE CASCADE 一并清理 rulesets/custom_groups/
    // profile_rule_sets/generated_cache 等从表。
    let result = sqlx::query("DELETE FROM profiles WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct TokenResponse {
    token: String,
    subscription_url: String,
}

/// 重置订阅 token。旧 token 立即失效(公开端点按 token 查库);缓存随即离线重生成,使其中按 token
/// 隔离的规则集托管链接改用新 token。
pub async fn reset_token(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let _ = load_profile_row(&state, &id).await?;
    let token = random_token();
    sqlx::query("UPDATE profiles SET token = ?, updated_at = ? WHERE id = ?")
        .bind(&token)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    regenerate_from_cache(&state, &id).await;
    Ok(Json(TokenResponse {
        subscription_url: state.subscription_url(&token),
        token,
    }))
}

// ─── 规则 ──────────────────────────────────────────────────────────────────────

/// 规则/排序类保存的响应:改动已入库,`regenerate` 说明它是否已下发到所服务的订阅。
#[derive(Serialize)]
pub struct SavedResponse {
    regenerate: Regenerated,
}

#[derive(Deserialize)]
pub struct PutRules {
    content: String,
}

pub async fn put_rules(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PutRules>,
) -> ApiResult<impl IntoResponse> {
    let _ = load_profile_row(&state, &id).await?;
    sqlx::query("UPDATE rulesets SET content = ?, updated_at = ? WHERE profile_id = ?")
        .bind(&body.content)
        .bind(now())
        .bind(&id)
        .execute(&state.db)
        .await?;

    // 用缓存的机场原文离线重生成,使编辑立即反映到所服务的订阅(不合法则保留上一份合法输出)。
    Ok(Json(SavedResponse {
        regenerate: regenerate_from_cache(&state, &id).await,
    }))
}

// ─── 代理预览 & 节点/分组排序 ─────────────────────────────────────────────────
//
// 自定义节点的增删改现在归全局 `/api/global-nodes` 池(`src/global_nodes.rs`);profile 只持有
// section/分组的排序,以及从自身生成缓存解析出的只读代理预览。

/// 生成输出中的一个具名条目(代理或分组)的 `name` + `type` 预览,供「节点预览」「分组预览」使用。
/// 只读;前端通过与自定义节点列表交叉比对来标记哪些名字是可编辑的自定义节点。
#[derive(Serialize)]
struct EntryPreview {
    name: String,
    #[serde(rename = "type")]
    entry_type: String,
}

#[derive(Serialize)]
struct ProxiesResponse {
    /// 是否已存在生成缓存(机场节点从中解析)。
    generated: bool,
    generated_at: Option<String>,
    /// 全部输出代理的有序列表(机场块 + 自定义块,按 `node_section_order`)。前端按自定义节点
    /// 名集合区分机场与自定义。
    proxies: Vec<EntryPreview>,
    /// 两个节点块的顺序(默认 `["provider","custom"]`),使节点预览在首次生成前也能渲染块顺序。
    node_section_order: Vec<String>,
    /// 生成输出中的 proxy-groups(name + type),供分组预览与成员建议使用。
    groups: Vec<EntryPreview>,
}

/// 列出最近一次生成输出中的全部代理(机场代理 + 并入的自定义节点)与全部分组。从
/// `generated_cache.output_yaml` 解析;profile 从未生成过时返回 `generated: false` 与空列表。
pub async fn list_proxies_and_groups(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let _ = load_profile_row(&state, &id).await?;
    let cache = sqlx::query_as::<_, (String, String)>(
        "SELECT output_yaml, generated_at FROM generated_cache WHERE profile_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;

    let section_order = section_order(&state, &id).await?;

    let Some((output_yaml, generated_at)) = cache else {
        return Ok(Json(ProxiesResponse {
            generated: false,
            generated_at: None,
            proxies: Vec::new(),
            node_section_order: section_order,
            groups: Vec::new(),
        }));
    };

    // 输出是可信的(我们自己产生)且已分块/排序(任何排序编辑都经 `regenerate_from_cache` 重生成缓存),
    // 故原样返回。仍走限界解析器解析;遇到任何意外时优雅降级。
    let (proxies, groups) = yaml::parse_limited(&output_yaml)
        .ok()
        .map(|v| {
            (
                extract_previews(&v, "proxies"),
                extract_previews(&v, "proxy-groups"),
            )
        })
        .unwrap_or_default();

    Ok(Json(ProxiesResponse {
        generated: true,
        generated_at: Some(generated_at),
        proxies,
        node_section_order: section_order,
        groups,
    }))
}

/// 从生成输出的某顶层序列(`proxies` 或 `proxy-groups`)提取 `name` + `type` 预览。
fn extract_previews(root: &serde_yaml::Value, key: &str) -> Vec<EntryPreview> {
    match root.get(key) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|item| {
                let name = item.get("name").and_then(|v| v.as_str())?.to_string();
                let entry_type = item
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                Some(EntryPreview { name, entry_type })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// 请求针对哪一列手动排序。映射到固定列名,使 SQL 永不由调用方输入拼接。
#[derive(Clone, Copy)]
pub(crate) enum OrderKind {
    Group,
    /// 两个节点块的顺序(`node_section_order`):provider / custom。
    Section,
}

/// 解析存储的 `node_order`/`group_order` JSON 数组;NULL 或异常值返回空列表(= 默认顺序)。
/// 生成(`src/generate.rs`)与预览共用此实现。
pub(crate) fn parse_order(stored: Option<String>) -> Vec<String> {
    stored
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

/// 读取 profile 持久化的手动顺序(proxy-group 或 section 名)。缺失/NULL 或异常 JSON 返回空列表
/// (= 默认顺序)。
pub(crate) async fn load_order(
    state: &AppState,
    profile_id: &str,
    kind: OrderKind,
) -> ApiResult<Vec<String>> {
    let sql = match kind {
        OrderKind::Group => "SELECT group_order FROM profiles WHERE id = ?",
        OrderKind::Section => "SELECT node_section_order FROM profiles WHERE id = ?",
    };
    Ok(parse_order(
        sqlx::query_scalar::<_, Option<String>>(sql)
            .bind(profile_id)
            .fetch_optional(&state.db)
            .await?
            .flatten(),
    ))
}

/// 已保存的 node-section 顺序,或默认 `["provider","custom"]`。
async fn section_order(state: &AppState, profile_id: &str) -> ApiResult<Vec<String>> {
    let saved = load_order(state, profile_id, OrderKind::Section).await?;
    Ok(if saved.is_empty() {
        vec!["provider".to_string(), "custom".to_string()]
    } else {
        saved
    })
}

#[derive(Deserialize)]
pub struct OrderBody {
    /// 有序的名字(provider + custom)。profile 中不存在的名字在生成时忽略;空数组清除手动顺序。
    order: Vec<String>,
}

/// 为给定列校验并持久化一个手动顺序。同时驱动生成输出(离线重生成后立即应用)与预览列表。
async fn set_order(
    state: &AppState,
    id: &str,
    kind: OrderKind,
    body: OrderBody,
) -> ApiResult<Json<SavedResponse>> {
    let _ = load_profile_row(state, id).await?;

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

    // 空列表清除手动顺序(回到默认);存 NULL。
    let stored = if body.order.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&body.order)
                .map_err(|_| ApiError::BadRequest("order could not be serialized".into()))?,
        )
    };

    let sql = match kind {
        OrderKind::Group => "UPDATE profiles SET group_order = ?, updated_at = ? WHERE id = ?",
        OrderKind::Section => {
            "UPDATE profiles SET node_section_order = ?, updated_at = ? WHERE id = ?"
        }
    };
    sqlx::query(sql)
        .bind(&stored)
        .bind(now())
        .bind(id)
        .execute(&state.db)
        .await?;

    // 用缓存的机场原文离线重生成(不重拉机场),把新顺序立即应用到所服务的订阅;尽力而为。
    Ok(Json(SavedResponse {
        regenerate: regenerate_from_cache(state, id).await,
    }))
}

/// `PUT /api/profiles/:id/group-order` —— 持久化一个手动 proxy-group 顺序。
pub async fn set_group_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<OrderBody>,
) -> ApiResult<impl IntoResponse> {
    set_order(&state, &id, OrderKind::Group, body).await
}

/// `PUT /api/profiles/:id/node-section-order` —— 持久化两个节点块的顺序。请求体必须恰是
/// `["provider","custom"]` 的一个排列。
pub async fn set_node_section_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<OrderBody>,
) -> ApiResult<impl IntoResponse> {
    let valid = body.order.len() == 2
        && body.order.contains(&"provider".to_string())
        && body.order.contains(&"custom".to_string());
    if !valid {
        return Err(ApiError::BadRequest(
            "order must be a permutation of [\"provider\",\"custom\"]".into(),
        ));
    }
    set_order(&state, &id, OrderKind::Section, body).await
}

// ─── 自定义分组 ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct GroupBody {
    name: String,
    group_type: String,
    members: Vec<String>,
    options: Option<JsonValue>,
    enabled: Option<bool>,
}

fn validate_group(body: &GroupBody) -> ApiResult<()> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    if !GROUP_TYPES.contains(&body.group_type.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "group_type must be one of {GROUP_TYPES:?}"
        )));
    }
    if let Some(options) = &body.options {
        if !options.is_object() {
            return Err(ApiError::BadRequest("options must be a JSON object".into()));
        }
    }
    Ok(())
}

pub async fn list_groups(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let _ = load_profile_row(&state, &id).await?;
    let groups: Vec<GroupResponse> = sqlx::query_as::<_, GroupRow>(
        "SELECT id, name, group_type, members, options, enabled, created_at, updated_at
         FROM custom_groups WHERE profile_id = ? ORDER BY created_at ASC",
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(group_response)
    .collect();
    Ok(Json(groups))
}

pub async fn create_group(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<GroupBody>,
) -> ApiResult<impl IntoResponse> {
    let _ = load_profile_row(&state, &id).await?;
    validate_group(&body)?;
    let group_id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    sqlx::query(
        "INSERT INTO custom_groups (id, profile_id, name, group_type, members, options, enabled, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&group_id)
    .bind(&id)
    .bind(body.name.trim())
    .bind(&body.group_type)
    .bind(serde_json::to_string(&body.members).unwrap_or_else(|_| "[]".into()))
    .bind(body.options.as_ref().map(|o| o.to_string()))
    .bind(body.enabled.unwrap_or(true))
    .bind(&ts)
    .bind(&ts)
    .execute(&state.db)
    .await?;
    let row = fetch_group(&state, &id, &group_id).await?;
    Ok((StatusCode::CREATED, Json(group_response(row))))
}

#[derive(Serialize)]
pub struct ImportGroupsResponse {
    imported: usize,
    /// 因同名已存在或类型不支持而跳过的数量。
    skipped: usize,
}

/// `POST /api/profiles/:id/import-provider-groups` —— 拉取机场订阅,把其 `proxy-groups` 导入为
/// 可编辑的自定义分组(否则转换器会像对规则那样替换机场分组)。跳过同名已存在或类型不支持的分组。
/// 实时、SSRF 保护的拉取(同 `provider-rules`);不缓存。在下次生成后生效。
pub async fn import_provider_groups(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let profile = load_profile_row(&state, &id).await?;
    let fetched = state
        .fetcher
        .fetch(&profile.source_url)
        .await
        .map_err(|e| ApiError::Upstream(e.status_label()))?;
    let root = yaml::parse_limited(&fetched.body)
        .map_err(|_| ApiError::Upstream("provider_parse".to_string()))?;
    let provider_groups = match root.get("proxy-groups") {
        Some(serde_yaml::Value::Sequence(items)) => items.clone(),
        _ => Vec::new(),
    };

    // 已占用的分组名:起始为 DB 中已存在的,循环中把本批新导入的也计入,以做批内去重。
    let mut taken_names: std::collections::HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT name FROM custom_groups WHERE profile_id = ?")
            .bind(&id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();

    let mut imported = 0usize;
    let mut skipped = 0usize;
    for item in &provider_groups {
        let Some((name, group_type, members, options)) = parse_provider_group(item) else {
            skipped += 1;
            continue;
        };
        if !taken_names.insert(name.clone()) {
            skipped += 1; // name already present
            continue;
        }
        let ts = now();
        sqlx::query(
            "INSERT INTO custom_groups (id, profile_id, name, group_type, members, options, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .bind(&name)
        .bind(&group_type)
        .bind(serde_json::to_string(&members).unwrap_or_else(|_| "[]".into()))
        .bind(options.as_ref().map(|o| o.to_string()))
        .bind(true)
        .bind(&ts)
        .bind(&ts)
        .execute(&state.db)
        .await?;
        imported += 1;
    }

    Ok(Json(ImportGroupsResponse { imported, skipped }))
}

/// 把一个机场 `proxy-groups` 条目解析为自定义分组 `(name, type, members, options)`。条目缺 name/type
/// 或类型不支持时返回 `None`。`options` 收集除 name/type/proxies 之外的每个键。
fn parse_provider_group(
    item: &serde_yaml::Value,
) -> Option<(String, String, Vec<String>, Option<JsonValue>)> {
    let m = item.as_mapping()?;
    let name = m.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let group_type = m.get("type")?.as_str()?.to_string();
    if !GROUP_TYPES.contains(&group_type.as_str()) {
        return None;
    }
    let members = m
        .get("proxies")
        .and_then(|v| v.as_sequence())
        .map(|s| {
            s.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut opts = serde_json::Map::new();
    for (k, v) in m {
        let Some(ks) = k.as_str() else { continue };
        if matches!(ks, "name" | "type" | "proxies") {
            continue;
        }
        if let Ok(jv) = serde_json::to_value(v) {
            opts.insert(ks.to_string(), jv);
        }
    }
    let options = (!opts.is_empty()).then_some(JsonValue::Object(opts));
    Some((name, group_type, members, options))
}

pub async fn update_group(
    State(state): State<Arc<AppState>>,
    Path((id, group_id)): Path<(String, String)>,
    Json(body): Json<GroupBody>,
) -> ApiResult<impl IntoResponse> {
    let _ = fetch_group(&state, &id, &group_id).await?;
    validate_group(&body)?;
    sqlx::query(
        "UPDATE custom_groups SET name = ?, group_type = ?, members = ?, options = ?, enabled = ?, updated_at = ?
         WHERE id = ? AND profile_id = ?",
    )
    .bind(body.name.trim())
    .bind(&body.group_type)
    .bind(serde_json::to_string(&body.members).unwrap_or_else(|_| "[]".into()))
    .bind(body.options.as_ref().map(|o| o.to_string()))
    .bind(body.enabled.unwrap_or(true))
    .bind(now())
    .bind(&group_id)
    .bind(&id)
    .execute(&state.db)
    .await?;
    let row = fetch_group(&state, &id, &group_id).await?;
    Ok(Json(group_response(row)))
}

pub async fn delete_group(
    State(state): State<Arc<AppState>>,
    Path((id, group_id)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let result = sqlx::query("DELETE FROM custom_groups WHERE id = ? AND profile_id = ?")
        .bind(&group_id)
        .bind(&id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 按 (group_id, profile_id) 双限定读取分组,防止用他人 profile 的 id 访问/操作其分组。
async fn fetch_group(state: &AppState, profile_id: &str, group_id: &str) -> ApiResult<GroupRow> {
    sqlx::query_as::<_, GroupRow>(
        "SELECT id, name, group_type, members, options, enabled, created_at, updated_at
         FROM custom_groups WHERE id = ? AND profile_id = ?",
    )
    .bind(group_id)
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}
