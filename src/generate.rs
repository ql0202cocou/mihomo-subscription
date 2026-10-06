//! 生成、预览,以及公开订阅端点。
//!
//! `generate`(也是源卡片的手动刷新)拉取机场、转换、持久化缓存,并更新 `last_fetch_*`。
//! `preview` 是只读对应物(不写缓存、不改 `last_fetch_*`)。公开端点提供新鲜缓存,在 per-profile
//! single-flight 锁下刷新,刷新失败时回退到陈旧缓存,对无效访问返回统一 `404`、无缓存且拉取失败时
//! 返回通用 `503`。见 `docs/architecture.md`「API 设计」与「安全设计」。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::app::AppState;
use crate::converter::{self, ConvertError, ConvertInput, CustomGroup, CustomNode, RuleProvider};
use crate::error::{ApiError, ApiResult};
use crate::profiles;
use crate::util::{completed_since, is_fresh, now};

const UPDATE_INTERVAL_HOURS: u32 = 24;

// ─── 共享行/输入类型 ────────────────────────────────────────────────────────

#[derive(FromRow, Clone)]
struct ProfileCore {
    id: String,
    name: String,
    source_url: String,
    token: String,
}

#[derive(FromRow, Clone)]
struct CacheRow {
    output_yaml: String,
    subscription_userinfo: Option<String>,
    generated_at: String,
}

struct Built {
    yaml: String,
    userinfo: Option<String>,
    /// 本次转换所用的机场原文,随缓存保存,供编辑后离线重生成(见 [`regenerate_from_cache`])。
    provider_yaml: String,
    content_hash: String,
    generated_at: String,
    /// 与机场 `rule-providers` 撞名、已被面板托管版覆盖的自定义规则集名(空表示无冲突)。
    ruleset_conflicts: Vec<String>,
}

/// 一次刷新尝试的结果,用于选择公开响应。
enum BuildError {
    Validation(Vec<String>),
    Upstream(String),
    /// DB 等内部错误(已由 `convert` 经 `ApiError::Internal` 脱敏并记日志);不再误报为机场拉取失败。
    Internal,
}

// ─── 处理器 ────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct GenerateResponse {
    subscription_url: String,
    generated_at: String,
    /// 与机场 `rule-providers` 撞名、已被面板托管版覆盖的自定义规则集名(空表示无冲突)。
    ruleset_conflicts: Vec<String>,
}

/// `POST /api/profiles/:id/generate` —— 校验、拉取、转换、持久化。
pub async fn generate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    // 与公开刷新、离线重生成共用 per-profile 锁:否则并发的一方可能用较旧的机场原文覆盖较新的缓存。
    let (profile, built) = state
        .keyed_lock
        .run(&id, async {
            let profile = load_core(&state, &id).await?.ok_or(ApiError::NotFound)?;
            let built = match fetch_convert_and_record(&state, &profile).await {
                Ok(b) => b,
                Err(BuildError::Validation(errors)) => return Err(ApiError::Validation(errors)),
                Err(BuildError::Upstream(label)) => return Err(ApiError::Upstream(label)),
                Err(BuildError::Internal) => return Err(ApiError::Internal),
            };
            persist_cache_and_group_order(&state, &profile.id, &built).await?;
            Ok((profile, built))
        })
        .await?;
    Ok(Json(GenerateResponse {
        subscription_url: state.subscription_url(&profile.token),
        generated_at: built.generated_at,
        ruleset_conflicts: built.ruleset_conflicts,
    }))
}

/// 新建订阅后自动拉取一次。尽力而为:拉取/转换失败仅由 `fetch_convert_and_record` 记录 `last_fetch_status`,
/// 绝不让创建本身失败。供 `profiles::create` 复用,使新订阅立即带有真实拉取状态(无「未拉取」中间态)。
pub async fn generate_best_effort(state: &AppState, id: &str) {
    state
        .keyed_lock
        .run(id, async {
            let Some(profile) = load_core(state, id).await.ok().flatten() else {
                return;
            };
            if let Ok(built) = fetch_convert_and_record(state, &profile).await {
                let _ = persist_cache_and_group_order(state, id, &built).await;
            }
        })
        .await;
}

