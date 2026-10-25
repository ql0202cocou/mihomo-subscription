//! 生成、预览,以及公开订阅端点。
//!
//! `generate`(也是源卡片的手动刷新)拉取机场、转换、持久化缓存,并更新 `last_fetch_*`。
//! `preview` 是只读对应物(不写缓存、不改 `last_fetch_*`)。公开端点提供新鲜缓存,在 per-profile
//! single-flight 锁下刷新,刷新失败时回退到陈旧缓存,对无效访问返回统一 `404`、无缓存且拉取失败时
//! 返回通用 `503`。见 `docs/architecture.md`「API 设计」与「安全设计」。

use std::sync::Arc;

use axum::{
    extract::{rejection::PathRejection, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::app::{capability_hash, AppState};
use crate::converter::{
    self, ConvertError, ConvertInput, ConvertOutput, CustomGroup, CustomNode, RuleProvider,
};
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
    /// 来源版本(换源时递增):回源状态记录与缓存回写均绑定它,换源前已在途的旧来源
    /// 回源结果在写入时因版本不符被丢弃。
    source_revision: i64,
}

#[derive(FromRow, Clone)]
struct CacheRow {
    output_yaml: String,
    subscription_userinfo: Option<String>,
    generated_at: String,
    source_revision: Option<i64>,
    capability_hash: Option<String>,
}

#[derive(Clone)]
struct BuildContext {
    source_revision: i64,
    token: String,
    prefix: String,
}

impl BuildContext {
    fn new(profile: &ProfileCore, prefix: &str) -> Self {
        Self {
            source_revision: profile.source_revision,
            token: profile.token.clone(),
            prefix: prefix.to_string(),
        }
    }

    fn capability_hash(&self) -> String {
        capability_hash(&self.token, &self.prefix)
    }
}

struct Built {
    yaml: String,
    userinfo: Option<String>,
    /// 本次转换所用的机场原文,随缓存保存,供编辑后离线重生成(见 [`regenerate_from_cache`])。
    provider_yaml: String,
    /// 本次转换所用的规则文本,随缓存保存(快照),供轮换 token/前缀时在当前草稿未通过校验的
    /// 情况下用最近一份合法规则离线重生成(见 [`regenerate_for_rotation`])。
    rules: String,
    content_hash: String,
    generated_at: String,
    /// 与机场 `rule-providers` 撞名、已被面板托管版覆盖的自定义规则集名(空表示无冲突)。
    ruleset_conflicts: Vec<String>,
    /// ML-KEM 已开启但指纹不是 chrome 的 REALITY 节点名;随缓存保存,供详情页常驻告警。
    reality_fingerprint_risks: Vec<String>,
    /// 输出的分组名(按输出顺序),回写为 `profiles.group_order` 快照。
    group_names: Vec<String>,
    context: BuildContext,
    group_order_before: Option<String>,
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
            let prefix = state.current_prefix();
            let built = match fetch_convert_and_record(&state, &profile, &prefix).await {
                Ok(b) => b,
                Err(BuildError::Validation(errors)) => return Err(ApiError::Validation(errors)),
                Err(BuildError::Upstream(label)) => return Err(ApiError::Upstream(label)),
                Err(BuildError::Internal) => return Err(ApiError::Internal),
            };
            // 拉取期间来源或公开能力变化:旧结果不落缓存,提示重试。
            if !persist_cache_and_group_order(&state, &profile.id, &built).await? {
                return Err(ApiError::Conflict(
                    "generation context changed while generating; please retry".into(),
                ));
            }
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
            let prefix = state.current_prefix();
            if let Ok(built) = fetch_convert_and_record(state, &profile, &prefix).await {
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
    state
        .keyed_lock
        .run(&id, async {
            let profile = load_core(&state, &id).await?.ok_or(ApiError::NotFound)?;
            let prefix = state.current_prefix();

            if let Some(cache) = load_cache(&state, &profile.id).await? {
                if cache_is_compatible(&cache, &profile, &prefix)
                    && is_fresh(&cache.generated_at, state.cache_ttl)
                {
                    return Ok(yaml_body(cache.output_yaml));
                }
            }

            // 实时生成;不持久化、不更新 last_fetch_*。与写入同锁,避免旧正文搭配新配置。
            let fetched = state
                .fetcher
                .fetch(&profile.source_url)
                .await
                .map_err(|e| ApiError::Upstream(e.status_label()))?;
            let converted = convert(
                &state,
                &profile.id,
                &profile.token,
                &prefix,
                &fetched.body,
                None,
            )
            .await?
            .map_err(map_convert_err)?;
            Ok(yaml_body(converted.output.yaml))
        })
        .await
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
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let Ok(Path((prefix, token))) = path else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(profile) = gated_core(&state, &prefix, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let served = serve_or_refresh(&state, &profile, &prefix).await;
    // 锁等待、回源、转换过程中可能已轮换能力;交付前再次校验前缀和 token。
    if gated_core(&state, &prefix, &token).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match served {
        Some(served) => public_response(&profile.name, served),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// 经 [`AppState::public_gate`] 按前缀与 token 定位 profile;任一不符(或查询失败)返回 `None`。
async fn gated_core(state: &AppState, prefix: &str, token: &str) -> Option<ProfileCore> {
    state
        .public_gate(prefix, async {
            load_core_by_token(state, token).await.ok().flatten()
        })
        .await
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
            let current = gated_core(state, prefix, &profile.token).await?;
            let profile = &current;
            // 只有来源与公开能力都匹配的缓存才能进入新鲜复用和失败兜底。
            let cached = load_usable_cache(state, profile, prefix).await;
            let serve_cached = cached.as_ref().is_some_and(|cache| {
                is_fresh(&cache.generated_at, state.public_refresh_min_interval)
                    || completed_since(&cache.generated_at, &arrived)
            });
            if serve_cached {
                return cached.map(Served::from);
            }
            let attempted = sqlx::query_scalar::<_, Option<String>>("SELECT public_refresh_at FROM profiles WHERE id = ?")
                .bind(&profile.id).fetch_optional(&state.db).await.ok().flatten().flatten();
            let recent_attempt = attempted.as_deref().is_some_and(|at| {
                is_fresh(at, state.public_refresh_min_interval) || completed_since(at, &arrived)
            });
            if recent_attempt {
                return cached.map(Served::from);
            }
            let attempt_prefix = state.current_prefix();
            let result = fetch_convert_and_record(state, profile, &attempt_prefix).await;
            // 使用完成时间,慢失败后等待者也共享结果;旧来源/能力的在途尝试不得记录到新上下文。
            record_public_attempt(state, profile, &attempt_prefix).await;
            match result {
                Ok(built) => {
                    match persist_cache_and_group_order(state, &profile.id, &built).await {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!(profile = %profile.id, "discarded fetch result: generation context changed while fetching"),
                        Err(_) => tracing::error!(profile = %profile.id, "failed to persist generated cache"),
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

fn cache_is_compatible(cache: &CacheRow, profile: &ProfileCore, prefix: &str) -> bool {
    cache_context_matches(
        cache.source_revision,
        cache.capability_hash.as_deref(),
        profile.source_revision,
        &capability_hash(&profile.token, prefix),
    )
}

/// 缓存记录的构建上下文(来源版本、能力摘要)是否与当前一致;NULL(旧缓存/已标脏)一律不一致。
/// 公开端点与管理端展示共用这一判定。
pub(crate) fn cache_context_matches(
    revision: Option<i64>,
    hash: Option<&str>,
    source_revision: i64,
    expected_hash: &str,
) -> bool {
    revision == Some(source_revision) && hash == Some(expected_hash)
}

/// 在 profile 锁内验证缓存上下文。仅能力变化可离线迁移;来源未知或变化时必须在线重建。
async fn load_usable_cache(
    state: &AppState,
    profile: &ProfileCore,
    prefix: &str,
) -> Option<CacheRow> {
    let cache = load_cache(state, &profile.id).await.ok().flatten()?;
    if cache.source_revision != Some(profile.source_revision) {
        let _ = invalidate_cache(state, &profile.id).await;
        return None;
    }
    if cache_is_compatible(&cache, profile, prefix) {
        return Some(cache);
    }
    match regenerate_for_rotation_locked(state, &profile.id).await {
        Ok(Regenerated::Applied) => load_cache(state, &profile.id)
            .await
            .ok()
            .flatten()
            .filter(|cache| cache_is_compatible(cache, profile, prefix)),
        _ => {
            let _ = invalidate_cache(state, &profile.id).await;
            None
        }
    }
}

pub(crate) async fn invalidate_cache(state: &AppState, profile_id: &str) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM generated_cache WHERE profile_id = ?")
        .bind(profile_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE profiles SET public_refresh_at = NULL WHERE id = ?")
        .bind(profile_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn record_public_attempt(state: &AppState, profile: &ProfileCore, prefix: &str) {
    let _ = sqlx::query(
        "UPDATE profiles SET public_refresh_at = ? \
         WHERE id = ? AND source_revision = ? AND token = ? \
         AND ? = (SELECT public_path_prefix FROM app_settings WHERE id = 1)",
    )
    .bind(now())
    .bind(&profile.id)
    .bind(profile.source_revision)
    .bind(&profile.token)
    .bind(prefix)
    .execute(&state.db)
    .await;
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
    prefix: &str,
) -> Result<Built, BuildError> {
    let fetched = match state.fetcher.fetch(&profile.source_url).await {
        Ok(f) => f,
        Err(e) => {
            let label = e.status_label();
            let _ = update_last_fetch(state, profile, &label, prefix).await;
            return Err(BuildError::Upstream(label));
        }
    };
    let _ = update_last_fetch(state, profile, "success", prefix).await;

    let converted = convert(
        state,
        &profile.id,
        &profile.token,
        prefix,
        &fetched.body,
        None,
    )
    .await
    // DB 错误已被 `From<sqlx::Error>` 脱敏并记日志;传播为内部错误,不再误报为机场拉取失败。
    .map_err(|_| BuildError::Internal)?;
    let converted = match converted {
        Ok(converted) => converted,
        Err(ConvertError::Validation(v)) => return Err(BuildError::Validation(v)),
        // 拉取成功但内容不是可解析的 Mihomo YAML(如机场不认 UA 返回的 base64 节点列表):同样是
        // 机场侧问题,覆盖上面刚写入的 `success`,使列表/详情如实反映。
        Err(ConvertError::ProviderParse) => {
            let label = "provider_parse".to_string();
            let _ = update_last_fetch(state, profile, &label, prefix).await;
            return Err(BuildError::Upstream(label));
        }
        Err(ConvertError::OutputSerialize) => return Err(BuildError::Internal),
    };
    if !converted.output.ruleset_conflicts.is_empty() {
        tracing::warn!(
            profile = %profile.id,
            conflicts = ?converted.output.ruleset_conflicts,
            "custom rule-sets override same-named provider rule-providers",
        );
    }

    Ok(converted.into_built(
        fetched.body,
        fetched.subscription_userinfo,
        now(),
        BuildContext::new(profile, prefix),
    ))
}

/// 一次成功转换的产出。
struct Converted {
    output: ConvertOutput,
    /// 本次转换实际使用的规则文本(随缓存快照;轮换 token/前缀时,当前草稿未通过校验则用它
    /// 离线重生成,见 [`regenerate_for_rotation`])。
    rules: String,
    group_order_before: Option<String>,
}

impl Converted {
    /// 连同本次所用的机场原文、响应头与生成时间组装成待写入缓存的 [`Built`]。
    fn into_built(
        self,
        provider_yaml: String,
        userinfo: Option<String>,
        generated_at: String,
        context: BuildContext,
    ) -> Built {
        let ConvertOutput {
            yaml,
            ruleset_conflicts,
            reality_fingerprint_risks,
            group_names,
        } = self.output;
        Built {
            content_hash: content_hash_of(&yaml),
            yaml,
            userinfo,
            provider_yaml,
            rules: self.rules,
            generated_at,
            ruleset_conflicts,
            reality_fingerprint_risks,
            group_names,
            context,
            group_order_before: self.group_order_before,
        }
    }
}

/// 从 DB 装载转换输入并运行转换器。外层 `ApiResult` 表示 DB 错误;内层 `Result` 是转换器结果。
/// `rules_override` 取代库中当前规则文本(仅轮换回退使用,见 [`regenerate_for_rotation`])。
async fn convert(
    state: &AppState,
    profile_id: &str,
    token: &str,
    prefix: &str,
    provider_yaml: &str,
    rules_override: Option<&str>,
) -> ApiResult<Result<Converted, ConvertError>> {
    let rules = match rules_override {
        Some(rules) => rules.to_string(),
        None => {
            sqlx::query_scalar::<_, String>("SELECT content FROM rulesets WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_optional(&state.db)
                .await?
                .unwrap_or_default()
        }
    };

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
            if crate::rulelib::validate_name(&name).is_err() {
                return Ok(Err(ConvertError::Validation(vec![format!(
                    "rule-set `{name}` has an invalid name"
                )])));
            }
            if behavior == "classical" && format == "mrs" {
                return Ok(Err(ConvertError::Validation(vec![format!(
                    "rule-set `{name}` uses unsupported classical MRS"
                )])));
            }
            // remote 且关闭本地缓存托管:直接注入上游 URL;否则指向按订阅隔离的面板托管链接。
            let link = if source == "remote" && !cache {
                match url.as_deref().and_then(direct_rule_url) {
                    Some(u) => u,
                    None => {
                        return Ok(Err(ConvertError::Validation(vec![format!(
                            "rule-set `{name}` has an invalid upstream URL"
                        )])))
                    }
                }
            } else {
                state.profile_rule_set_url_with_prefix(prefix, token, &name, &behavior, &format)
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
    // 故这里 `node_order` 为空。另取是否为 REALITY 节点自动开启 ML-KEM(缺省开启)。
    let (node_section_order, group_order_before, reality_mlkem) =
        sqlx::query_as::<_, (Option<String>, Option<String>, bool)>(
            "SELECT node_section_order, group_order, reality_mlkem FROM profiles WHERE id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or((None, None, true));
    let node_section_order = profiles::parse_order(node_section_order);
    let group_order = profiles::parse_order(group_order_before.clone());

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
        reality_mlkem,
    })
    .map(|output| Converted {
        output,
        rules,
        group_order_before,
    }))
}

fn direct_rule_url(raw: &str) -> Option<String> {
    crate::rulelib::parse_http_url(raw).map(|url| url.to_string())
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
        "SELECT id, name, source_url, token, source_revision FROM profiles WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?)
}

async fn load_core_by_token(state: &AppState, token: &str) -> ApiResult<Option<ProfileCore>> {
    Ok(sqlx::query_as::<_, ProfileCore>(
        "SELECT id, name, source_url, token, source_revision FROM profiles WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await?)
}

async fn load_cache(state: &AppState, profile_id: &str) -> ApiResult<Option<CacheRow>> {
    Ok(sqlx::query_as::<_, CacheRow>(
        "SELECT output_yaml, subscription_userinfo, generated_at, source_revision, capability_hash \
         FROM generated_cache WHERE profile_id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?)
}

/// 持久化生成缓存并回写分组顺序快照。只有构建时的来源、token 和公共前缀仍与数据库一致
/// 才写入;失配时整份旧缓存保持不变。返回是否真的写入了缓存。
async fn persist_cache_and_group_order(
    state: &AppState,
    profile_id: &str,
    built: &Built,
) -> ApiResult<bool> {
    // 指纹风险随缓存保存(无则 NULL)。只在名单变化时记日志:同一批节点在每次刷新、每条订阅里
    // 重复出现,常驻提示交给详情页。调用方都持有 per-profile 锁,先读后写不会与并发生成交错。
    let risks = (!built.reality_fingerprint_risks.is_empty())
        .then(|| serde_json::to_string(&built.reality_fingerprint_risks).unwrap_or_default());
    let previous_risks = sqlx::query_scalar::<_, Option<String>>(
        "SELECT reality_fingerprint_risks FROM generated_cache WHERE profile_id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();
    let result = sqlx::query(
        "INSERT INTO generated_cache (profile_id, content_hash, output_yaml, subscription_userinfo, provider_yaml, rules_snapshot, reality_fingerprint_risks, generated_at, source_revision, capability_hash)
         SELECT p.id, ?, ?, ?, ?, ?, ?, ?, p.source_revision, ?
         FROM profiles AS p CROSS JOIN app_settings AS s
         WHERE p.id = ? AND s.id = 1 AND p.source_revision = ? AND p.token = ?
           AND s.public_path_prefix = ?
         ON CONFLICT(profile_id) DO UPDATE SET
            content_hash = excluded.content_hash,
            output_yaml = excluded.output_yaml,
            subscription_userinfo = excluded.subscription_userinfo,
            provider_yaml = excluded.provider_yaml,
            rules_snapshot = excluded.rules_snapshot,
            reality_fingerprint_risks = excluded.reality_fingerprint_risks,
            generated_at = excluded.generated_at,
            source_revision = excluded.source_revision,
            capability_hash = excluded.capability_hash",
    )
    .bind(&built.content_hash)
    .bind(&built.yaml)
    .bind(&built.userinfo)
    .bind(&built.provider_yaml)
    .bind(&built.rules)
    .bind(&risks)
    .bind(&built.generated_at)
    .bind(built.context.capability_hash())
    .bind(profile_id)
    .bind(built.context.source_revision)
    .bind(&built.context.token)
    .bind(&built.context.prefix)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    if risks.is_some() && risks != previous_risks {
        tracing::warn!(
            profile = %profile_id,
            nodes = ?built.reality_fingerprint_risks,
            "REALITY nodes with X25519MLKEM768 enabled but no chrome client-fingerprint: \
             the key share is not sent, so recent Xray servers may reject the handshake",
        );
    }

    // 把输出的 proxy-group 顺序快照回写,使其在机场刷新间保持稳定:仍存在的分组保住其位置,
    // 新增的落到末尾。之后的手动拖拽会经 `set_group_order` 覆盖本快照。节点顺序是全局的
    // (`global_nodes.position`),不做 per-profile 快照。尽力而为;绝不让生成失败。
    if snapshot_group_order(
        state,
        profile_id,
        &built.group_names,
        built.context.source_revision,
        &built.group_order_before,
    )
    .await
    .is_err()
    {
        tracing::warn!(profile = %profile_id, "failed to snapshot group order");
    }
    Ok(true)
}

/// 把输出的 proxy-group 顺序回写到 `profiles.group_order`(新增分组持久化到末尾)。节点顺序是
/// 全局的(`global_nodes.position`),从不做 per-profile 快照。空 → NULL。
async fn snapshot_group_order(
    state: &AppState,
    profile_id: &str,
    group_names: &[String],
    source_revision: i64,
    expected: &Option<String>,
) -> ApiResult<()> {
    let group_order = if group_names.is_empty() {
        None
    } else {
        serde_json::to_string(group_names).ok()
    };
    sqlx::query(
        "UPDATE profiles SET group_order = ? \
         WHERE id = ? AND source_revision = ? AND group_order IS ?",
    )
    .bind(&group_order)
    .bind(profile_id)
    .bind(source_revision)
    .bind(expected)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn update_last_fetch(
    state: &AppState,
    profile: &ProfileCore,
    status: &str,
    prefix: &str,
) -> ApiResult<()> {
    // 旧来源或旧公开能力的在途结果不得写入新上下文。
    sqlx::query(
        "UPDATE profiles SET last_fetch_at = ?, last_fetch_status = ? \
         WHERE id = ? AND source_revision = ? AND token = ? \
         AND ? = (SELECT public_path_prefix FROM app_settings WHERE id = 1)",
    )
    .bind(now())
    .bind(status)
    .bind(&profile.id)
    .bind(profile.source_revision)
    .bind(&profile.token)
    .bind(prefix)
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
/// 编辑后调用,使改动立即反映到所服务的订阅,且与完整生成走同一条路径。
pub async fn regenerate_from_cache(state: &AppState, profile_id: &str) -> Regenerated {
    state
        .keyed_lock
        .run(profile_id, regenerate_from_cache_locked(state, profile_id))
        .await
}

/// 供已经持有 profile 锁的调用方使用,避免锁重入。
pub(crate) async fn regenerate_from_cache_locked(
    state: &AppState,
    profile_id: &str,
) -> Regenerated {
    regenerate_locked(state, profile_id, RulesSource::Current)
        .await
        .unwrap_or_else(|_| {
            tracing::warn!(profile = %profile_id, "failed to regenerate cache");
            Regenerated::Pending
        })
}

/// 离线重生成的规则来源。
enum RulesSource {
    /// 库中当前规则(规则/排序编辑后的常规路径)。
    Current,
    /// 产出该缓存的最近一份合法规则(随缓存保存的快照;仅轮换回退使用)。
    Snapshot,
}

/// [`regenerate_from_cache`] 的锁内实现:按 `source` 选择规则文本离线重跑完整转换。
async fn regenerate_locked(
    state: &AppState,
    profile_id: &str,
    source: RulesSource,
) -> ApiResult<Regenerated> {
    let Some(profile) = load_core(state, profile_id).await? else {
        return Ok(Regenerated::Pending);
    };
    let Some((Some(provider_yaml), userinfo, generated_at, rules_snapshot, Some(cache_revision))) =
        sqlx::query_as::<_, (Option<String>, Option<String>, String, Option<String>, Option<i64>)>(
            "SELECT provider_yaml, subscription_userinfo, generated_at, rules_snapshot, source_revision \
             FROM generated_cache WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&state.db)
        .await?
    else {
        return Ok(Regenerated::Pending);
    };
    if cache_revision != profile.source_revision {
        return Ok(Regenerated::Pending);
    }
    let rules_override = match source {
        RulesSource::Current => None,
        RulesSource::Snapshot => match rules_snapshot {
            Some(rules) => Some(rules),
            None => return Ok(Regenerated::Pending),
        },
    };
    let prefix = state.current_prefix();
    let out = match convert(
        state,
        profile_id,
        &profile.token,
        &prefix,
        &provider_yaml,
        rules_override.as_deref(),
    )
    .await?
    {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(profile = %profile_id, "edited config is invalid; keeping the last valid cache");
            return Ok(match e {
                ConvertError::Validation(errors) => Regenerated::Invalid { errors },
                _ => Regenerated::Pending,
            });
        }
    };
    let built = out.into_built(
        provider_yaml,
        userinfo,
        generated_at,
        BuildContext::new(&profile, &prefix),
    );
    if !persist_cache_and_group_order(state, profile_id, &built).await? {
        return Ok(Regenerated::Pending);
    }
    Ok(Regenerated::Applied)
}

/// 重置 token/公共前缀后的缓存迁移:能力地址已更换,缓存须重生成才能继续下发其中的托管链接。
pub async fn regenerate_for_rotation(state: &AppState, profile_id: &str) -> Regenerated {
    state
        .keyed_lock
        .run(
            profile_id,
            regenerate_for_rotation_locked(state, profile_id),
        )
        .await
        .unwrap_or_else(|_| {
            tracing::warn!(profile = %profile_id, "failed to regenerate cache after rotation");
            Regenerated::Pending
        })
}

/// 锁内轮换实现。已有缓存但无法迁移时必须失效;没有缓存时仅返回 Pending。
pub(crate) async fn regenerate_for_rotation_locked(
    state: &AppState,
    profile_id: &str,
) -> ApiResult<Regenerated> {
    let had_cache = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM generated_cache WHERE profile_id = ?)",
    )
    .bind(profile_id)
    .fetch_one(&state.db)
    .await?;
    let draft = regenerate_locked(state, profile_id, RulesSource::Current).await?;
    if matches!(draft, Regenerated::Applied) {
        return Ok(draft);
    }
    if matches!(draft, Regenerated::Invalid { .. }) {
        let fallback = regenerate_locked(state, profile_id, RulesSource::Snapshot).await?;
        if matches!(fallback, Regenerated::Applied) {
            return Ok(fallback);
        }
    }
    if had_cache {
        invalidate_cache(state, profile_id).await?;
        tracing::warn!(
            profile = %profile_id,
            "cleared cached subscription after rotation: no valid output could be migrated"
        );
    }
    Ok(draft)
}

/// 对每条 profile 执行 [`regenerate_from_cache`]。用于影响所有 profile 的改动(全局节点排序)。
/// 逐 profile 尽力而为:某个失败就让该 profile 在下次生成时再吸收改动。
pub async fn regenerate_all_from_cache(state: &AppState) {
    let ids = sqlx::query_scalar::<_, String>("SELECT id FROM profiles")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for id in ids {
        regenerate_from_cache(state, &id).await;
    }
}

/// 对每条 profile 执行 [`regenerate_for_rotation`]。用于重置公共路径前缀。逐 profile 尽力而为。
pub async fn regenerate_all_for_rotation(state: &AppState) {
    let ids = sqlx::query_scalar::<_, String>("SELECT id FROM profiles")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for id in ids {
        regenerate_for_rotation(state, &id).await;
    }
}

// ─── 辅助 ──────────────────────────────────────────────────────────────────────

fn content_hash_of(output_yaml: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(output_yaml.as_bytes());
    crate::util::hex(&hasher.finalize())
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
