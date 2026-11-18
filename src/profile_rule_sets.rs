//! Per-profile 自包含规则库(③ 托管规则库)。
//!
//! 每个订阅持有自己的规则集定义;`RULE-SET,<name>,<policy>` 规则按名引用,生成时由
//! `src/generate.rs` 注入到输出 `rule-providers:`,`url` 指向 **按订阅 token 隔离** 的托管链接
//! `/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>`。生成只读本表。从全局库导入时
//! 保留来源 ID，全局编辑同步到关联的订阅。
//!
//! 两种来源与全局库一致(见 `src/rulelib.rs`):`manual` 托管录入 payload;`remote` 镜像远程规则集
//! (`cache=1` 懒刷新二次托管,支持二进制 `mrs`;`cache=0` 转换时直接注入上游 URL)。
//!
//! 写操作(增删改/导入)在与生成/公开刷新共用的 per-profile 锁内完成读改写,改动需要重写所服务
//! 输出时在锁内离线重生成(`generate::*_locked`,不重入锁)。被 `RULE-SET` 规则引用(含产出当前
//! 缓存的规则快照)的规则集不可改名/停用/删除(409),避免正在下发的配置失去定义。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{rejection::PathRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow, SqliteConnection, SqliteExecutor};

use crate::app::AppState;
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::generate::Regenerated;
use crate::mask;
use crate::rulelib::{self, RuleSetBody};
use crate::util::{completed_since, is_fresh, now};

// ─── 行 / 响应 ──────────────────────────────────────────────────────────────────