/// `GET /api/profiles/:id/preview` —— 只读的生成 YAML。有新鲜缓存则返回,否则实时生成、不持久化、
/// 不触碰 `last_fetch_*`。
pub async fn preview(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let profile = load_core(&state, &id).await?.ok_or(ApiError::NotFound)?;

    if let Some(cache) = load_cache(&state, &profile.id).await? {
        if is_fresh(&cache.generated_at, state.cache_ttl) {
            return Ok(yaml_body(cache.output_yaml));
        }
    }

    // 实时生成;不持久化、不更新 last_fetch_*。
    let fetched = state
        .fetcher
        .fetch(&profile.source_url)
        .await
        .map_err(|e| ApiError::Upstream(e.status_label()))?;
    let (yaml, _) = convert(&state, &profile.id, &profile.token, &fetched.body)
        .await?
        .map_err(map_convert_err)?;
    Ok(yaml_body(yaml))
}

#[derive(Serialize)]
struct ProviderRules {
    rules: Vec<String>,
}

/// `GET /api/profiles/:id/provider-rules` —— 拉取机场订阅并返回其 `rules` 行,使管理员能用机场
/// 自带的规则预填规则编辑器(否则转换器会替换它们)。实时、SSRF 保护的拉取;不缓存,也不触碰
/// `last_fetch_*`。
pub async fn provider_rules(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let profile = load_core(&state, &id).await?.ok_or(ApiError::NotFound)?;
    let fetched = state
        .fetcher
        .fetch(&profile.source_url)
        .await
        .map_err(|e| ApiError::Upstream(e.status_label()))?;
    let root = crate::yaml::parse_limited(&fetched.body)
        .map_err(|_| ApiError::Upstream("provider_parse".to_string()))?;
    let rules = match root.get("rules") {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    Ok(Json(ProviderRules { rules }).into_response())
}

/// `GET /:public_path_prefix/api/sub/:token` —— 公开订阅下载。
pub async fn public_sub(
    State(state): State<Arc<AppState>>,
    Path((prefix, token)): Path<(String, String)>,
) -> Response {
    let lookup = async { load_core_by_token(&state, &token).await.ok().flatten() };
    let profile = match state.public_gate(&prefix, lookup).await {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let served = serve_or_refresh(&state, &profile, &prefix).await;
    // 锁等待、回源、转换过程中可能已轮换能力;交付前再次校验前缀和 token。
    if state
        .public_gate(&prefix, async {
            load_core_by_token(&state, &token).await.ok().flatten()
        })
        .await
        .is_none()
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match served {
        Some(served) => public_response(&profile.name, served),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

// ─── 公开 serve/refresh(带 single-flight）──────────────────────────────────

struct Served {
    yaml: String,
    userinfo: Option<String>,
}

async fn serve_or_refresh(state: &AppState, profile: &ProfileCore, prefix: &str) -> Option<Served> {
    // 公开拉取在一个较短的最小刷新间隔内复用最近缓存,避免 token 泄露后被高频请求放大为机场
    // 回源压力;间隔外仍尽力实时回源,失败时用缓存兜底。
    let arrived = now();

    // 把该 profile 的并发拉取合并为一次机场拉取。
    state
        .keyed_lock
        .run(&profile.id, async {
            let current = state.public_gate(prefix, async {
                load_core_by_token(state, &profile.token).await.ok().flatten()
            }).await?;
            let profile = &current;
            // 若缓存仍处于公开刷新最小间隔内,直接提供它;否则如果本批里另一个请求在我们等锁期间已刷新过
            // (缓存在我们到达时或之后被重生),也提供它而非再次拉取。
            let cached = load_cache(state, &profile.id).await.ok().flatten();
            let serve_cached = cached.as_ref().is_some_and(|cache| {
                is_fresh(&cache.generated_at, state.public_refresh_min_interval)
                    || completed_since(&cache.generated_at, &arrived)
            });
            let attempted = sqlx::query_scalar::<_, Option<String>>("SELECT public_refresh_at FROM profiles WHERE id = ?")
                .bind(&profile.id).fetch_optional(&state.db).await.ok().flatten().flatten();
            let recent_attempt = attempted.as_deref().is_some_and(|at| {
                is_fresh(at, state.public_refresh_min_interval) || completed_since(at, &arrived)
            });
            if serve_cached || recent_attempt {
                return cached.map(Served::from);
            }
            let result = fetch_convert_and_record(state, profile).await;
            // 使用完成时间,慢失败后等待者也共享结果;无需改变旧成功缓存的时间戳。
            let _ = sqlx::query("UPDATE profiles SET public_refresh_at = ? WHERE id = ?")
                .bind(now()).bind(&profile.id).execute(&state.db).await;
            match result {
                Ok(built) => {
                    if persist_cache_and_group_order(state, &profile.id, &built).await.is_err() {
                        tracing::error!(profile = %profile.id, "failed to persist generated cache");
                    }
                    Some(Served {
                        yaml: built.yaml,
                        userinfo: built.userinfo,
                    })
                }
                // 机场侧失败的 `last_fetch_status` 已由 `fetch_convert_and_record` 记录。
                Err(_) => {
                    // 有陈旧缓存就提供;否则给出 503。
                    match cached {
                        Some(cache) => {
                            tracing::warn!(profile = %profile.id, "refresh failed; serving stale cache");
                            Some(cache.into())
                        }
                        None => None,
                    }
                }
            }
        })
        .await
}

impl From<CacheRow> for Served {
    fn from(c: CacheRow) -> Self {
        Served {
            yaml: c.output_yaml,
            userinfo: c.subscription_userinfo,
        }
    }
}

// ─── 核心 拉取 + 转换 ──────────────────────────────────────────────────────────

/// 拉取机场并转换。拉取成功时把 `last_fetch_*` 更新为 `success`;拉取失败或机场内容无法解析时
/// 记录对应状态标签。
async fn fetch_convert_and_record(
    state: &AppState,
    profile: &ProfileCore,
) -> Result<Built, BuildError> {
    let fetched = match state.fetcher.fetch(&profile.source_url).await {
        Ok(f) => f,
        Err(e) => {
            let label = e.status_label();
            let _ = update_last_fetch(state, &profile.id, &label).await;
            return Err(BuildError::Upstream(label));
        }
    };
    let _ = update_last_fetch(state, &profile.id, "success").await;

    let converted = convert(state, &profile.id, &profile.token, &fetched.body)
        .await
        // DB 错误已被 `From<sqlx::Error>` 脱敏并记日志;传播为内部错误,不再误报为机场拉取失败。
        .map_err(|_| BuildError::Internal)?;
    let (yaml, ruleset_conflicts) = match converted {
        Ok(out) => out,
        Err(ConvertError::Validation(v)) => return Err(BuildError::Validation(v)),
        // 拉取成功但内容不是可解析的 Mihomo YAML(如机场不认 UA 返回的 base64 节点列表):同样是
        // 机场侧问题,覆盖上面刚写入的 `success`,使列表/详情如实反映。
        Err(ConvertError::ProviderParse) => {
            let label = "provider_parse".to_string();
            let _ = update_last_fetch(state, &profile.id, &label).await;
            return Err(BuildError::Upstream(label));
        }
        Err(ConvertError::OutputSerialize) => return Err(BuildError::Internal),
    };
    if !ruleset_conflicts.is_empty() {
        tracing::warn!(
            profile = %profile.id,
            conflicts = ?ruleset_conflicts,
            "custom rule-sets override same-named provider rule-providers",
        );
    }

    let content_hash = content_hash_of(&yaml);
    Ok(Built {
        yaml,
        userinfo: fetched.subscription_userinfo,
        provider_yaml: fetched.body,
        content_hash,
        generated_at: now(),
        ruleset_conflicts,
    })
}

/// 从 DB 装载转换输入并运行转换器。外层 `ApiResult` 表示 DB 错误;内层 `Result` 是转换器结果。
async fn convert(
    state: &AppState,
    profile_id: &str,
    token: &str,
    provider_yaml: &str,
) -> ApiResult<Result<(String, Vec<String>), ConvertError>> {
    let rules =
        sqlx::query_scalar::<_, String>("SELECT content FROM rulesets WHERE profile_id = ?")
            .bind(profile_id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_default();

    // 被本 profile `RULE-SET` 规则引用、且启用的 **本订阅自有** 规则集(③)→ 注入输出
    // `rule-providers:`(`url` 指向按订阅 token 隔离的托管链接)。未被引用的不注入;全局 ② 库不参与
    // 生成。规则集通常很少,取全量后在内存里按引用过滤。
    let refs = converter::ruleset_refs(&rules);
    let rule_providers: Vec<RuleProvider> = if refs.is_empty() {
        Vec::new()
    } else {
        let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>, bool)>(
            "SELECT name, behavior, format, source, url, cache FROM profile_rule_sets \
             WHERE profile_id = ? AND enabled = 1",
        )
        .bind(profile_id)
        .fetch_all(&state.db)
        .await?;
        let mut providers = Vec::new();
        for (name, behavior, format, source, url, cache) in rows {
            if !refs.iter().any(|r| r == &name) {
                continue;
            }
            // remote 且关闭本地缓存托管:直接注入上游 URL;否则指向按订阅隔离的面板托管链接。
            let link = if source == "remote" && !cache {
                // remote 必须有上游 URL;缺失(脏数据)报明确校验错误,而非注入空 URL。
                match url.filter(|u| !u.trim().is_empty()) {
                    Some(u) => u,
                    None => {
                        return Ok(Err(ConvertError::Validation(vec![format!(
                            "rule-set `{name}` is remote without cache but has no upstream URL"
                        )])))
                    }
                }
            } else {
                state.profile_rule_set_url(token, &name, &behavior, &format)
            };
            providers.push(RuleProvider {
                url: link,
                name,
                behavior,
                format,
            });
        }
        providers
    };

    // 自定义节点是单一全局池(模型 C),追加到每条 profile 的输出,且查询已按自定义块顺序
    // (`position`)取出,故无需 per-profile 的 `node_order`——转换器保持此顺序。
    let nodes = sqlx::query_as::<_, (String, String)>(
        "SELECT name, content FROM global_nodes WHERE enabled = 1 ORDER BY position, name",
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|(name, content)| CustomNode { name, content })
    .collect();

    let groups = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT name, group_type, members, options FROM custom_groups WHERE profile_id = ? AND enabled = 1 ORDER BY created_at",
    )
    .bind(profile_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|(name, group_type, members, options)| CustomGroup {
        name,
        group_type,
        members: serde_json::from_str(&members).unwrap_or_default(),
        options: options.and_then(|o| serde_json::from_str(&o).ok()),
    })
    .collect();

    // per-profile 的手动排序(NULL/异常 -> 空 -> 默认):两个节点块的先后(机场/自定义)与
    // proxy-group 顺序。自定义块的内部顺序是全局的(`global_nodes.position`),上面的查询已应用,
    // 故这里 `node_order` 为空。
    let (node_section_order, group_order) = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT node_section_order, group_order FROM profiles WHERE id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?
    .map(|(s, g)| (profiles::parse_order(s), profiles::parse_order(g)))
    .unwrap_or_default();

    // 转换器在注入自定义规则集时一并报出与机场 `rule-providers` 撞名(覆盖)的名字,无需二次解析。
    Ok(converter::convert(ConvertInput {
        provider_yaml,
        rules: &rules,
        nodes,
        groups,
        node_order: Vec::new(),
        node_section_order,
        group_order,
        rule_providers,
    }))
}

fn map_convert_err(e: ConvertError) -> ApiError {
    match e {
        ConvertError::Validation(v) => ApiError::Validation(v),
        ConvertError::ProviderParse => ApiError::Upstream("provider_parse".to_string()),
        ConvertError::OutputSerialize => ApiError::Internal,
    }
}

// ─── DB 辅助 ───────────────────────────────────────────────────────────────────

async fn load_core(state: &AppState, id: &str) -> ApiResult<Option<ProfileCore>> {
    Ok(sqlx::query_as::<_, ProfileCore>(
        "SELECT id, name, source_url, token FROM profiles WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?)
}

async fn load_core_by_token(state: &AppState, token: &str) -> ApiResult<Option<ProfileCore>> {
    Ok(sqlx::query_as::<_, ProfileCore>(
        "SELECT id, name, source_url, token FROM profiles WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await?)
}

async fn load_cache(state: &AppState, profile_id: &str) -> ApiResult<Option<CacheRow>> {
    Ok(sqlx::query_as::<_, CacheRow>(
        "SELECT output_yaml, subscription_userinfo, generated_at FROM generated_cache WHERE profile_id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?)
}

async fn persist_cache_and_group_order(
    state: &AppState,
    profile_id: &str,
    built: &Built,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO generated_cache (profile_id, content_hash, output_yaml, subscription_userinfo, provider_yaml, generated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(profile_id) DO UPDATE SET
            content_hash = excluded.content_hash,
            output_yaml = excluded.output_yaml,
            subscription_userinfo = excluded.subscription_userinfo,
            provider_yaml = excluded.provider_yaml,
            generated_at = excluded.generated_at",
    )
    .bind(profile_id)
    .bind(&built.content_hash)
    .bind(&built.yaml)
    .bind(&built.userinfo)
    .bind(&built.provider_yaml)
    .bind(&built.generated_at)
    .execute(&state.db)
    .await?;

    // 把输出的 proxy-group 顺序快照回写,使其在机场刷新间保持稳定:仍存在的分组保住其位置,
    // 新增的落到末尾。之后的手动拖拽会经 `set_group_order` 覆盖本快照。节点顺序是全局的
    // (`global_nodes.position`),不做 per-profile 快照。尽力而为;绝不让生成失败。
    if snapshot_group_order(state, profile_id, &built.yaml)
        .await
        .is_err()
    {
        tracing::warn!(profile = %profile_id, "failed to snapshot group order");
    }
    Ok(())
}

/// 把输出的 proxy-group 顺序回写到 `profiles.group_order`(新增分组持久化到末尾)。节点顺序是
/// 全局的(`global_nodes.position`),从不做 per-profile 快照。空 → NULL。
async fn snapshot_group_order(state: &AppState, profile_id: &str, yaml: &str) -> ApiResult<()> {
    let Ok(root) = crate::yaml::parse_limited(yaml) else {
        return Ok(());
    };
    let group_order = order_json(&root, "proxy-groups", |_| true);
    sqlx::query("UPDATE profiles SET group_order = ? WHERE id = ?")
        .bind(&group_order)
        .bind(profile_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// 提取某顶层序列的有序 `name`,仅保留匹配 `keep` 的,序列化为 JSON 数组;无则 `None`(→ SQL NULL)。
fn order_json(root: &serde_yaml::Value, key: &str, keep: impl Fn(&str) -> bool) -> Option<String> {
    let names: Vec<&str> = match root.get(key) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|item| item.get("name").and_then(|v| v.as_str()))
            .filter(|n| keep(n))
            .collect(),
        _ => Vec::new(),
    };
    if names.is_empty() {
        None
    } else {
        serde_json::to_string(&names).ok()
    }
}

async fn update_last_fetch(state: &AppState, profile_id: &str, status: &str) -> ApiResult<()> {
    sqlx::query("UPDATE profiles SET last_fetch_at = ?, last_fetch_status = ? WHERE id = ?")
        .bind(now())
        .bind(status)
        .bind(profile_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// 离线重生成的结果。规则/排序类保存接口随响应返回,前端据此如实提示改动是否已下发。
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Regenerated {
    /// 已按新配置重写缓存,公开链接立即下发新输出。
    Applied,
    /// 没有可用的机场原文(尚未生成、升级前的缓存),或重生成出错:改动在下次回源后生效。
    Pending,
    /// 新配置未通过校验:保留上一份合法输出。`errors` 与「生成」返回的逐条错误一致。
    Invalid { errors: Vec<String> },
}

/// 用缓存中的机场原文离线重跑完整转换(校验、`rule-providers` 注入、排序),不重拉机场。规则/排序
/// 编辑与重置 token/前缀后调用,使改动立即反映到所服务的订阅,且与完整生成走同一条路径。
///
/// - 校验失败时保留上一份合法输出:编辑已入库,坏配置不会被下发,逐条错误经返回值交给调用方。
/// - 尚未生成、或升级前生成的缓存(无机场原文)为 no-op,改动在下次回源拉取后生效。
/// - 保留 `generated_at`,使公开端点的回源节奏不变(内容仍来自上次拉取)。
/// - 与公开刷新共用 per-profile 锁,避免并发刷新用旧输入覆盖本次结果。
/// - 尽力而为:出错时记日志并返回 `Pending`,绝不让发起的保存请求失败。
pub async fn regenerate_from_cache(state: &AppState, profile_id: &str) -> Regenerated {
    let result: ApiResult<Regenerated> = state
        .keyed_lock
        .run(profile_id, async {
            let Some(profile) = load_core(state, profile_id).await? else {
                return Ok(Regenerated::Pending);
            };
            let Some((Some(provider_yaml), userinfo, generated_at)) =
                sqlx::query_as::<_, (Option<String>, Option<String>, String)>(
                    "SELECT provider_yaml, subscription_userinfo, generated_at \
                     FROM generated_cache WHERE profile_id = ?",
                )
                .bind(profile_id)
                .fetch_optional(&state.db)
                .await?
            else {
                return Ok(Regenerated::Pending);
            };
            let (yaml, ruleset_conflicts) =
                match convert(state, profile_id, &profile.token, &provider_yaml).await? {
                    Ok(out) => out,
                    Err(e) => {
                        tracing::warn!(profile = %profile_id, "edited config is invalid; keeping the last valid cache");
                        return Ok(match e {
                            ConvertError::Validation(errors) => Regenerated::Invalid { errors },
                            _ => Regenerated::Pending,
                        });
                    }
                };
            let built = Built {
                content_hash: content_hash_of(&yaml),
                yaml,
                userinfo,
                provider_yaml,
                generated_at,
                ruleset_conflicts,
            };
            persist_cache_and_group_order(state, profile_id, &built).await?;
            Ok(Regenerated::Applied)
        })
        .await;
    result.unwrap_or_else(|_| {
        tracing::warn!(profile = %profile_id, "failed to regenerate cache");
        Regenerated::Pending
    })
}

/// 对每条 profile 执行 [`regenerate_from_cache`]。用于影响所有 profile 的改动(全局节点排序、重置公共
/// 路径前缀)。逐 profile 尽力而为:某个失败就让该 profile 在下次生成时再吸收改动。
pub async fn regenerate_all_from_cache(state: &AppState) {
    let ids = sqlx::query_scalar::<_, String>("SELECT id FROM profiles")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for id in ids {
        regenerate_from_cache(state, &id).await;
    }
}

// ─── 辅助 ──────────────────────────────────────────────────────────────────────

fn content_hash_of(output_yaml: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(output_yaml.as_bytes());
    hex(hasher.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn yaml_body(yaml: String) -> Response {
    ([(header::CONTENT_TYPE, "text/yaml; charset=utf-8")], yaml).into_response()
}

fn public_response(profile_name: &str, served: Served) -> Response {
    let filename = sanitize_filename(profile_name);
    let mut headers = vec![
        (header::CONTENT_TYPE, "text/yaml; charset=utf-8".to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}.yaml\""),
        ),
        (
            header::HeaderName::from_static("profile-update-interval"),
            UPDATE_INTERVAL_HOURS.to_string(),
        ),
    ];
    if let Some(userinfo) = served.userinfo {
        headers.push((
            header::HeaderName::from_static("subscription-userinfo"),
            userinfo,
        ));
    }
    (build_header_map(headers), served.yaml).into_response()
}

fn build_header_map(pairs: Vec<(header::HeaderName, String)>) -> header::HeaderMap {
    let mut map = header::HeaderMap::new();
    for (name, value) in pairs {
        if let Ok(v) = header::HeaderValue::from_str(&value) {
            map.insert(name, v);
        }
    }
    map
}

/// 只保留文件名安全的字符,使该值无法破坏 `Content-Disposition` 头或客户端的文件处理。
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "subscription".to_string()
    } else {
        trimmed.to_string()
    }
}
