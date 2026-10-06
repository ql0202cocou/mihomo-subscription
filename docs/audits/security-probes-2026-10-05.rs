//! Audit probes: passing means the documented unsafe behavior was reproduced.
//! Copy to tests/security_audit_probe.rs to run; see ../security-audit-2026-10-05.md.
//! These are observations of the audit baseline, not regression safety assertions.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
fn scalar_aliases_amplify_bytes_without_hitting_limits() {
    let blob = "x".repeat(64 * 1024);
    let aliases = ["*a"; 31].join(", ");
    let provider = format!("blob: &a {blob}\ncopies: [{aliases}]\nproxies: []\n");
    assert!(yaml::parse_limited(&provider).is_ok());
    let (output, _) = convert(conversion(&provider)).unwrap();
    assert!(output.len() > provider.len() * 30);
    eprintln!(
        "scalar alias amplification: input={} output={}",
        provider.len(),
        output.len()
    );
}

#[test]
fn tagged_values_bypass_node_and_depth_limits() {
    let many = format!("payload: !opaque [{}]", ["x"; 10_001].join(","));
    assert!(yaml::parse_limited(&many).is_ok());
    assert!(yaml::parse_limited(&many.replace("!opaque ", "")).is_err());
    let deep = format!("payload: !opaque {}x{}", "[".repeat(40), "]".repeat(40));
    assert!(yaml::parse_limited(&deep).is_ok());
    assert!(yaml::parse_limited(&deep.replace("!opaque ", "")).is_err());
}

#[test]
fn mapping_keys_bypass_node_and_depth_limits() {
    let key = format!("? [{}]\n: value\n", ["x"; 10_001].join(","));
    assert!(yaml::parse_limited(&key).is_ok());
    let key = format!("? {}x{}\n: value\n", "[".repeat(40), "]".repeat(40));
    assert!(yaml::parse_limited(&key).is_ok());
}

#[test]
fn provider_can_set_client_controller_and_remove_its_authentication() {
    let provider = "proxies: []\nexternal-controller: 0.0.0.0:9090\nsecret: ''\nallow-lan: true\nbind-address: '*'\nauthentication: []\n";
    let (output, _) = convert(conversion(provider)).unwrap();
    let root: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();
    assert_eq!(root["external-controller"].as_str(), Some("0.0.0.0:9090"));
    assert_eq!(root["secret"].as_str(), Some(""));
    assert_eq!(root["allow-lan"].as_bool(), Some(true));
    assert!(root["authentication"].as_sequence().unwrap().is_empty());
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
async fn failed_subscription_refresh_does_not_coalesce_or_obey_refresh_floor() {
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
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 10);
    eprintln!("10 concurrent stale subscription requests produced 10 failed upstream fetches despite 30-second floor");
}

#[tokio::test]
async fn failed_remote_ruleset_refresh_does_not_coalesce() {
    let temp = TempDb::new();
    let fetcher = Arc::new(FailureFetcher {
        fail: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let state = test_state_with_fetcher(&temp, fetcher.clone()).await;
    let app = build_router(state);
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
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 10);
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
async fn old_inflight_fetch_repopulates_cache_after_source_update() {
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
    assert_eq!(inflight.await.unwrap().status(), StatusCode::OK);
    let response = app
        .oneshot(Request::get(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(text(response).await, "+.old-untrusted.example\n");
    let current_url: String = sqlx::query_scalar("SELECT url FROM profile_rule_sets WHERE id = ?")
        .bind(ruleset["id"].as_str().unwrap())
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(current_url, "https://new.example/rules");
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        1,
        "new source never fetched; old content cached for new definition"
    );
}

#[tokio::test]
async fn queued_old_public_path_request_learns_rotated_prefix() {
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
    assert_eq!(old_response.status(), StatusCode::OK);
    let output = text(old_response).await;
    assert_eq!(reset.await.unwrap().status(), StatusCode::OK);
    let new_prefix = state.current_prefix();
    assert!(
        output.contains(&format!("/{new_prefix}/api/sub/")),
        "old public link reveals new prefix in rule-provider URL"
    );
    let token = old_path.rsplit('/').next().unwrap();
    let learned_path = format!("/{new_prefix}/api/sub/{token}");
    let response = app
        .oneshot(Request::get(learned_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "learned link remains usable after rotation completes"
    );
}

#[test]
fn rate_limiter_accepts_more_than_cleanup_threshold_active_keys() {
    let limiter = RateLimiter::new(1, Duration::from_secs(60));
    let started = Instant::now();
    for n in 0..15_000 {
        assert!(limiter.try_acquire(&format!("dl:2001:db8::{n:x}")));
    }
    eprintln!(
        "15,000 different active IP keys admitted; elapsed={:?}",
        started.elapsed()
    );
}
