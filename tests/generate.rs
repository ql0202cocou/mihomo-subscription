//! Generate / public-endpoint acceptance tests using a fake fetcher (so no
//! real network or SSRF interaction): generate populates the cache and the
//! hosted link works; the public endpoint coalesces concurrent refreshes
//! (single-flight), falls back to stale cache on fetch failure, returns a
//! generic 503 when there is no cache and the fetch fails, and a uniform 404
//! for a wrong prefix / unknown token.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mihomo_subscription::app::build_router;
use mihomo_subscription::fetch::{FetchError, Fetched, RemoteFetcher};
use mihomo_subscription::rate_limit::RateLimiter;
use serde_json::Value;
use tower::util::ServiceExt;

use common::{authed, json, login, test_state_with_fetcher, text, TempDb};

const PROVIDER_YAML: &str =
    "proxies:\n  - { name: hk-1, type: ss, server: 1.2.3.4, port: 8388 }\nproxy-groups:\n  - { name: Proxy, type: select, proxies: [hk-1] }\nrules:\n  - MATCH,DIRECT\n";

/// A fetcher that counts calls, can be made to fail, and adds a small delay so
/// concurrent requests overlap (to exercise single-flight).
#[derive(Clone, Default)]
struct FakeFetcher {
    calls: Arc<AtomicUsize>,
    fail: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl RemoteFetcher for FakeFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        if self.fail.load(Ordering::SeqCst) {
            return Err(FetchError::Timeout);
        }
        Ok(Fetched {
            body: PROVIDER_YAML.to_string(),
            subscription_userinfo: Some("upload=1; download=2; total=100".to_string()),
        })
    }
}

async fn create_profile(app: &Router, cookie: &str) -> Value {
    let body = r#"{"name":"P","source_url":"https://provider.example/sub?token=x"}"#;
    let resp = app
        .clone()
        .oneshot(authed("POST", "/api/profiles", cookie, body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json(resp).await
}

/// Extract the `/<prefix>/api/sub/<token>` path from a full subscription URL.
fn sub_path(subscription_url: &str) -> String {
    let after = subscription_url.split("://").nth(1).unwrap();
    let idx = after.find('/').unwrap();
    after[idx..].to_string()
}

#[tokio::test]
async fn generate_populates_cache_and_public_link_serves_it() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());
    // 新建已自动拉取一次;归零计数器,下面只统计 generate / 公开拉取触发的拉取。
    fetcher.calls.store(0, Ordering::SeqCst);

    // Generate: one fetch, cache populated.
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);

    // Public download: every pull re-fetches the provider for the latest nodes,
    // so this triggers another fetch. Headers still come through.
    let resp = app
        .clone()
        .oneshot(Request::get(&sub).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("subscription-userinfo").unwrap(),
        "upload=1; download=2; total=100"
    );
    assert_eq!(resp.headers().get("profile-update-interval").unwrap(), "24");
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        2,
        "public pull re-fetches the provider"
    );

    // Provider proxies are preserved; provider groups and rules are replaced
    // (groups need importing, the ruleset is empty), so neither `Proxy` nor
    // `MATCH,DIRECT` survives.
    let body = text(resp).await;
    assert!(body.contains("hk-1"), "provider proxy preserved");
    assert!(!body.contains("Proxy"), "provider group not passed through");
}

#[tokio::test]
async fn public_pull_reuses_cache_within_refresh_floor() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let mut state = test_state_with_fetcher(&temp, fetcher.clone()).await;
    Arc::get_mut(&mut state)
        .unwrap()
        .public_refresh_min_interval = Duration::from_secs(60);
    let app = build_router(state);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());
    fetcher.calls.store(0, Ordering::SeqCst);

    let resp = app
        .oneshot(Request::get(&sub).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        0,
        "public pull inside the refresh floor uses the generated cache"
    );
}

