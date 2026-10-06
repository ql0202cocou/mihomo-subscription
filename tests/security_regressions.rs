//! Regression coverage for the backend security audit. No real network requests.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mihomo_subscription::app::build_router;
use mihomo_subscription::converter::{convert, ConvertInput};
use mihomo_subscription::fetch::{FetchError, Fetched, RemoteFetcher};
use mihomo_subscription::{rate_limit::RateLimiter, yaml};
use serde_json::Value;
use tokio::sync::Semaphore;
use tower::util::ServiceExt;

use common::{authed, json, login, test_state_with_fetcher, text, TempDb};

const PROVIDER: &str = "proxies: []\nrules: []\n";

fn conversion(provider: &str) -> ConvertInput<'_> {
    ConvertInput {
        provider_yaml: provider,
        rules: "MATCH,DIRECT",
        nodes: vec![],
        groups: vec![],
        node_order: vec![],
        node_section_order: vec![],
        group_order: vec![],
        rule_providers: vec![],
    }
}

#[test]
fn scalar_alias_expansion_is_bounded() {
    let blob = "x".repeat(64 * 1024);
    let aliases = ["*a"; 31].join(", ");
    let provider = format!("blob: &a {blob}\ncopies: [{aliases}]\nproxies: []\n");
    assert!(matches!(
        yaml::parse_limited(&provider),
        Err(yaml::YamlError::TooComplex)
    ));
    assert!(convert(conversion(&provider)).is_err());
}

#[test]
fn tagged_values_obey_node_and_depth_limits() {
    let many = format!("payload: !opaque [{}]", ["x"; 10_001].join(","));
    assert!(yaml::parse_limited(&many).is_err());
    assert!(yaml::parse_limited(&many.replace("!opaque ", "")).is_err());
    let deep = format!("payload: !opaque {}x{}", "[".repeat(40), "]".repeat(40));
    assert!(yaml::parse_limited(&deep).is_err());
    assert!(yaml::parse_limited(&deep.replace("!opaque ", "")).is_err());
}

#[test]
fn mapping_keys_obey_node_and_depth_limits() {
    let key = format!("? [{}]\n: value\n", ["x"; 10_001].join(","));
    assert!(yaml::parse_limited(&key).is_err());
    let key = format!("? {}x{}\n: value\n", "[".repeat(40), "]".repeat(40));
    assert!(yaml::parse_limited(&key).is_err());
}

struct FailureFetcher {
    fail: AtomicBool,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl RemoteFetcher for FailureFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        if self.fail.load(Ordering::SeqCst) {
            return Err(FetchError::Timeout);
        }
        Ok(Fetched {
            body: PROVIDER.into(),
            subscription_userinfo: None,
        })
    }
}

