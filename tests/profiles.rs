//! Profile CRUD acceptance tests: auth gating, source URL masking, the
//! write-only URL rule, name-conflict 409, custom node/group validation, and
//! reset-token / reset-public-path effects on the hosted link. Also covers
//! per-profile write serialization: concurrent updates each keep their change,
//! locked saves complete cleanly, a provider-group import that started before a
//! source change writes nothing, and previews ignore caches whose recorded
//! context no longer matches.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use mihomo_subscription::app::build_router;
use mihomo_subscription::fetch::{FetchError, Fetched, RemoteFetcher};
use serde_json::Value;
use tokio::sync::Barrier;
use tower::util::ServiceExt;

use common::{authed, json, login, test_state, test_state_with_fetcher, TempDb};

// ─── helpers ──────────────────────────────────────────────────────────────────

async fn send(app: &Router, req: Request<Body>) -> Response<Body> {
    app.clone().oneshot(req).await.unwrap()
}

async fn create_profile(app: &Router, cookie: &str, name: &str) -> Value {
    let body = format!(
        r#"{{"name":"{name}","source_url":"https://provider.example/sub?token=secret123"}}"#
    );
    let resp = send(app, authed("POST", "/api/profiles", cookie, &body)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    json(resp).await
}

// ─── fetcher fakes ────────────────────────────────────────────────────────────

const PROVIDER_YAML: &str = "proxies:\n  - { name: hk-1, type: ss, server: 1.2.3.4, port: 8388 }\nproxy-groups:\n  - { name: Proxy, type: select, proxies: [hk-1] }\nrules:\n  - MATCH,DIRECT\n";

const PROVIDER_YAML_V2: &str = "proxies:\n  - { name: hk-2, type: ss, server: 5.6.7.8, port: 8388 }\nproxy-groups:\n  - { name: Proxy2, type: select, proxies: [hk-2] }\nrules:\n  - MATCH,DIRECT\n";

/// 按 URL 区分内容的拉取假对象:`v2` 来源返回另一份节点,用于区分换源前后的缓存。
struct StaticFetcher;

#[async_trait::async_trait]
impl RemoteFetcher for StaticFetcher {
    async fn fetch(&self, url: &str) -> Result<Fetched, FetchError> {
        let body = if url.contains("v2") {
            PROVIDER_YAML_V2
        } else {
            PROVIDER_YAML
        };
        Ok(Fetched {
            body: body.to_string(),
            subscription_userinfo: None,
        })
    }
}

/// 可暂停的拉取假对象:暂停期间到达的拉取先在 `entered` 会合点报告「在途」,再阻塞在
/// `release` 会合点,确定性制造「拉取在途」窗口(会合点同步,不靠时序巧合)。
#[derive(Clone)]
struct GatedFetcher {
    paused: Arc<AtomicBool>,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

#[async_trait::async_trait]
impl RemoteFetcher for GatedFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        if self.paused.load(Ordering::SeqCst) {
            self.entered.wait().await;
            self.release.wait().await;
        }
        Ok(Fetched {
            body: PROVIDER_YAML.to_string(),
            subscription_userinfo: None,
        })
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn profiles_require_auth() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = send(
        &app,
        Request::get("/api/profiles").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_masks_url_and_builds_subscription_link() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie, "My Profile").await;

    // The raw provider secret is never echoed back.
    assert_eq!(
        profile["source_url_masked"],
        "https://provider.example/sub?token=***"
    );
    assert!(profile.get("source_url").is_none());

    // The hosted link uses base URL + current prefix + token.
    let url = profile["subscription_url"].as_str().unwrap();
    assert!(url.starts_with("https://sub.example.com/testprefix/api/sub/"));

    // A fresh profile carries an (empty) ruleset and no nodes/groups.
    assert_eq!(profile["rules"]["content"], "");
    assert!(profile["nodes"].as_array().unwrap().is_empty());
    assert!(profile["groups"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn duplicate_name_conflicts() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;

    create_profile(&app, &cookie, "dup").await;
    let body = r#"{"name":"dup","source_url":"https://x.example/s?t=1"}"#;
    let resp = send(&app, authed("POST", "/api/profiles", &cookie, body)).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn disallowed_source_url_is_rejected_at_write_time() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;

    // Non-http scheme, loopback host, and a blocked literal IP are all rejected
    // up front (defense in depth; the fetch path re-validates with DNS pinning).
    for url in [
        "file:///etc/passwd",
        "http://localhost/sub",
        "http://127.0.0.1/sub",
    ] {
        let body = format!(r#"{{"name":"bad-{url:?}","source_url":"{url}"}}"#);
        let resp = send(&app, authed("POST", "/api/profiles", &cookie, &body)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "url={url}");
    }
}

#[tokio::test]
async fn update_keeps_url_when_omitted() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie, "p").await;
    let id = profile["id"].as_str().unwrap();

    // Update only the name; the write-only URL must be retained.
    let resp = send(
        &app,
        authed(
            "PUT",
            &format!("/api/profiles/{id}"),
            &cookie,
            r#"{"name":"renamed"}"#,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let updated = json(resp).await;
    assert_eq!(updated["name"], "renamed");
    assert_eq!(
        updated["source_url_masked"],
        "https://provider.example/sub?token=***"
    );
}

#[tokio::test]
async fn node_requires_valid_yaml_mapping() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;

    // Global custom nodes need no profile. A non-mapping content is rejected.
    let bad = r#"{"name":"n","node_type":"ss","content":"- just\n- a\n- list"}"#;
    let resp = send(&app, authed("POST", "/api/global-nodes", &cookie, bad)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // A valid proxy mapping is accepted.
    let good = r#"{"name":"my-ss","node_type":"ss","content":"name: my-ss\ntype: ss\nserver: 1.2.3.4\nport: 8388"}"#;
    let resp = send(&app, authed("POST", "/api/global-nodes", &cookie, good)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn group_member_and_options_round_trip() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;
    let id = create_profile(&app, &cookie, "p").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let body =
        r#"{"name":"G","group_type":"select","members":["a","DIRECT"],"options":{"interval":300}}"#;
    let resp = send(
        &app,
        authed("POST", &format!("/api/profiles/{id}/groups"), &cookie, body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let group = json(resp).await;
    assert_eq!(group["members"][1], "DIRECT");
    assert_eq!(group["options"]["interval"], 300);
}

#[tokio::test]
async fn group_names_reserved_builtins_are_rejected_but_global_is_allowed() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;
    let id = create_profile(&app, &cookie, "p").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 内置名(DIRECT/REJECT/…)占用会让 Mihomo 以 duplicate name 拒绝加载输出:写入时拦截。
    for name in ["DIRECT", "REJECT", "PASS-RULE"] {
        let body = format!(r#"{{"name":"{name}","group_type":"select","members":[]}}"#);
        let resp = send(
            &app,
            authed(
                "POST",
                &format!("/api/profiles/{id}/groups"),
                &cookie,
                &body,
            ),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "name={name}");
    }

    // GLOBAL 分组是上游支持的用法(上游仅在用户未定义时才内置创建),放行。
    let resp = send(
        &app,
        authed(
            "POST",
            &format!("/api/profiles/{id}/groups"),
            &cookie,
            r#"{"name":"GLOBAL","group_type":"select","members":[]}"#,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn reset_token_changes_the_hosted_link() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;
    let profile = create_profile(&app, &cookie, "p").await;
    let id = profile["id"].as_str().unwrap();
    let old_url = profile["subscription_url"].as_str().unwrap().to_string();

    let resp = send(
        &app,
        authed(
            "POST",
            &format!("/api/profiles/{id}/reset-token"),
            &cookie,
            "",
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let new_url = json(resp).await["subscription_url"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(old_url, new_url);
}

#[tokio::test]
async fn reset_public_path_changes_prefix_for_all_profiles() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;
    create_profile(&app, &cookie, "p").await;

    let resp = send(
        &app,
        authed("POST", "/api/settings/reset-public-path", &cookie, ""),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let new_prefix = json(resp).await["public_path_prefix"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_prefix, "testprefix");

    // The new prefix is reflected in subsequently read hosted links.
    let resp = send(&app, authed("GET", "/api/profiles", &cookie, "")).await;
    let list = json(resp).await;
    let url = list[0]["subscription_url"].as_str().unwrap();
    assert!(url.contains(&format!("/{new_prefix}/api/sub/")));
}

#[tokio::test]
async fn delete_profile_returns_404_afterward() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;
    let id = create_profile(&app, &cookie, "p").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = send(
        &app,
        authed("DELETE", &format!("/api/profiles/{id}"), &cookie, ""),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = send(
        &app,
        authed("GET", &format!("/api/profiles/{id}"), &cookie, ""),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_trims_name_and_rejects_blank() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie, "p").await;
    let path = format!("/api/profiles/{}", profile["id"].as_str().unwrap());

    // 与创建一致:空白名拒绝,首尾空白去除。
    let resp = send(&app, authed("PUT", &path, &cookie, r#"{"name":"   "}"#)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = send(
        &app,
        authed("PUT", &path, &cookie, r#"{"name":"  renamed  "}"#),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["name"], "renamed");
}

// ─── per-profile 写串行化与缓存上下文 ────────────────────────────────────────

#[tokio::test]
async fn concurrent_source_change_and_rename_keep_both_changes() {
    let temp = TempDb::new();
    let state = test_state_with_fetcher(&temp, Arc::new(StaticFetcher)).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let profile = create_profile(&app, &cookie, "p").await;
    let id = profile["id"].as_str().unwrap().to_string();
    let path = format!("/api/profiles/{id}");

    // 并发的换源与改名在 per-profile 锁内串行;任一执行顺序下两者的改动都必须留存。
    let (rename, change) = tokio::join!(
        send(&app, authed("PUT", &path, &cookie, r#"{"name":"renamed"}"#)),
        send(
            &app,
            authed(
                "PUT",
                &path,
                &cookie,
                r#"{"source_url":"https://provider.example/v2?token=y"}"#,
            ),
        ),
    );
    assert_eq!(rename.status(), StatusCode::OK);
    assert_eq!(change.status(), StatusCode::OK);

    let resp = send(&app, authed("GET", &path, &cookie, "")).await;
    let detail = json(resp).await;
    assert_eq!(detail["name"], "renamed");
    assert_eq!(
        detail["source_url_masked"],
        "https://provider.example/v2?token=***"
    );

    // 来源版本恰好递增一次,并与新来源、新名字一致。
    let (revision, name, source_url) = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT source_revision, name, source_url FROM profiles WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(revision, 1);
    assert_eq!(name, "renamed");
    assert_eq!(source_url, "https://provider.example/v2?token=y");

    // 换源后的重拉已按新上下文重建缓存:预览展示新来源节点。
    let resp = send(&app, authed("GET", &format!("{path}/proxies"), &cookie, "")).await;
    let out = json(resp).await;
    assert_eq!(out["generated"], true);
    let names: Vec<&str> = out["proxies"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(
        names.contains(&"hk-2") && !names.contains(&"hk-1"),
        "names={names:?}"
    );
}

#[tokio::test]
async fn locked_saves_and_token_reset_complete_cleanly() {
    let temp = TempDb::new();
    let state = test_state_with_fetcher(&temp, Arc::new(StaticFetcher)).await;
    let app = build_router(state);
    let cookie = login(&app).await;
    let id = create_profile(&app, &cookie, "p").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 这些写接口都在同一 per-profile 锁内完成读改写与(需要的)锁内离线重生成;任一实现
    // 若在锁内再自取同一把锁,这里会因永不完成而在超时处确定性失败。正常路径远快于此阈值。
    const TIMEOUT: Duration = Duration::from_secs(10);

    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "PUT",
                &format!("/api/profiles/{id}/rules"),
                &cookie,
                r#"{"content":"MATCH,DIRECT"}"#,
            ),
        ),
    )
    .await
    .expect("PUT rules did not complete");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "POST",
                &format!("/api/profiles/{id}/groups"),
                &cookie,
                r#"{"name":"G","group_type":"select","members":["hk-1"]}"#,
            ),
        ),
    )
    .await
    .expect("POST group did not complete");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let group_id = json(resp).await["id"].as_str().unwrap().to_string();

    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "PUT",
                &format!("/api/profiles/{id}/group-order"),
                &cookie,
                r#"{"order":["G"]}"#,
            ),
        ),
    )
    .await
    .expect("PUT group-order did not complete");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "PUT",
                &format!("/api/profiles/{id}/node-section-order"),
                &cookie,
                r#"{"order":["custom","provider"]}"#,
            ),
        ),
    )
    .await
    .expect("PUT node-section-order did not complete");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "PUT",
                &format!("/api/profiles/{id}/groups/{group_id}"),
                &cookie,
                r#"{"name":"G2","group_type":"select","members":["hk-1"]}"#,
            ),
        ),
    )
    .await
    .expect("PUT group did not complete");
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "DELETE",
                &format!("/api/profiles/{id}/groups/{group_id}"),
                &cookie,
                "",
            ),
        ),
    )
    .await
    .expect("DELETE group did not complete");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 重置 token 在锁内迁移缓存;当前规则合法,迁移应直接生效。
    let resp = tokio::time::timeout(
        TIMEOUT,
        send(
            &app,
            authed(
                "POST",
                &format!("/api/profiles/{id}/reset-token"),
                &cookie,
                "",
            ),
        ),
    )
    .await
    .expect("POST reset-token did not complete");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");
}