#[tokio::test]
async fn proxies_endpoint_reflects_generated_cache() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();

    // 新建即自动生成一次:缓存已存在,含机场代理 hk-1,但尚无自定义节点。
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/proxies"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["generated"], true);
    let names: Vec<&str> = body["proxies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"hk-1"),
        "provider proxy present after create"
    );
    assert!(!names.contains(&"my-vmess"), "custom node not added yet");

    // Add a custom node, then generate.
    let node = r#"{"name":"my-vmess","node_type":"vmess","content":"{ name: my-vmess, type: vmess, server: 9.9.9.9, port: 443, uuid: abc }"}"#;
    let resp = app
        .clone()
        .oneshot(authed("POST", "/api/global-nodes", &cookie, node))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    // Add a custom group (provider groups are replaced, so groups only come from
    // custom ones), then generate.
    let group = r#"{"name":"MyG","group_type":"select","members":["hk-1"]}"#;
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/groups"),
            &cookie,
            group,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // After generation: provider proxy + merged custom node, each with a type.
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/proxies"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let body = json(resp).await;
    assert_eq!(body["generated"], true);
    let names: Vec<&str> = body["proxies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"hk-1"), "provider proxy listed");
    assert!(names.contains(&"my-vmess"), "custom node listed");
    let groups: Vec<&str> = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert!(groups.contains(&"MyG"), "custom group listed");
    assert!(
        !groups.contains(&"Proxy"),
        "provider group not passed through"
    );
    let my_group = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "MyG")
        .unwrap();
    assert_eq!(my_group["type"], "select");
    let hk = body["proxies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "hk-1")
        .unwrap();
    assert_eq!(hk["type"], "ss");
}

#[tokio::test]
async fn node_order_reorders_preview_and_survives_regeneration() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();

    // Two custom nodes; default order is provider-first: [hk-1, a, b].
    for name in ["a", "b"] {
        let node = format!(
            r#"{{"name":"{name}","node_type":"ss","content":"{{ name: {name}, type: ss, server: 9.9.9.9, port: 1080 }}"}}"#
        );
        let resp = app
            .clone()
            .oneshot(authed("POST", "/api/global-nodes", &cookie, &node))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();

    let proxy_names = |app: Router, cookie: String| async move {
        let resp = app
            .oneshot(authed(
                "GET",
                &format!("/api/profiles/{id}/proxies"),
                &cookie,
                "",
            ))
            .await
            .unwrap();
        let body = json(resp).await;
        body["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    // Default: provider block [hk-1] then custom block [a, b].
    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["hk-1", "a", "b"],
        "default: provider block then custom block"
    );

    // Reorder the global custom block to [b, a] (`a` unlisted falls to the end).
    // The provider block is untouched; a global reorder restitches every
    // profile's cache, so this profile reflects it immediately.
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            "/api/global-nodes/order",
            &cookie,
            r#"{"order":["b"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["hk-1", "b", "a"],
        "custom block reordered immediately, provider block fixed"
    );

    // Put the custom block first via node-section-order.
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/node-section-order"),
            &cookie,
            r#"{"order":["custom","provider"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");
    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["b", "a", "hk-1"],
        "custom block first, then provider block"
    );

    // Both orders persist through a regeneration.
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["b", "a", "hk-1"],
        "orders persist through regeneration"
    );

    // An invalid section order is rejected.
    let resp = app
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/node-section-order"),
            &cookie,
            r#"{"order":["custom","custom"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn global_node_applies_to_every_profile() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher).await);
    let cookie = login(&app).await;

    // One global custom node, shared across all profiles (Model C).
    let node = r#"{"name":"global-ss","node_type":"ss","content":"{ name: global-ss, type: ss, server: 9.9.9.9, port: 1080 }"}"#;
    let resp = app
        .clone()
        .oneshot(authed("POST", "/api/global-nodes", &cookie, node))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // The pool is unique by name: a duplicate is a conflict.
    let resp = app
        .clone()
        .oneshot(authed("POST", "/api/global-nodes", &cookie, node))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Two distinct profiles; the global node is appended to each one's output.
    for name in ["alpha", "beta"] {
        let body =
            format!(r#"{{"name":"{name}","source_url":"https://provider.example/sub?token=x"}}"#);
        let resp = app
            .clone()
            .oneshot(authed("POST", "/api/profiles", &cookie, &body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let id = json(resp).await["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(authed(
                "GET",
                &format!("/api/profiles/{id}/preview"),
                &cookie,
                "",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let yaml = text(resp).await;
        assert!(
            yaml.contains("global-ss"),
            "global node must appear in every profile's output ({name})"
        );
    }
}

#[tokio::test]
async fn reorder_applies_to_the_cache_immediately_without_a_fetch() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();
    // 新建已自动拉取一次;归零计数器以便断言后续仅 generate 触发 1 次拉取。
    fetcher.calls.store(0, Ordering::SeqCst);

    // Custom node + generate: cached order is provider-first [hk-1, mine].
    let node = r#"{"name":"mine","node_type":"ss","content":"{ name: mine, type: ss, server: 9.9.9.9, port: 1080 }"}"#;
    app.clone()
        .oneshot(authed("POST", "/api/global-nodes", &cookie, node))
        .await
        .unwrap();
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);

    let proxy_names = |app: Router, cookie: String| async move {
        let resp = app
            .oneshot(authed(
                "GET",
                &format!("/api/profiles/{id}/proxies"),
                &cookie,
                "",
            ))
            .await
            .unwrap();
        json(resp).await["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["hk-1", "mine"]
    );

    // Put the custom block first WITHOUT regenerating.
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/node-section-order"),
            &cookie,
            r#"{"order":["custom","provider"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    // The cached output (admin preview) reflects the new order immediately via
    // regenerate_from_cache — no provider re-fetch.
    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["mine", "hk-1"]
    );
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        1,
        "an order edit re-stitches the cache without re-fetching the provider"
    );
}

