//! Per-profile 自包含规则库(③ 托管规则库)。
//!
//! 每个订阅持有自己的规则集定义;`RULE-SET,<name>,<policy>` 规则按名引用,生成时由
//! `src/generate.rs` 注入到输出 `rule-providers:`,`url` 指向 **按订阅 token 隔离** 的托管链接
//! `/<prefix>/api/sub/<token>/r/<name>/<behavior>.<format>`。订阅由此自包含:生成只读本表,不再
//! 依赖全局 `rule_sets`(② 仅作导入源)。提供 per-profile 增删改 + 从 ② 导入 + 公开托管端点。
//!
//! 两种来源与全局库一致(见 `src/rulelib.rs`):`manual` 托管录入 payload;`remote` 镜像远程规则集
//! (`cache=1` 懒刷新二次托管,支持二进制 `mrs`;`cache=0` 转换时直接注入上游 URL)。
//!
//! 写操作(增删改/导入)在与生成/公开刷新共用的 per-profile 锁内完成读改写,改动需要重写所服务
//! 输出时在锁内离线重生成(`generate::*_locked`,不重入锁)。被 `RULE-SET` 规则引用(含产出当前
//! 缓存的规则快照)的规则集不可改名/停用/删除(409),避免正在下发的配置失去定义。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{rejection::PathRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow};

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
const META_COLS: &str = "id, name, behavior, format, source, content, rule_count, url, \
     interval_hours, cache, last_fetch_status, enabled, created_at, updated_at";