#[tokio::test]
async fn import_started_before_a_source_change_writes_nothing() {
    let temp = TempDb::new();
    let paused = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let fetcher = GatedFetcher {
        paused: paused.clone(),
        entered: entered.clone(),
        release: release.clone(),
    };
    let state = test_state_with_fetcher(&temp, Arc::new(fetcher)).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let id = create_profile(&app, &cookie, "p").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 导入的拉取在途时完成换源:提交阶段重查来源版本必须发现不一致,整批 409、零写入。
    paused.store(true, Ordering::SeqCst);
    let import = tokio::spawn({
        let app = app.clone();
        let cookie = cookie.clone();
        let path = format!("/api/profiles/{id}/import-provider-groups");
        async move {
            app.oneshot(authed("POST", &path, &cookie, ""))
                .await
                .unwrap()
        }
    });

    entered.wait().await; // 导入拉取已在途(它记录的仍是换源前的来源版本)

    // 换源引发的新拉取不再经过闸门;在途的导入拉取仍阻塞,直到换源提交完毕。
    paused.store(false, Ordering::SeqCst);
    let resp = send(
        &app,
        authed(
            "PUT",
            &format!("/api/profiles/{id}"),
            &cookie,
            r#"{"source_url":"https://provider.example/v2?token=y"}"#,
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    release.wait().await; // 放行在途的导入拉取
    let resp = import.await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // 零写入:分组列表仍为空;来源版本只被换源递增了一次。
    let resp = send(
        &app,
        authed("GET", &format!("/api/profiles/{id}/groups"), &cookie, ""),
    )
    .await;
    assert!(json(resp).await.as_array().unwrap().is_empty());
    let revision =
        sqlx::query_scalar::<_, i64>("SELECT source_revision FROM profiles WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(revision, 1);
}

#[tokio::test]
async fn previews_treat_cache_with_stale_context_as_absent() {
    let temp = TempDb::new();
    let state = test_state_with_fetcher(&temp, Arc::new(StaticFetcher)).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let id = create_profile(&app, &cookie, "p").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let proxies_path = format!("/api/profiles/{id}/proxies");

    // 新建的自动拉取已写入带当前上下文的缓存:预览展示节点。
    let resp = send(&app, authed("GET", &proxies_path, &cookie, "")).await;
    let out = json(resp).await;
    assert_eq!(out["generated"], true);
    assert!(out["proxies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "hk-1"));

    // 上下文不明的缓存(升级前形态:来源版本/能力摘要为 NULL)按未生成处理。
    sqlx::query(
        "UPDATE generated_cache SET source_revision = NULL, capability_hash = NULL \
         WHERE profile_id = ?",
    )
    .bind(&id)
    .execute(&state.db)
    .await
    .unwrap();
    let resp = send(&app, authed("GET", &proxies_path, &cookie, "")).await;
    let out = json(resp).await;
    assert_eq!(out["generated"], false);
    assert_eq!(out["generated_at"], Value::Null);
    assert!(out["proxies"].as_array().unwrap().is_empty());
    assert!(out["groups"].as_array().unwrap().is_empty());

    // 重新生成写回当前上下文后恢复展示。
    let resp = send(
        &app,
        authed("POST", &format!("/api/profiles/{id}/generate"), &cookie, ""),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&app, authed("GET", &proxies_path, &cookie, "")).await;
    assert_eq!(json(resp).await["generated"], true);

    // 能力摘要不匹配(如轮换前的残留)同样按未生成处理。
    sqlx::query("UPDATE generated_cache SET capability_hash = 'stale' WHERE profile_id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();
    let resp = send(&app, authed("GET", &proxies_path, &cookie, "")).await;
    assert_eq!(json(resp).await["generated"], false);

    // 详情的指纹风险名单遵循同一判定:有效上下文展示,失效则不展示。
    let resp = send(
        &app,
        authed("POST", &format!("/api/profiles/{id}/generate"), &cookie, ""),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    sqlx::query(
        "UPDATE generated_cache SET reality_fingerprint_risks = '[\"hk-1\"]' WHERE profile_id = ?",
    )
    .bind(&id)
    .execute(&state.db)
    .await
    .unwrap();
    let resp = send(
        &app,
        authed("GET", &format!("/api/profiles/{id}"), &cookie, ""),
    )
    .await;
    assert_eq!(json(resp).await["reality_fingerprint_risks"][0], "hk-1");

    sqlx::query(
        "UPDATE generated_cache SET source_revision = source_revision + 1 WHERE profile_id = ?",
    )
    .bind(&id)
    .execute(&state.db)
    .await
    .unwrap();
    let resp = send(
        &app,
        authed("GET", &format!("/api/profiles/{id}"), &cookie, ""),
    )
    .await;
    assert!(json(resp).await["reality_fingerprint_risks"]
        .as_array()
        .unwrap()
        .is_empty());
    let resp = send(&app, authed("GET", &proxies_path, &cookie, "")).await;
    assert_eq!(json(resp).await["generated"], false);
}