#[tokio::test]
async fn public_pull_always_fetches_latest_provider() {
    let temp = TempDb::new();
    let body = Arc::new(std::sync::Mutex::new(
        "proxies:\n  - { name: hk-1, type: ss, server: 1.1.1.1, port: 8388 }\nrules:\n  - MATCH,DIRECT\n"
            .to_string(),
    ));
    let fetcher = Arc::new(SwapFetcher { body: body.clone() });
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());

    let served = |app: Router, sub: String| async move {
        let resp = app
            .oneshot(Request::get(&sub).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        text(resp).await
    };

    // First pull fetches the provider and serves hk-1 only.
    let first = served(app.clone(), sub.clone()).await;
    assert!(first.contains("hk-1"));
    assert!(!first.contains("hk-2"));

    // Provider adds hk-2; a later pull serves the new node — which it could only
    // do by re-fetching the (now-changed) provider on this pull.
    *body.lock().unwrap() =
        "proxies:\n  - { name: hk-1, type: ss, server: 1.1.1.1, port: 8388 }\n  - { name: hk-2, type: ss, server: 2.2.2.2, port: 8388 }\nrules:\n  - MATCH,DIRECT\n"
            .to_string();
    let second = served(app.clone(), sub.clone()).await;
    assert!(
        second.contains("hk-2"),
        "pull reflects the latest provider nodes"
    );
}