#[derive(Serialize)]
pub struct ProfileRuleSetResponse {
    id: String,
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

async fn fetch_one(state: &AppState, profile_id: &str, id: &str) -> ApiResult<RuleSetRow> {
    sqlx::query_as::<_, RuleSetRow>(AssertSqlSafe(format!(
        "SELECT {META_COLS} FROM profile_rule_sets WHERE id = ? AND profile_id = ?"
    )))
    .bind(id)
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

// ─── 引用检查 ────────────────────────────────────────────────────────────────────

/// 收集「当前规则草稿 ∪ 产出所服务缓存的规则」引用到的规则集名(含逻辑规则内嵌套,由
/// `converter::ruleset_refs` 收集)。被引用的规则集改名/停用/删除会让正在下发的配置或下次
/// 重生成失去定义,必须拦截(409)。升级前的缓存无 `rules_snapshot`,改从 `output_yaml` 的
/// `rules` 段提取;缓存不存在时不含缓存引用,缓存损坏无法判断时返回 `None`,调用方按
/// 「被引用」保守处理。
async fn referenced_names(
    state: &AppState,
    profile_id: &str,
) -> ApiResult<Option<HashSet<String>>> {
    let mut out: HashSet<String> = HashSet::new();
    let current =
        sqlx::query_scalar::<_, String>("SELECT content FROM rulesets WHERE profile_id = ?")
            .bind(profile_id)
            .fetch_optional(&state.db)
            .await?;
    if let Some(content) = current {
        out.extend(crate::converter::ruleset_refs(&content));
    }
    // 规则快照缺失(升级前的旧缓存)时才需要整份输出作退路,避免每次读出大块 output_yaml。
    let cache = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT rules_snapshot, CASE WHEN rules_snapshot IS NULL THEN output_yaml END \
         FROM generated_cache WHERE profile_id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?;
    match cache {
        None => {}
        Some((Some(snapshot), _)) => out.extend(crate::converter::ruleset_refs(&snapshot)),
        Some((None, output)) => match cached_output_rules(output.as_deref().unwrap_or_default()) {
            Some(rules) => out.extend(crate::converter::ruleset_refs(&rules)),
            None => return Ok(None),
        },
    }
    Ok(Some(out))
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

/// `names` 为 `None`(缓存损坏,无法判断)时按「被引用」处理。
fn is_referenced(names: &Option<HashSet<String>>, name: &str) -> bool {
    names.as_ref().is_none_or(|s| s.contains(name))
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
        let referenced = referenced_names(&state, &profile_id).await?;
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
        let row = fetch_one(&state, &profile_id, &id).await?;
        let regenerate = if is_referenced(&referenced, name) {
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
/// 改动已注入输出的字段(托管链接的 behavior/format、注入方式 source/cache、remote 直连 URL、
/// 启停)时,同一事务先把 `generated_cache.capability_hash` 置 NULL(标脏,崩溃窗口内旧缓存不会被
/// 当作兼容缓存下发),提交后离线重生成;重生成非 Applied(无机场原文或新配置不合法)时删除缓存,
/// 下次公开拉取回源重建。纯 manual 内容/镜像间隔不影响输出,不做无谓重生成。
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path((profile_id, rsid)): Path<(String, String)>,
    ApiJson(body): ApiJson<RuleSetBody>,
) -> ApiResult<impl IntoResponse> {
    state.keyed_lock.run(&profile_id, async {
        let token = profile_token(&state, &profile_id).await?;
        let existing = fetch_one(&state, &profile_id, &rsid).await?;
        let n = rulelib::normalize(&body, existing.url.as_deref())?;
        let name = body.name.trim();
        let enabled = body.enabled.unwrap_or(true);
        let referenced = referenced_names(&state, &profile_id).await?;
        let was_referenced = is_referenced(&referenced, &existing.name);
        if was_referenced && name != existing.name {
            return Err(referenced_conflict(&existing.name, "renaming"));
        }
        if was_referenced && existing.enabled && !enabled {
            return Err(referenced_conflict(&existing.name, "disabling"));
        }
        // 输出中的 rule-providers 条目由「被引用且启用」的定义注入;条目出现(改名/启用补齐引用)、
        // 消失或字段变化都会改变输出,须重生成缓存。
        let now_referenced = is_referenced(&referenced, name);
        let had_entry = was_referenced && existing.enabled;
        let has_entry = now_referenced && enabled;
        let entry_changed = had_entry
            && has_entry
            && (body.behavior != existing.behavior
                || body.format != existing.format
                || n.source != existing.source
                || n.cache != existing.cache
                || (n.source == "remote" && !n.cache && n.url != existing.url));
        let affects_output = had_entry != has_entry || entry_changed;

        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if affects_output {
            sqlx::query("UPDATE generated_cache SET capability_hash = NULL WHERE profile_id = ?")
                .bind(&profile_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "UPDATE profile_rule_sets SET name = ?, behavior = ?, format = ?, source = ?, content = ?, \
             rule_count = ?, url = ?, interval_hours = ?, cache = ?, enabled = ?, \
             cached_body = NULL, cached_at = NULL, last_fetch_at = NULL, last_fetch_status = NULL, revision = revision + 1, updated_at = ?
             WHERE id = ? AND profile_id = ?",
        )
        .bind(name)
        .bind(&body.behavior)
        .bind(&body.format)
        .bind(&n.source)
        .bind(&n.content)
        .bind(n.rule_count)
        .bind(&n.url)
        .bind(n.interval_hours)
        .bind(n.cache)
        .bind(enabled)
        .bind(now())
        .bind(&rsid)
        .bind(&profile_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        let row = fetch_one(&state, &profile_id, &rsid).await?;
        let regenerate = if affects_output {
            let r = crate::generate::regenerate_from_cache_locked(&state, &profile_id).await;
            if !matches!(r, Regenerated::Applied) {
                // 无法用当前规则重建:旧缓存中的托管链接可能已随本次改动失效,删除缓存而不是
                // 继续下发,下次公开拉取回源重建。
                crate::generate::invalidate_cache(&state, &profile_id).await?;
            }
            Some(r)
        } else {
            None
        };
        Ok(Json(RuleSetSavedResponse {
            rule_set: to_response(&state, &token, row),
            regenerate,
        }))
    })
    .await
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
            let existing = fetch_one(&state, &profile_id, &rsid).await?;
            let referenced = referenced_names(&state, &profile_id).await?;
            if is_referenced(&referenced, &existing.name) {
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
    /// 复制后追加的 `RULE-SET` 规则统一指向的策略。
    policy: String,
}

#[derive(Serialize)]
pub struct ImportResult {
    /// 实际复制进本订阅(③)的规则集定义数(已存在同名定义的不计)。
    imported: usize,
    /// 离线重生成结果;复制了定义或新增了规则行时触发,两者都未发生(无实际改动)时为 `null`。
    regenerate: Option<Regenerated>,
    /// 导入提交后的完整规则文本,供前端同步本地编辑状态。
    rules_content: String,
}

/// 从全局 ② 库复制一行所需的字段(含 `rule_count`,直接沿用,免重算)。
#[derive(FromRow)]
struct GlobalRuleSet {
    name: String,
    behavior: String,
    format: String,
    source: String,
    content: String,
    url: Option<String>,
    interval_hours: i64,
    cache: bool,
    rule_count: i64,
}

/// 旧全局库可能含有新表单不再接受的定义,导入前按当前 Mihomo 组合与 URL 语法复核。
fn importable(rule_set: &GlobalRuleSet) -> bool {
    if rule_set.behavior == "classical" && rule_set.format == "mrs" {
        return false;
    }
    match rule_set.source.as_str() {
        "manual" => rulelib::MANUAL_FORMATS.contains(&rule_set.format.as_str()),
        "remote" => rule_set
            .url
            .as_deref()
            .and_then(rulelib::parse_http_url)
            .is_some(),
        _ => false,
    }
}

/// 单次导入的名字数上限:防止超大请求放大为逐名查询。
const MAX_IMPORT_NAMES: usize = 1000;

/// `POST /api/profiles/:id/rule-sets/import` —— 从全局 ② 复制选中规则集进本订阅 ③(含真实远程 URL,
/// 前端拿不到脱敏后的 URL,故由后端复制),并为尚未引用的名追加 `RULE-SET,<name>,<policy>` 规则行,
/// 随后离线重生成缓存使其立即生效。定义复制与规则更新在同一个 `BEGIN IMMEDIATE` 事务内完成
/// (规则 UPDATE 带旧内容 CAS,并发修改则整体回滚 → 409,重试即可)。只复制 ② 中实际存在的名,
/// 也只给复制后 ③ 中实际存在的名追加规则行;已存在同名定义的不重复复制、不计入 `imported`;
/// 复制了定义或新增了规则行都会触发离线重生成。
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

    // 与生成/公开刷新共用 per-profile 锁;锁内只能调 `_locked` 变体,不得重入锁。
    state.keyed_lock.run(&profile_id, async {
        let _ = profile_token(&state, &profile_id).await?;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;

        // 复制定义:逐个把 ② 行复制进 ③(本订阅已有同名则跳过);rule_count 直接沿用 ② 的值。
        // `available` 收集复制完成后 ③ 中实际存在的名(保序),只有它们才会得到规则行。
        let mut imported = 0usize;
        let mut available: Vec<&str> = Vec::new();
        for name in &names {
            let exists = sqlx::query_scalar::<_, String>(
                "SELECT id FROM profile_rule_sets WHERE profile_id = ? AND name = ?",
            )
            .bind(&profile_id)
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
            if exists.is_some() {
                available.push(name.as_str());
                continue;
            }
            let Some(g) = sqlx::query_as::<_, GlobalRuleSet>(
                "SELECT name, behavior, format, source, content, url, interval_hours, cache, rule_count \
                 FROM rule_sets WHERE name = ?",
            )
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
            else {
                continue;
            };
            if !importable(&g) {
                continue;
            }
            let id = uuid::Uuid::new_v4().to_string();
            let ts = now();
            sqlx::query(
                "INSERT INTO profile_rule_sets (id, profile_id, name, behavior, format, source, content, \
                 rule_count, url, interval_hours, cache, enabled, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
            )
            .bind(&id)
            .bind(&profile_id)
            .bind(&g.name)
            .bind(&g.behavior)
            .bind(&g.format)
            .bind(&g.source)
            .bind(&g.content)
            .bind(g.rule_count)
            .bind(&g.url)
            .bind(g.interval_hours)
            .bind(g.cache)
            .bind(&ts)
            .bind(&ts)
            .execute(&mut *tx)
            .await?;
            imported += 1;
            available.push(name.as_str());
        }

        // 追加规则行:仅为复制后实际存在且尚未被 RULE-SET 引用的名追加,统一指向 `policy`。
        let old_content =
            sqlx::query_scalar::<_, String>("SELECT content FROM rulesets WHERE profile_id = ?")
                .bind(&profile_id)
                .fetch_optional(&mut *tx)
                .await?
                .unwrap_or_default();
        let referenced = crate::converter::ruleset_refs(&old_content);
        let lines: Vec<String> = available
            .iter()
            .filter(|n| !referenced.iter().any(|r| r.as_str() == **n))
            .map(|n| format!("RULE-SET,{n},{policy}"))
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

        // 用缓存的机场原文离线重生成(含 rule-providers 注入),使导入立即反映到所服务的订阅:
        // 复制来的定义可能补齐了已有引用,新增的规则行也需要注入对应 rule-providers 条目。
        let regenerate = if imported > 0 || rules_changed {
            Some(crate::generate::regenerate_from_cache_locked(&state, &profile_id).await)
        } else {
            None
        };
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