async fn create_profile(app: &Router, cookie: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(authed(
            "POST",
            "/api/profiles",
            cookie,
            r#"{"name":"audit","source_url":"https://provider.example/sub"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let value = json(response).await;
    (
        value["id"].as_str().unwrap().into(),
        url::Url::parse(value["subscription_url"].as_str().unwrap())
            .unwrap()
            .path()
            .into(),
    )
}

async fn create_remote(app: &Router, cookie: &str, id: &str) -> Value {
    let response = app.clone().oneshot(authed("POST", &format!("/api/profiles/{id}/rule-sets"), cookie,
        r#"{"name":"audit-rules","behavior":"domain","format":"text","source":"remote","url":"https://old.example/rules","cache":true}"#)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json(response).await
}

async fn burst(app: &Router, path: &str, status: StatusCode) {
    let mut handles = vec![];
    for _ in 0..10 {
        let app = app.clone();
        let request = Request::get(path).body(Body::empty()).unwrap();
        handles.push(tokio::spawn(async move {
            app.oneshot(request).await.unwrap().status()
        }));
    }
    for handle in handles {
        assert_eq!(handle.await.unwrap(), status);
    }
}

#[tokio::test]
async fn failed_subscription_refresh_coalesces_and_obeys_refresh_floor() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FailureFetcher {
        fail: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let mut state = test_state_with_fetcher(&temp, fetcher.clone()).await;
    Arc::get_mut(&mut state)
        .unwrap()
        .public_refresh_min_interval = Duration::from_secs(30);
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (id, path) = create_profile(&app, &cookie).await;
    sqlx::query(
        "UPDATE generated_cache SET generated_at = '2000-01-01T00:00:00Z' WHERE profile_id = ?",
    )
    .bind(id)
    .execute(&state.db)
    .await
    .unwrap();
    fetcher.fail.store(true, Ordering::SeqCst);
    fetcher.calls.store(0, Ordering::SeqCst);
    burst(&app, &path, StatusCode::OK).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    burst(&app, &path, StatusCode::OK).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_remote_ruleset_refresh_coalesces() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FailureFetcher {
        fail: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let state = test_state_with_fetcher(&temp, fetcher.clone()).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (id, _) = create_profile(&app, &cookie).await;
    let ruleset = create_remote(&app, &cookie, &id).await;
    let path = url::Url::parse(ruleset["url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_string();
    fetcher.fail.store(true, Ordering::SeqCst);
    fetcher.calls.store(0, Ordering::SeqCst);
    burst(&app, &path, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    burst(&app, &path, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    sqlx::query("UPDATE profile_rule_sets SET last_fetch_at = '2000-01-01T00:00:00Z' WHERE id = ?")
        .bind(ruleset["id"].as_str().unwrap())
        .execute(&state.db)
        .await
        .unwrap();
    fetcher.fail.store(false, Ordering::SeqCst);
    burst(&app, &path, StatusCode::OK).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
}

struct BlockedFetcher {
    started: Semaphore,
    release: Semaphore,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl RemoteFetcher for BlockedFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        Ok(Fetched {
            body: PROVIDER.into(),
            subscription_userinfo: None,
        })
    }
    async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if url == "https://old.example/rules" {
            self.started.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Ok(b"+.old-untrusted.example\n".to_vec())
        } else {
            Ok(b"+.new-trusted.example\n".to_vec())
        }
    }
}

#[tokio::test]
async fn old_inflight_fetch_cannot_repopulate_cache_after_source_update() {
    let temp = TempDb::new();
    let fetcher = Arc::new(BlockedFetcher {
        started: Semaphore::new(0),
        release: Semaphore::new(0),
        calls: AtomicUsize::new(0),
    });
    let state = test_state_with_fetcher(&temp, fetcher.clone()).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (id, _) = create_profile(&app, &cookie).await;
    let ruleset = create_remote(&app, &cookie, &id).await;
    let path = url::Url::parse(ruleset["url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_string();
    let public_app = app.clone();
    let request = Request::get(&path).body(Body::empty()).unwrap();
    let inflight = tokio::spawn(async move { public_app.oneshot(request).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), fetcher.started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let update = app.clone().oneshot(authed("PUT", &format!("/api/profiles/{id}/rule-sets/{}", ruleset["id"].as_str().unwrap()), &cookie,
        r#"{"name":"audit-rules","behavior":"domain","format":"text","source":"remote","url":"https://new.example/rules","cache":true}"#)).await.unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    fetcher.release.add_permits(1);
    assert_eq!(inflight.await.unwrap().status(), StatusCode::NOT_FOUND);
    let response = app
        .oneshot(Request::get(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(text(response).await, "+.new-trusted.example\n");
    let current_url: String = sqlx::query_scalar("SELECT url FROM profile_rule_sets WHERE id = ?")
        .bind(ruleset["id"].as_str().unwrap())
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(current_url, "https://new.example/rules");
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        2,
        "new definition must fetch its own source"
    );
}

#[tokio::test]
async fn queued_old_public_path_request_is_revoked() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FailureFetcher {
        fail: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let state = test_state_with_fetcher(&temp, fetcher).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (id, old_path) = create_profile(&app, &cookie).await;
    let response = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{id}/rule-sets"),
            &cookie,
            r#"{"name":"local","behavior":"domain","format":"text","content":"+.example.org"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{id}/rules"),
            &cookie,
            r#"{"content":"RULE-SET,local,DIRECT\nMATCH,DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Simulate an existing refresh holding the profile lock. Poll the old-link
    // request while it is blocked, then rotate the prefix through the real API.
    let (mut waiting, reset) = state
        .keyed_lock
        .run(&id, async {
            let mut waiting = Box::pin(
                app.clone()
                    .oneshot(Request::get(&old_path).body(Body::empty()).unwrap()),
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(100), waiting.as_mut())
                    .await
                    .is_err()
            );
            let reset_app = app.clone();
            let reset_request = authed("POST", "/api/settings/reset-public-path", &cookie, "");
            let reset =
                tokio::spawn(async move { reset_app.oneshot(reset_request).await.unwrap() });
            tokio::time::timeout(Duration::from_secs(3), async {
                while state.current_prefix() == "testprefix" {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            (waiting, reset)
        })
        .await;
    let old_response = waiting.as_mut().await.unwrap();
    assert_eq!(old_response.status(), StatusCode::NOT_FOUND);
    assert!(!text(old_response).await.contains(&state.current_prefix()));
    assert_eq!(reset.await.unwrap().status(), StatusCode::OK);
}

#[test]
fn rate_limiter_bounds_active_keys_without_resetting_existing_budget() {
    let limiter = RateLimiter::new(1, Duration::from_secs(60));
    for n in 0..10_000 {
        assert!(limiter.try_acquire(&format!("dl:2001:db8::{n:x}")));
    }
    assert!(!limiter.try_acquire("new-ip"));
    assert!(!limiter.try_acquire("dl:2001:db8::0"));
}

#[tokio::test]
async fn failed_subscription_without_cache_retries_after_floor() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FailureFetcher {
        fail: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
    });
    let mut state = test_state_with_fetcher(&temp, fetcher.clone()).await;
    Arc::get_mut(&mut state)
        .unwrap()
        .public_refresh_min_interval = Duration::from_secs(30);
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (id, path) = create_profile(&app, &cookie).await;
    fetcher.calls.store(0, Ordering::SeqCst);
    burst(&app, &path, StatusCode::SERVICE_UNAVAILABLE).await;
    burst(&app, &path, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    sqlx::query("UPDATE profiles SET public_refresh_at = '2000-01-01T00:00:00Z' WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();
    fetcher.fail.store(false, Ordering::SeqCst);
    burst(&app, &path, StatusCode::OK).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn ruleset_refresh_rechecks_rotated_capabilities_before_delivery() {
    for reset_token in [false, true] {
        let temp = TempDb::new();
        let fetcher = Arc::new(BlockedFetcher {
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            calls: AtomicUsize::new(0),
        });
        let state = test_state_with_fetcher(&temp, fetcher.clone()).await;
        let app = build_router(state.clone());
        let cookie = login(&app).await;
        let (id, _) = create_profile(&app, &cookie).await;
        let ruleset = create_remote(&app, &cookie, &id).await;
        let path = url::Url::parse(ruleset["url"].as_str().unwrap())
            .unwrap()
            .path()
            .to_string();
        let public_app = app.clone();
        let inflight = tokio::spawn(async move {
            public_app
                .oneshot(Request::get(&path).body(Body::empty()).unwrap())
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(3), fetcher.started.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let reset_path = if reset_token {
            format!("/api/profiles/{id}/reset-token")
        } else {
            "/api/settings/reset-public-path".into()
        };
        let response = app
            .clone()
            .oneshot(authed("POST", &reset_path, &cookie, ""))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        fetcher.release.add_permits(1);
        assert_eq!(inflight.await.unwrap().status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn queued_ruleset_requests_recheck_all_hosting_conditions() {
    for change in [
        "url", "enabled", "name", "format", "source", "cache", "delete",
    ] {
        let temp = TempDb::new();
        let fetcher = Arc::new(BlockedFetcher {
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            calls: AtomicUsize::new(0),
        });
        let state = test_state_with_fetcher(&temp, fetcher.clone()).await;
        let app = build_router(state.clone());
        let cookie = login(&app).await;
        let (id, _) = create_profile(&app, &cookie).await;
        let ruleset = create_remote(&app, &cookie, &id).await;
        let rsid = ruleset["id"].as_str().unwrap();
        let path = url::Url::parse(ruleset["url"].as_str().unwrap())
            .unwrap()
            .path()
            .to_string();
        let key = format!("profile-ruleset:{rsid}");
        let response = state.keyed_lock.run(&key, async {
            let mut waiting = Box::pin(app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()));
            assert!(tokio::time::timeout(Duration::from_millis(30), waiting.as_mut()).await.is_err());
            let mut body = serde_json::json!({"name":"audit-rules", "behavior":"domain", "format":"text", "source":"remote", "url":"https://new.example/rules", "cache":true});
            match change {
                "enabled" => body["enabled"] = false.into(),
                "name" => body["name"] = "renamed".into(),
                "format" => body["format"] = "yaml".into(),
                "source" => { body["source"] = "manual".into(); body["content"] = "+.manual.example".into(); },
                "cache" => body["cache"] = false.into(),
                _ => {}
            }
            let method = if change == "delete" { "DELETE" } else { "PUT" };
            let response = app.clone().oneshot(authed(method, &format!("/api/profiles/{id}/rule-sets/{rsid}"), &cookie, &body.to_string())).await.unwrap();
            assert!(response.status().is_success());
            (waiting,)
        }).await;
        let response = response.0.await.unwrap();
        if change == "url" {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(text(response).await, "+.new-trusted.example\n");
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{change}");
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0, "{change}");
        }
    }
}