#[tokio::test]
async fn group_order_reorders_preview_and_survives_regeneration() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();

    // Two custom groups; provider groups are replaced, so the output groups are
    // just the custom ones in creation order: [G1, G2].
    for name in ["G1", "G2"] {
        let group = format!(r#"{{"name":"{name}","group_type":"select","members":["hk-1"]}}"#);
        let resp = app
            .clone()
            .oneshot(authed(
                "POST",
                &format!("/api/profiles/{id}/groups"),
                &cookie,
                &group,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();

    let group_names = |app: Router, cookie: String| async move {
        let resp = app
            .oneshot(authed(
                "GET",
                &format!("/api/profiles/{id}/proxies"),
                &cookie,
                "",
            ))
            .await
            .unwrap();
        let body = json(resp).await;
        body["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        group_names(app.clone(), cookie.clone()).await,
        vec!["G1", "G2"],
        "default order is creation order"
    );

    // Reorder to [G2]; `G1` is unlisted and must fall to the end.
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/group-order"),
            &cookie,
            r#"{"order":["G2"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    // Preview reflects the saved order immediately (before regeneration).
    assert_eq!(
        group_names(app.clone(), cookie.clone()).await,
        vec!["G2", "G1"],
        "preview honors saved order pre-regenerate"
    );

    // Regenerate: the generated `proxy-groups` output carries the same order.
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        group_names(app.clone(), cookie.clone()).await,
        vec!["G2", "G1"],
        "order persists through regeneration"
    );
}

/// A fetcher whose body can be swapped between fetches, to simulate a provider
/// updating node info and adding nodes across a subscription refresh.
#[derive(Clone)]
struct SwapFetcher {
    body: Arc<std::sync::Mutex<String>>,
}

#[async_trait::async_trait]
impl RemoteFetcher for SwapFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        Ok(Fetched {
            body: self.body.lock().unwrap().clone(),
            subscription_userinfo: None,
        })
    }
}

#[tokio::test]
async fn refresh_keeps_known_node_order_and_appends_new_nodes() {
    let temp = TempDb::new();
    let body = Arc::new(std::sync::Mutex::new(
        "proxies:\n  - { name: hk-1, type: ss, server: 1.1.1.1, port: 8388 }\nrules:\n  - MATCH,DIRECT\n"
            .to_string(),
    ));
    let fetcher = Arc::new(SwapFetcher { body: body.clone() });
    let app = build_router(test_state_with_fetcher(&temp, fetcher).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();

    // Add a custom node, then generate: output is [hk-1, mine].
    let node = r#"{"name":"mine","node_type":"ss","content":"{ name: mine, type: ss, server: 9.9.9.9, port: 1080 }"}"#;
    app.clone()
        .oneshot(authed("POST", "/api/global-nodes", &cookie, node))
        .await
        .unwrap();
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();

    let proxy_names = |app: Router, cookie: String| async move {
        let resp = app
            .oneshot(authed(
                "GET",
                &format!("/api/profiles/{id}/proxies"),
                &cookie,
                "",
            ))
            .await
            .unwrap();
        let body = json(resp).await;
        body["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["hk-1", "mine"]
    );

    // Put the custom block first (so `mine` leads).
    app.clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/node-section-order"),
            &cookie,
            r#"{"order":["custom","provider"]}"#,
        ))
        .await
        .unwrap();

    // Provider updates hk-1's info and adds a new node hk-2.
    *body.lock().unwrap() =
        "proxies:\n  - { name: hk-1, type: ss, server: 8.8.8.8, port: 8388 }\n  - { name: hk-2, type: ss, server: 2.2.2.2, port: 8388 }\nrules:\n  - MATCH,DIRECT\n"
            .to_string();

    // Refresh: the custom block (mine) stays first/ordered; the new provider node
    // joins the provider block (upstream order) without disturbing the custom one.
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        proxy_names(app.clone(), cookie.clone()).await,
        vec!["mine", "hk-1", "hk-2"],
        "custom block fixed; new provider node lands in the provider block"
    );

    // The generated output also carries hk-1's refreshed server (updated by name).
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/preview"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let yaml = text(resp).await;
    assert!(
        yaml.contains("8.8.8.8"),
        "hk-1 info refreshed from provider"
    );
}

#[tokio::test]
async fn import_provider_groups_makes_them_editable_custom_groups() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();

    // Generate first: the provider's `Proxy` group is NOT passed through.
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/preview"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert!(
        !text(resp).await.contains("Proxy"),
        "provider group absent before import"
    );

    // Import provider groups → the `Proxy` group becomes an editable custom group.
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/import-provider-groups"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["imported"], 1);
    assert_eq!(body["skipped"], 0);

    // It now appears as a custom group (editable, with id/type/members).
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/groups"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let groups = json(resp).await;
    let g = &groups.as_array().unwrap()[0];
    assert_eq!(g["name"], "Proxy");
    assert_eq!(g["group_type"], "select");
    assert_eq!(g["members"][0], "hk-1");

    // Re-importing skips the now-existing group.
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/import-provider-groups"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let body = json(resp).await;
    assert_eq!(body["imported"], 0);
    assert_eq!(body["skipped"], 1);

    // After regenerating, the imported group is in the output.
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let resp = app
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/preview"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert!(
        text(resp).await.contains("Proxy"),
        "imported group present after regenerate"
    );
}