/// 列表 / CRUD 用的元信息行(不含 BLOB `cached_body`)。
#[derive(FromRow)]
struct RuleSetRow {
    id: String,
    global_rule_set_id: Option<String>,
    pending_global_name: Option<String>,
    name: String,
    behavior: String,
    format: String,
    source: String,
    content: String,
    rule_count: i64,
    url: Option<String>,
    interval_hours: i64,
    cache: bool,
    last_fetch_status: Option<String>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

// 列名常量,仅经 `AssertSqlSafe(format!(..))` 拼入 SQL,不含外部输入。
const META_COLS: &str =
    "id, global_rule_set_id, pending_global_name, name, behavior, format, source, content, rule_count, url, \
     interval_hours, cache, last_fetch_status, enabled, created_at, updated_at";

#[derive(Serialize)]
pub struct ProfileRuleSetResponse {
    id: String,
    global_rule_set_id: Option<String>,
    pending_global_name: Option<String>,
    name: String,
    behavior: String,
    format: String,
    source: String,
    content: String,
    enabled: bool,
    /// 规则条数(manual=payload 行数;remote=最近成功镜像的行数,mrs 为 0)。
    count: i64,
    /// 按订阅 token 隔离的托管链接。
    url: String,
    /// 远程来源 URL(已脱敏);manual 为 null。
    remote_url_masked: Option<String>,
    interval_hours: i64,
    cache: bool,
    last_fetch_status: Option<String>,
    created_at: String,
    updated_at: String,
}

fn to_response(state: &AppState, token: &str, row: RuleSetRow) -> ProfileRuleSetResponse {
    ProfileRuleSetResponse {
        url: state.profile_rule_set_url(token, &row.name, &row.behavior, &row.format),
        remote_url_masked: row.url.as_deref().map(mask::mask_url),
        count: row.rule_count,
        id: row.id,
        global_rule_set_id: row.global_rule_set_id,
        pending_global_name: row.pending_global_name,
        name: row.name,
        behavior: row.behavior,
        format: row.format,
        source: row.source,
        content: row.content,
        enabled: row.enabled,
        interval_hours: row.interval_hours,
        cache: row.cache,
        last_fetch_status: row.last_fetch_status,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// 取 profile 的 token(同时校验 profile 存在);不存在 → 404。
async fn profile_token(state: &AppState, profile_id: &str) -> ApiResult<String> {
    sqlx::query_scalar::<_, String>("SELECT token FROM profiles WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn fetch_one<'e>(
    db: impl SqliteExecutor<'e>,
    profile_id: &str,
    id: &str,
) -> ApiResult<RuleSetRow> {
    sqlx::query_as::<_, RuleSetRow>(AssertSqlSafe(format!(
        "SELECT {META_COLS} FROM profile_rule_sets WHERE id = ? AND profile_id = ?"
    )))
    .bind(id)
    .bind(profile_id)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound)
}

// ─── 引用检查 ────────────────────────────────────────────────────────────────────

/// 规则集引用,按来源分开:当前规则草稿与产出所服务缓存的规则(含逻辑规则内嵌套,由
/// `converter::ruleset_refs` 收集)。被任一来源引用的规则集改名/停用/删除会让正在下发的配置
/// 或下次重生成失去定义,必须拦截(409);只有缓存引用的定义才真正注入在所服务的输出里。
struct RuleSetRefs {
    draft: RuleDraft,
    /// 缓存不存在时为空集;缓存损坏无法判断时为 `None`,按「被引用」保守处理。
    cache: Option<HashSet<String>>,
}

impl RuleSetRefs {
    /// `name` 是否被规则草稿或所服务缓存引用。
    fn contains(&self, name: &str) -> bool {
        self.draft.names.contains(name) || self.in_cache(name)
    }

    /// `name` 是否被产出所服务缓存的规则引用。
    fn in_cache(&self, name: &str) -> bool {
        self.cache.as_ref().is_none_or(|s| s.contains(name))
    }
}

struct RuleDraft {
    content: String,
    names: HashSet<String>,
}

async fn load_rule_draft(conn: &mut SqliteConnection, profile_id: &str) -> ApiResult<RuleDraft> {
    let content =
        sqlx::query_scalar::<_, String>("SELECT content FROM rulesets WHERE profile_id = ?")
            .bind(profile_id)
            .fetch_optional(conn)
            .await?
            .unwrap_or_default();
    let names = crate::converter::ruleset_refs(&content)
        .into_iter()
        .collect();
    Ok(RuleDraft { content, names })
}

/// 收集规则集引用。升级前的缓存无 `rules_snapshot`,改从 `output_yaml` 的 `rules` 段提取。
async fn referenced_names(conn: &mut SqliteConnection, profile_id: &str) -> ApiResult<RuleSetRefs> {
    let draft = load_rule_draft(conn, profile_id).await?;
    // 规则快照缺失(升级前的旧缓存)时才需要整份输出作退路,避免每次读出大块 output_yaml。
    let cache = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT rules_snapshot, CASE WHEN rules_snapshot IS NULL THEN output_yaml END \
         FROM generated_cache WHERE profile_id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&mut *conn)
    .await?;
    let cache = match cache {
        None => Some(HashSet::new()),
        Some((Some(snapshot), _)) => Some(
            crate::converter::ruleset_refs(&snapshot)
                .into_iter()
                .collect(),
        ),
        Some((None, output)) => cached_output_rules(output.as_deref().unwrap_or_default())
            .map(|rules| crate::converter::ruleset_refs(&rules).into_iter().collect()),
    };
    Ok(RuleSetRefs { draft, cache })
}

fn referenced_conflict(name: &str, action: &str) -> ApiError {
    ApiError::Conflict(format!(
        "rule-set `{name}` is referenced by rules; remove the referencing RULE-SET rules before {action}"
    ))
}

/// 从缓存的输出 YAML 提取 `rules` 段文本(升级前无规则快照的缓存的退路);输出不可解析或
/// `rules` 不是字符串序列时返回 `None`(无法判断)。
fn cached_output_rules(output_yaml: &str) -> Option<String> {
    let root = crate::yaml::parse_generated_limited(output_yaml).ok()?;
    let items = match root.get("rules")? {
        serde_yaml::Value::Sequence(items) => items,
        _ => return None,
    };
    let mut lines = Vec::with_capacity(items.len());
    for item in items {
        lines.push(item.as_str()?.to_string());
    }
    Some(lines.join("\n"))
}

// ─── CRUD ──────────────────────────────────────────────────────────────────────

/// `GET /api/profiles/:id/rule-sets` —— 列出该订阅的规则集。
pub async fn list(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let token = profile_token(&state, &profile_id).await?;
    let rows = sqlx::query_as::<_, RuleSetRow>(AssertSqlSafe(format!(
        "SELECT {META_COLS} FROM profile_rule_sets WHERE profile_id = ? ORDER BY name ASC"
    )))
    .bind(&profile_id)
    .fetch_all(&state.db)
    .await?;
    let out: Vec<ProfileRuleSetResponse> = rows
        .into_iter()
        .map(|row| to_response(&state, &token, row))
        .collect();
    Ok(Json(out))
}

/// 新建/更新规则集的响应:最新定义;改动需要重写所服务输出时另带离线重生成结果
/// (见 `generate::Regenerated`),未触发重生成时无 `regenerate` 字段。列表不含该字段。
#[derive(Serialize)]
pub struct RuleSetSavedResponse {
    #[serde(flatten)]
    rule_set: ProfileRuleSetResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    regenerate: Option<Regenerated>,
}

/// `POST /api/profiles/:id/rule-sets` —— 新建。name 在本订阅内唯一冲突 → 409。
/// 若补齐了已被 `RULE-SET` 规则引用的名字(此前因定义缺失而无法注入),创建后离线重生成缓存。
pub async fn create(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
    ApiJson(body): ApiJson<RuleSetBody>,
) -> ApiResult<impl IntoResponse> {
    // 与生成/公开刷新共用 per-profile 锁:引用检查、插入与离线重生成不与该订阅的并发写交错;
    // 锁内只能调 `_locked` 变体,不得重入锁。
    state.keyed_lock.run(&profile_id, async {
        let token = profile_token(&state, &profile_id).await?;
        let n = rulelib::normalize(&body, None)?;
        let name = body.name.trim();
        let referenced = referenced_names(&mut *state.db.acquire().await?, &profile_id).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let ts = now();
        sqlx::query(
            "INSERT INTO profile_rule_sets (id, profile_id, name, behavior, format, source, content, \
             rule_count, url, interval_hours, cache, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&profile_id)
        .bind(name)
        .bind(&body.behavior)
        .bind(&body.format)
        .bind(&n.source)
        .bind(&n.content)
        .bind(n.rule_count)
        .bind(&n.url)
        .bind(n.interval_hours)
        .bind(n.cache)
        .bind(body.enabled.unwrap_or(true))
        .bind(&ts)
        .bind(&ts)
        .execute(&state.db)
        .await?;
        let row = fetch_one(&state.db, &profile_id, &id).await?;
        // 新建只会补上旧缓存里不存在的定义(引用未定义规则集的规则过不了校验),旧缓存的托管
        // 链接都仍有效:重生成未通过时保留它继续下发,不同于 `update` 改动已注入定义时删缓存。
        let regenerate = if referenced.contains(name) {
            Some(crate::generate::regenerate_from_cache_locked(&state, &profile_id).await)
        } else {
            None
        };
        Ok((
            StatusCode::CREATED,
            Json(RuleSetSavedResponse {
                rule_set: to_response(&state, &token, row),
                regenerate,
            }),
        ))
    })
    .await
}

/// `PUT /api/profiles/:id/rule-sets/:rsid` —— 更新。任何更新都清空镜像缓存(下次拉取重新回源)。
/// 被 `RULE-SET` 规则引用(含逻辑规则嵌套,含产出当前缓存的规则快照)的规则集不可改名/停用 → 409。
/// 改动影响输出的字段(托管链接的 behavior/format、注入方式 source/cache、remote 直连 URL、
/// 启停)时提交后离线重生成。定义已注入所服务的缓存(缓存自己的规则引用它)时,同一事务先把
/// `generated_cache.capability_hash` 置 NULL(标脏,崩溃窗口内旧缓存不会被当作兼容缓存下发),
/// 重生成非 Applied(无机场原文或新配置不合法)时删除缓存,下次公开拉取回源重建;定义只被未通过
/// 校验的规则草稿引用时旧缓存不含它,重生成失败也保留。纯 manual 内容/镜像间隔不影响输出,
/// 不做无谓重生成。
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path((profile_id, rsid)): Path<(String, String)>,
    ApiJson(body): ApiJson<RuleSetBody>,
) -> ApiResult<impl IntoResponse> {
    state
        .keyed_lock
        .run(&profile_id, async {
            let token = profile_token(&state, &profile_id).await?;
            let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
            let existing = fetch_one(&mut *tx, &profile_id, &rsid).await?;
            let normalized = rulelib::normalize(&body, existing.url.as_deref())?;
            let impact = DefinitionEdit {
                name: body.name.trim(),
                enabled: body.enabled.unwrap_or(true),
                behavior: &body.behavior,
                format: &body.format,
                values: &normalized,
                global_id: None,
            }
            .apply(&mut tx, &profile_id, &existing, &mut None)
            .await?;
            tx.commit().await?;
            let row = fetch_one(&state.db, &profile_id, &rsid).await?;
            let regenerate = impact.regenerate_locked(&state, &profile_id).await?;
            Ok(Json(RuleSetSavedResponse {
                rule_set: to_response(&state, &token, row),
                regenerate,
            }))
        })
        .await
}

#[derive(Default)]
pub(crate) struct CacheImpact {
    regenerate: bool,
    invalidate: bool,
}

impl CacheImpact {
    fn merge(&mut self, other: Self) {
        self.regenerate |= other.regenerate;
        self.invalidate |= other.invalidate;
    }

    pub(crate) async fn regenerate_locked(
        &self,
        state: &AppState,
        profile_id: &str,
    ) -> ApiResult<Option<Regenerated>> {
        if !self.regenerate {
            return Ok(None);
        }
        let result = crate::generate::regenerate_from_cache_locked(state, profile_id).await;
        if self.invalidate && !matches!(result, Regenerated::Applied) {
            crate::generate::invalidate_cache(state, profile_id).await?;
        }
        Ok(Some(result))
    }
}

struct DefinitionEdit<'a> {
    name: &'a str,
    enabled: bool,
    behavior: &'a str,
    format: &'a str,
    values: &'a rulelib::Normalized,
    global_id: Option<&'a str>,
}

impl DefinitionEdit<'_> {
    async fn apply(
        &self,
        conn: &mut SqliteConnection,
        profile_id: &str,
        old: &RuleSetRow,
        batch_refs: &mut Option<RuleSetRefs>,
    ) -> ApiResult<CacheImpact> {
        let n = self.values;
        let mut impact = CacheImpact::default();
        let entry_changed = self.behavior != old.behavior
            || self.format != old.format
            || n.source != old.source
            || n.cache != old.cache
            || (n.source == "remote" && !n.cache && n.url != old.url);
        if self.name != old.name || self.enabled != old.enabled || entry_changed {
            let refs = match batch_refs {
                Some(refs) => refs,
                None => batch_refs.insert(referenced_names(conn, profile_id).await?),
            };
            let was_referenced = refs.contains(&old.name);
            if was_referenced && self.name != old.name {
                return Err(referenced_conflict(&old.name, "renaming"));
            }
            if was_referenced && old.enabled && !self.enabled {
                return Err(referenced_conflict(&old.name, "disabling"));
            }
            let had_entry = was_referenced && old.enabled;
            let has_entry = refs.contains(self.name) && self.enabled;
            impact.regenerate = had_entry != has_entry || (had_entry && has_entry && entry_changed);
            impact.invalidate = impact.regenerate && old.enabled && refs.in_cache(&old.name);
        }
        if impact.invalidate {
            sqlx::query("UPDATE generated_cache SET capability_hash = NULL WHERE profile_id = ?")
                .bind(profile_id)
                .execute(&mut *conn)
                .await?;
        }
        sqlx::query(
            "UPDATE profile_rule_sets SET name = ?, behavior = ?, format = ?, source = ?, content = ?,
             rule_count = ?, url = ?, interval_hours = ?, cache = ?, enabled = ?, global_rule_set_id = ?, pending_global_name = NULL,
             cached_body = NULL, cached_at = NULL, last_fetch_at = NULL, last_fetch_status = NULL,
             revision = revision + 1, updated_at = ? WHERE id = ? AND profile_id = ?",
        )
        .bind(self.name)
        .bind(self.behavior)
        .bind(self.format)
        .bind(&n.source)
        .bind(&n.content)
        .bind(n.rule_count)
        .bind(&n.url)
        .bind(n.interval_hours)
        .bind(n.cache)
        .bind(self.enabled)
        .bind(self.global_id)
        .bind(now())
        .bind(&old.id)
        .bind(profile_id)
        .execute(conn)
        .await?;
        Ok(impact)
    }
}

