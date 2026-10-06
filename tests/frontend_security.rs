//! Browser response protections, using a temporary SPA and no external network.
mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use mihomo_subscription::app::build_router;
use mihomo_subscription::fetch::{FetchError, Fetched, RemoteFetcher};
use tower::ServiceExt;

struct WebDir(PathBuf);

impl WebDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mihomo-web-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("index.html"),
            "<!doctype html><title>test</title>",
        )
        .unwrap();
        std::fs::write(path.join("asset.js"), "console.log('test');").unwrap();
        Self(path)
    }
}

impl Drop for WebDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Provider;

#[async_trait::async_trait]
impl RemoteFetcher for Provider {
    async fn fetch(&self, _: &str) -> Result<Fetched, FetchError> {
        Ok(Fetched {
            body: "proxies:\n  - { name: probe, type: __proto__, server: example.com, port: 443 }\nrules:\n  - MATCH,DIRECT\n".into(),
            subscription_userinfo: None,
        })
    }
}

fn assert_frame_protection(response: &Response<Body>) {
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "frame-ancestors 'none'"
    );
}

#[tokio::test]
async fn spa_deep_links_assets_head_and_not_modified_deny_framing() {
    let temp = common::TempDb::new();
    let web = WebDir::new();
    let mut state = common::test_state_with_fetcher(&temp, Arc::new(Provider)).await;
    Arc::get_mut(&mut state).unwrap().web_dir = web.0.to_str().unwrap().into();
    let app = build_router(state);

    for path in ["/", "/login", "/settings", "/profiles/probe", "/asset.js"] {
        for method in ["GET", "HEAD"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_frame_protection(&response);
        }
    }

    let response = app
        .clone()
        .oneshot(Request::get("/asset.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let modified = response.headers()[header::LAST_MODIFIED].clone();
    let response = app
        .oneshot(
            Request::get("/asset.js")
                .header(header::IF_MODIFIED_SINCE, modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_frame_protection(&response);
}

#[tokio::test]
async fn frame_protection_preserves_auth_and_subscription_downloads() {
    let temp = common::TempDb::new();
    let app = build_router(common::test_state_with_fetcher(&temp, Arc::new(Provider)).await);
    let response = app
        .clone()
        .oneshot(Request::get("/api/profiles").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_frame_protection(&response);

    let cookie = common::login(&app).await;
    let response = app
        .clone()
        .oneshot(common::authed(
            "POST",
            "/api/profiles",
            &cookie,
            r#"{"name":"probe","source_url":"https://provider.example/sub"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_frame_protection(&response);
    let profile = common::json(response).await;
    let path = url::Url::parse(profile["subscription_url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_string();
    let response = app
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_frame_protection(&response);
    assert!(common::text(response).await.contains("__proto__"));
}