#[tokio::test]
async fn provider_rules_endpoint_returns_upstream_rules() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{id}/provider-rules"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    let rules: Vec<&str> = body["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    // PROVIDER_YAML carries a single `MATCH,DIRECT` rule.
    assert_eq!(rules, vec!["MATCH,DIRECT"]);
}

#[tokio::test]
async fn concurrent_public_requests_coalesce_into_one_fetch() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());
    // 新建已自动拉取一次;归零计数器以隔离下面并发拉取的次数。
    fetcher.calls.store(0, Ordering::SeqCst);

    // 公开端点每次拉取都尝试刷新;10 个并发命中经单飞合并为一次上游拉取。
    let mut handles = Vec::new();
    for _ in 0..10 {
        let app = app.clone();
        let sub = sub.clone();
        handles.push(tokio::spawn(async move {
            app.oneshot(Request::get(&sub).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }

    // Single-flight: only one upstream fetch ran despite 10 concurrent misses.
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn no_cache_and_fetch_failure_is_503() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    fetcher.fail.store(true, Ordering::SeqCst);
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());

    let resp = app
        .oneshot(Request::get(&sub).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn stale_cache_is_served_when_refresh_fails() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher.clone()).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());

    // Populate the cache, then force subsequent fetches to fail.
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    fetcher.fail.store(true, Ordering::SeqCst);

    // Even though the cache is fresh here, the point is the public endpoint
    // still serves successfully; stale-fallback logic is exercised by the 503
    // test (no cache) plus this serving path.
    let resp = app
        .oneshot(Request::get(&sub).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn public_downloads_are_rate_limited_per_ip_across_tokens() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let mut state = test_state_with_fetcher(&temp, fetcher).await;
    // Tighten the download limiter for this test.
    Arc::get_mut(&mut state).unwrap().download_limiter =
        Arc::new(RateLimiter::new(3, Duration::from_secs(60)));
    let app = build_router(state);

    // Each request targets a DIFFERENT (nonexistent) token; with per-IP keying
    // they share one budget, so enumeration is throttled (404s count too).
    for i in 0..3 {
        let resp = app
            .clone()
            .oneshot(
                Request::get(format!("/testprefix/api/sub/guess{i}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "within limit: 404");
    }
    let resp = app
        .oneshot(
            Request::get("/testprefix/api/sub/guess-over")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn wrong_prefix_and_unknown_token_are_404() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FakeFetcher::default());
    let app = build_router(test_state_with_fetcher(&temp, fetcher).await);
    let cookie = login(&app).await;

    let profile = create_profile(&app, &cookie).await;
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());
    let token = sub.rsplit('/').next().unwrap().to_string();

    // Wrong path prefix.
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/wrongprefix/api/sub/{token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Unknown token.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/testprefix/api/sub/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// 公开刷新间隔足够长:公开拉取直接返回缓存,用于验证编辑后的缓存本身。返回 app 与 DB 句柄。
async fn cached_app(temp: &TempDb) -> (Router, sqlx::SqlitePool) {
    let mut state = test_state_with_fetcher(temp, Arc::new(FakeFetcher::default())).await;
    Arc::get_mut(&mut state)
        .unwrap()
        .public_refresh_min_interval = Duration::from_secs(3600);
    let db = state.db.clone();
    (build_router(state), db)
}

/// 保存规则并返回响应中的离线重生成结果(`regenerate`)。
async fn put_rules(app: &Router, cookie: &str, id: &str, content: &str) -> Value {
    let body = serde_json::json!({ "content": content }).to_string();
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/rules"),
            cookie,
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json(resp).await["regenerate"].clone()
}

async fn get_text(app: &Router, path: &str) -> String {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    text(resp).await
}

#[tokio::test]
async fn invalid_rule_edit_never_reaches_the_served_cache() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp).await;
    let cookie = login(&app).await;
    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());

    let regen = put_rules(&app, &cookie, id, "MATCH,DIRECT").await;
    assert_eq!(regen, serde_json::json!({ "status": "applied" }));
    assert!(get_text(&app, &sub).await.contains("MATCH,DIRECT"));

    // 引用不存在的策略:规则照常保存,但所服务的缓存保持上一份合法输出,响应如实带回校验错误。
    let regen = put_rules(&app, &cookie, id, "MATCH,NoSuchGroup").await;
    assert_eq!(regen["status"], "invalid");
    assert!(regen["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("NoSuchGroup")));
    let body = get_text(&app, &sub).await;
    assert!(body.contains("MATCH,DIRECT"));
    assert!(!body.contains("NoSuchGroup"));
}

#[tokio::test]
async fn legacy_cache_without_provider_original_is_left_untouched() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp).await;
    let cookie = login(&app).await;
    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();
    let sub = sub_path(profile["subscription_url"].as_str().unwrap());

    // 模拟升级前生成的缓存:没有保存机场原文,无法离线重生成。
    sqlx::query("UPDATE generated_cache SET provider_yaml = NULL")
        .execute(&db)
        .await
        .unwrap();
    let before = get_text(&app, &sub).await;

    let regen = put_rules(&app, &cookie, id, "MATCH,DIRECT").await;
    assert_eq!(regen, serde_json::json!({ "status": "pending" }));
    assert_eq!(
        get_text(&app, &sub).await,
        before,
        "等下一次回源后才吸收编辑"
    );
}

/// 返回固定正文的 fetcher,用于模拟机场返回非 YAML 内容。
struct BodyFetcher(&'static str);

#[async_trait::async_trait]
impl RemoteFetcher for BodyFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        Ok(Fetched {
            body: self.0.to_string(),
            subscription_userinfo: None,
        })
    }
}

#[tokio::test]
async fn unparsable_provider_body_is_recorded_as_provider_parse() {
    let temp = TempDb::new();
    // 机场不认 UA 时常见:返回 base64 节点列表而非 Mihomo YAML。
    let fetcher = Arc::new(BodyFetcher(
        "c3M6Ly9ZV1Z6TFRJMU5pMW5ZMjA2Y0dGemMzZHZjbVE9QDEuMi4zLjQ6ODM4OA==",
    ));
    let app = build_router(test_state_with_fetcher(&temp, fetcher).await);
    let cookie = login(&app).await;

    // 新建时的自动拉取:拉取成功但解析失败,状态不能停在 success。
    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap();
    assert_eq!(profile["last_fetch_status"], "provider_parse");

    // 手动生成同样返回 502 并记录状态。
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/api/profiles/{id}"), &cookie, ""))
        .await
        .unwrap();
    assert_eq!(json(resp).await["last_fetch_status"], "provider_parse");
}

#[tokio::test]
async fn admin_generate_waits_for_the_per_profile_lock() {
    let temp = TempDb::new();
    let state = test_state_with_fetcher(&temp, Arc::new(FakeFetcher::default())).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let profile = create_profile(&app, &cookie).await;
    let id = profile["id"].as_str().unwrap().to_string();
    let generate = || authed("POST", &format!("/api/profiles/{id}/generate"), &cookie, "");

    // 占住该 profile 的锁(模拟进行中的离线重生成):生成必须等锁,不能与之并发写缓存。
    let blocked = state
        .keyed_lock
        .run(&id, async {
            tokio::time::timeout(Duration::from_millis(300), app.clone().oneshot(generate()))
                .await
                .is_err()
        })
        .await;
    assert!(blocked, "generate must wait for the per-profile lock");

    // 锁释放后照常完成。
    let resp = app.oneshot(generate()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