/// 在全局定义的写事务及本订阅锁内同步。订阅内名字和启停状态由订阅自己持有。
pub(crate) async fn sync_from_global(
    conn: &mut SqliteConnection,
    profile_id: &str,
    global_id: &str,
    body: &RuleSetBody,
    values: &rulelib::Normalized,
) -> ApiResult<CacheImpact> {
    let rows = sqlx::query_as::<_, RuleSetRow>(AssertSqlSafe(format!(
        "SELECT {META_COLS} FROM profile_rule_sets WHERE profile_id = ? AND global_rule_set_id = ?"
    )))
    .bind(profile_id)
    .bind(global_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut impact = CacheImpact::default();
    let mut batch_refs = None;
    for row in rows {
        let edit = DefinitionEdit {
            name: &row.name,
            enabled: row.enabled,
            behavior: &body.behavior,
            format: &body.format,
            values,
            global_id: Some(global_id),
        };
        impact.merge(edit.apply(conn, profile_id, &row, &mut batch_refs).await?);
    }
    Ok(impact)
}

/// `DELETE /api/profiles/:id/rule-sets/:rsid` —— 删除。被 `RULE-SET` 规则引用(同上,含快照)
/// 的规则集不可删除 → 409;先移除引用它的规则行。未被引用的删除不影响输出,无需重生成。
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path((profile_id, rsid)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    state
        .keyed_lock
        .run(&profile_id, async {
            let existing = fetch_one(&state.db, &profile_id, &rsid).await?;
            let referenced = referenced_names(&mut *state.db.acquire().await?, &profile_id).await?;
            if referenced.contains(&existing.name) {
                return Err(referenced_conflict(&existing.name, "deleting"));
            }
            sqlx::query("DELETE FROM profile_rule_sets WHERE id = ? AND profile_id = ?")
                .bind(&rsid)
                .bind(&profile_id)
                .execute(&state.db)
                .await?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

// ─── 从 ② 全局库导入 ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ImportBody {
    /// 要从全局「规则托管」库复制进本订阅的规则集名。
    names: Vec<String>,
    /// 对已有独立定义，仅在显式指定名称及订阅内 ID 时替换并关联。
    #[serde(default)]
    link_existing: HashMap<String, String>,
    /// 新客户端为每个勾选项提供弹窗中看到的来源与本地定义身份。
    expected: Option<HashMap<String, ExpectedImport>>,
    /// 复制后追加的 `RULE-SET` 规则统一指向的策略。
    policy: String,
}

#[derive(Deserialize)]
struct ExpectedImport {
    global_id: String,
    local_id: Option<String>,
}

#[derive(FromRow)]
struct ImportLocal {
    id: String,
    name: String,
    enabled: bool,
    global_rule_set_id: Option<String>,
}

impl ExpectedImport {
    fn verify(
        &self,
        global_id: Option<&str>,
        local: Option<&ImportLocal>,
        replace_independent: bool,
    ) -> ApiResult<()> {
        let expected_source = if replace_independent {
            None
        } else {
            Some(self.global_id.as_str())
        };
        if global_id != Some(&self.global_id)
            || self.local_id.as_deref() != local.map(|row| row.id.as_str())
            || local.is_some_and(|row| {
                !row.enabled || row.global_rule_set_id.as_deref() != expected_source
            })
        {
            return Err(ApiError::Conflict(
                "rule-set selection changed; reopen the import dialog and retry".into(),
            ));
        }
        Ok(())
    }
}

async fn import_local(
    conn: &mut SqliteConnection,
    profile_id: &str,
    global_id: Option<&str>,
    name: &str,
) -> ApiResult<Option<ImportLocal>> {
    // 已关联的订阅别名优先。历史库若有多个别名，优先同源同名，否则取按名称排序的第一项。
    Ok(sqlx::query_as(
        "SELECT id, name, enabled, global_rule_set_id FROM profile_rule_sets
         WHERE profile_id = ?1 AND name = COALESCE((
             SELECT name FROM profile_rule_sets WHERE profile_id = ?1 AND global_rule_set_id = ?2
             ORDER BY (name = ?3) DESC, name LIMIT 1
         ), ?3)",
    )
    .bind(profile_id)
    .bind(global_id)
    .bind(name)
    .fetch_optional(conn)
    .await?)
}

#[derive(Serialize)]
pub struct ImportResult {
    /// 本次新导入或显式建立关联的规则集数。
    imported: usize,
    /// 新导入定义、补规则行或替换影响输出的字段时的离线重生成结果。
    regenerate: Option<Regenerated>,
    /// 导入提交后的完整规则文本,供前端同步本地编辑状态。
    rules_content: String,
}

/// 从全局 ② 库复制一行所需的字段(含 `rule_count`,直接沿用,免重算)。
#[derive(FromRow)]
struct GlobalRuleSet {
    id: String,
    name: String,
    behavior: String,
    format: String,
    #[sqlx(flatten)]
    values: rulelib::Normalized,
}

/// 旧全局库可能含有新表单不再接受的定义,导入前按当前 Mihomo 组合与 URL 语法复核。
fn importable(rule_set: &GlobalRuleSet) -> bool {
    if rule_set.behavior == "classical" && rule_set.format == "mrs" {
        return false;
    }
    match rule_set.values.source.as_str() {
        "manual" => rulelib::MANUAL_FORMATS.contains(&rule_set.format.as_str()),
        "remote" => rule_set
            .values
            .url
            .as_deref()
            .and_then(rulelib::parse_http_url)
            .is_some(),
        _ => false,
    }
}

/// 单次导入的名字数上限:防止超大请求放大为逐名查询。
const MAX_IMPORT_NAMES: usize = 1000;

/// 导入全局定义并关联来源，补齐尚未引用的规则行。同名定义默认沿用；显式指定本订阅的
/// 独立定义 ID 时才替换其内容并关联。定义和规则行在一个事务中提交。
pub async fn import(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
    ApiJson(body): ApiJson<ImportBody>,
) -> ApiResult<impl IntoResponse> {
    // policy 原样拼进 `RULE-SET,<name>,<policy>` 行:必须是单个非空名字,逗号或换行会拼出畸形/额外的
    // 规则行。名字同理逐个复用规则集名校验。两者在任何写入与取锁之前校验。
    let policy = body.policy.trim();
    if policy.is_empty() || policy.contains(',') || policy.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "policy must be a single non-empty policy name".into(),
        ));
    }
    let names = normalize_names(&body.names)?;
    if let Some(expected) = &body.expected {
        if expected.len() != names.len() || !names.iter().all(|name| expected.contains_key(name)) {
            return Err(ApiError::BadRequest(
                "expected must cover exactly the selected names".into(),
            ));
        }
    }

    // 与生成/公开刷新共用 per-profile 锁;锁内只能调 `_locked` 变体,不得重入锁。
    state.keyed_lock.run(&profile_id, async {
        let _ = profile_token(&state, &profile_id).await?;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;

        // `available` 收集复制完成后 ③ 中实际存在且启用的名(保序),只有它们才会得到规则行——
        // 生成只认启用的定义,为停用的定义追加引用会让规则无法通过校验。
        let mut imported = 0usize;
        let mut available = Vec::new();
        let mut impact = CacheImpact::default();
        let mut batch_refs = None;
        for name in &names {
            let global_id = sqlx::query_scalar::<_, String>("SELECT id FROM rule_sets WHERE name = ?")
                .bind(name).fetch_optional(&mut *tx).await?;
            let local = import_local(&mut tx, &profile_id, global_id.as_deref(), name).await?;
            let link_id = body.link_existing.get(name);
            if let Some(expected) = &body.expected {
                expected[name].verify(global_id.as_deref(), local.as_ref(), link_id.is_some())?;
            }
            if let Some(id) = link_id {
                if local.as_ref().is_none_or(|row| row.id != *id || !row.enabled) {
                    return Err(ApiError::Conflict("local rule-set changed; reload and retry".into()));
                }
            } else if let Some(row) = local {
                if row.enabled {
                    available.push(row.name);
                }
                continue;
            }
            let global = sqlx::query_as::<_, GlobalRuleSet>(
                "SELECT id, name, behavior, format, source, content, url, interval_hours, cache, rule_count
                 FROM rule_sets WHERE id = ?",
            )
            .bind(global_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(g) = global.filter(importable) else {
                if link_id.is_some() || body.expected.is_some() {
                    return Err(ApiError::Conflict("global rule-set is no longer available".into()));
                }
                continue;
            };
            if let Some(local) = local {
                available.push(local.name);
                if local.global_rule_set_id.as_ref() == Some(&g.id) {
                    continue;
                }
                if local.global_rule_set_id.is_some() {
                    return Err(ApiError::Conflict("local rule-set already follows another definition".into()));
                }
                let row = fetch_one(&mut *tx, &profile_id, &local.id).await?;
                let edit = DefinitionEdit {
                    name: &row.name,
                    enabled: row.enabled,
                    behavior: &g.behavior,
                    format: &g.format,
                    values: &g.values,
                    global_id: Some(&g.id),
                };
                impact.merge(edit.apply(&mut tx, &profile_id, &row, &mut batch_refs).await?);
                imported += 1;
                continue;
            }
            let id = uuid::Uuid::new_v4().to_string();
            let ts = now();
            sqlx::query(
                "INSERT INTO profile_rule_sets (id, profile_id, name, behavior, format, source, content, \
                 rule_count, url, interval_hours, cache, enabled, global_rule_set_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&profile_id)
            .bind(&g.name)
            .bind(&g.behavior)
            .bind(&g.format)
            .bind(&g.values.source)
            .bind(&g.values.content)
            .bind(g.values.rule_count)
            .bind(&g.values.url)
            .bind(g.values.interval_hours)
            .bind(g.values.cache)
            .bind(&g.id)
            .bind(&ts)
            .bind(&ts)
            .execute(&mut *tx)
            .await?;
            imported += 1;
            impact.regenerate = true;
            available.push(g.name);
        }

        // 追加规则行:仅为复制后实际存在且尚未被 RULE-SET 引用的名追加,统一指向 `policy`。
        let RuleDraft { content: old_content, mut names } = match batch_refs {
            Some(refs) => refs.draft,
            None => load_rule_draft(&mut tx, &profile_id).await?,
        };
        let lines: Vec<String> = available.into_iter()
            .filter(|name| names.insert(name.clone()))
            .map(|name| format!("RULE-SET,{name},{policy}"))
            .collect();
        let rules_changed = !lines.is_empty();
        let content = if rules_changed {
            // 客户端按规则顺序匹配:新行必须插到兜底 MATCH 之前(编辑器同样把 MATCH 钉底),
            // 追加在 MATCH 后的规则永远不会命中。
            insert_before_fallback_match(&old_content, &lines)
        } else {
            old_content.clone()
        };
        if rules_changed {
            // 带旧内容的 CAS:规则被并发修改过时整体回滚(定义复制一并撤销),调用方重试即可。
            let updated = sqlx::query(
                "UPDATE rulesets SET content = ?, updated_at = ? WHERE profile_id = ? AND content = ?",
            )
            .bind(&content)
            .bind(now())
            .bind(&profile_id)
            .bind(&old_content)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 0 {
                return Err(ApiError::Conflict(
                    "rules were changed concurrently; please retry".into(),
                ));
            }
        }
        tx.commit().await?;

        impact.regenerate |= rules_changed;
        let regenerate = impact.regenerate_locked(&state, &profile_id).await?;
        Ok(Json(ImportResult {
            imported,
            regenerate,
            rules_content: content,
        }))
    })
    .await
}

/// 把新规则行插到最后一条 MATCH 兜底行之前;没有 MATCH 时追加到末尾。
fn insert_before_fallback_match(content: &str, new_lines: &[String]) -> String {
    let mut lines: Vec<&str> = content.lines().collect();
    let pos = lines
        .iter()
        .rposition(|l| crate::converter::is_match_line(l))
        .unwrap_or(lines.len());
    lines.splice(pos..pos, new_lines.iter().map(String::as_str));
    lines.join("\n")
}

/// trim 每个名字、丢弃空串;其余名字逐个复用规则集名校验(与 ③ 定义同一约束,非法 → 400),
/// HashSet 保序去重。数量上限见 [`MAX_IMPORT_NAMES`]。
fn normalize_names(names: &[String]) -> ApiResult<Vec<String>> {
    if names.len() > MAX_IMPORT_NAMES {
        return Err(ApiError::BadRequest(format!(
            "names must have at most {MAX_IMPORT_NAMES} entries"
        )));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        rulelib::validate_name(n)?;
        if seen.insert(n.to_string()) {
            out.push(n.to_string());
        }
    }
    Ok(out)
}

// ─── 公开托管端点 ───────────────────────────────────────────────────────────────

/// 托管渲染所需的行(含 BLOB `cached_body`)。
#[derive(FromRow)]
struct ServeRow {
    id: String,
    name: String,
    enabled: bool,
    revision: i64,
    last_fetch_at: Option<String>,
    behavior: String,
    format: String,
    source: String,
    content: String,
    url: Option<String>,
    interval_hours: i64,
    cache: bool,
    cached_body: Option<Vec<u8>>,
    cached_at: Option<String>,
}

// 列名常量,仅经 `AssertSqlSafe(format!(..))` 拼入 SQL,不含外部输入。
const SERVE_COLS: &str =
    "id, name, enabled, revision, last_fetch_at, behavior, format, source, content, url, interval_hours, cache, cached_body, cached_at";

/// `GET /:public_path_prefix/api/sub/:token/r/:name/:file` —— 公开托管本订阅的规则集内容。无鉴权;
/// 按 token→订阅、再按 `(profile_id, name)` 定位,统一 404(路径段无法解码 / 前缀错 / token 错 /
/// 名不存在 / 未启用 / 文件名不符 / remote 未托管 一律 404)。`:file` 必须等于 `<behavior>.<format>`。
/// 规则集是规则清单、非私密,按名可枚举可接受(见 `docs/architecture.md`「安全设计」),
/// 由 IP 限流抑制枚举。
pub async fn public_serve(
    State(state): State<Arc<AppState>>,
    path: Result<Path<(String, String, String, String)>, PathRejection>,
) -> Response {
    let Ok(Path((prefix, token, name, file))) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(profile_id) = public_profile(&state, &prefix, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let row = match fetch_serve_by_name(&state, &profile_id, &name).await {
        Ok(Some(r)) => r,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if file != format!("{}.{}", row.behavior, row.format) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let response = match row.source.as_str() {
        "manual" => rulelib::serve_bytes(
            &row.format,
            rulelib::render_manual(&row.content, &row.format).into_bytes(),
        ),
        // remote + cache:面板二次托管;cache 关时不托管(转换时直接注入上游 URL)→ 404。
        "remote" if row.cache => serve_remote(&state, row, &prefix, &token, &name, &file).await,
        _ => StatusCode::NOT_FOUND.into_response(),
    };
    // 回源或等锁期间可能发生 token/前缀轮换,交付内容前重新校验。
    if public_profile(&state, &prefix, &token).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    response
}

async fn public_profile(state: &AppState, prefix: &str, token: &str) -> Option<String> {
    state
        .public_gate(prefix, async {
            sqlx::query_scalar::<_, String>("SELECT id FROM profiles WHERE token = ?")
                .bind(token)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten()
        })
        .await
}

async fn fetch_serve_by_name(
    state: &AppState,
    profile_id: &str,
    name: &str,
) -> ApiResult<Option<ServeRow>> {
    Ok(sqlx::query_as::<_, ServeRow>(AssertSqlSafe(format!(
        "SELECT {SERVE_COLS} FROM profile_rule_sets WHERE profile_id = ? AND name = ? AND enabled = 1"
    )))
    .bind(profile_id)
    .bind(name)
    .fetch_optional(&state.db)
    .await?)
}

async fn fetch_serve_by_id(state: &AppState, id: &str) -> ApiResult<Option<ServeRow>> {
    Ok(sqlx::query_as::<_, ServeRow>(AssertSqlSafe(format!(
        "SELECT {SERVE_COLS} FROM profile_rule_sets WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(&state.db)
    .await?)
}

/// 远程镜像的懒刷新托管:单飞合并并发;缓存超 `interval_hours` 才回源拉取(SSRF 安全字节);拉取失败
/// 回退旧缓存,无缓存则 `503`。作用于 per-profile 表(③);全局库 ② 不再对外托管。
async fn serve_remote(
    state: &AppState,
    original: ServeRow,
    prefix: &str,
    token: &str,
    name: &str,
    file: &str,
) -> Response {
    let arrived = now();
    let key = format!("profile-ruleset:{}", original.id);
    state
        .keyed_lock
        .run(&key, async {
            // 在锁内重新读取 URL 和完整托管条件;排队时换源/改名/禁用的请求不能沿用旧快照。
            let Some(mut row) = fetch_serve_by_id(state, &original.id).await.ok().flatten() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if !row.enabled
                || row.name != name
                || file != format!("{}.{}", row.behavior, row.format)
                || row.source != "remote"
                || !row.cache
                || public_profile(state, prefix, token).await.is_none()
            {
                return StatusCode::NOT_FOUND.into_response();
            }
            let Some(url) = row.url.as_deref() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let fresh = row
                .cached_at
                .as_deref()
                .is_some_and(|at| is_fresh(at, mirror_ttl(row.interval_hours)));
            // 失败短缓存 30 秒;同批等待者即使等待超过此间隔仍共享已经完成的结果。
            let recent_attempt = row.last_fetch_at.as_deref().is_some_and(|at| {
                is_fresh(at, Duration::from_secs(30)) || completed_since(at, &arrived)
            });
            if fresh || recent_attempt {
                return match row.cached_body.take() {
                    Some(body) => rulelib::serve_bytes(&row.format, body),
                    None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
            }
            match state.fetcher.fetch_bytes(url).await {
                Ok(bytes) => {
                    let count = rulelib::body_count(&bytes, &row.format);
                    match persist_remote_cache(state, &row, &bytes, count).await {
                        Ok(true) => rulelib::serve_bytes(&row.format, bytes),
                        Ok(false) => StatusCode::NOT_FOUND.into_response(), // 定义已变,丢弃旧回源结果
                        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    }
                }
                Err(e) => match update_fetch_status(state, &row, &e.status_label()).await {
                    Ok(true) => match row.cached_body {
                        Some(body) => rulelib::serve_bytes(&row.format, body),
                        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    },
                    Ok(false) => StatusCode::NOT_FOUND.into_response(),
                    Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                },
            }
        })
        .await
}

/// 镜像回源的最小间隔:`interval_hours` 转 `Duration`,下限 1 小时(与校验一致)。
fn mirror_ttl(interval_hours: i64) -> Duration {
    Duration::from_secs((interval_hours.max(1) as u64).saturating_mul(3600))
}

async fn persist_remote_cache(
    state: &AppState,
    row: &ServeRow,
    bytes: &[u8],
    count: i64,
) -> ApiResult<bool> {
    let completed = now();
    let result = sqlx::query(
        "UPDATE profile_rule_sets SET cached_body = ?, cached_at = ?, last_fetch_at = ?, last_fetch_status = 'success', \
         rule_count = ? WHERE id = ? AND revision = ?",
    )
    .bind(bytes).bind(&completed).bind(&completed).bind(count).bind(&row.id).bind(row.revision)
    .execute(&state.db).await?;
    Ok(result.rows_affected() == 1)
}

async fn update_fetch_status(state: &AppState, row: &ServeRow, label: &str) -> ApiResult<bool> {
    let result = sqlx::query("UPDATE profile_rule_sets SET last_fetch_status = ?, last_fetch_at = ? WHERE id = ? AND revision = ?")
        .bind(label).bind(now()).bind(&row.id).bind(row.revision).execute(&state.db).await?;
    Ok(result.rows_affected() == 1)
}
