// Historical unsafe-baseline probes; on fixed code use tests/frontend_security.rs.
// Do not add these old unsafe assertions to normal CI unchanged.
// To rerun: copy this file to tests/frontend_audit_probe.rs and remove the
// following path attribute (keep `mod common;`), then run the matching cargo test.
#[path = "../../tests/common/mod.rs"]
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mihomo_subscription::app::build_router;
use mihomo_subscription::fetch::{FetchError, Fetched, RemoteFetcher};
use std::sync::Arc;
use tower::ServiceExt;

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

#[tokio::test]
async fn provider_type_reaches_frontend_and_spa_has_no_frame_protection() {
    let temp = common::TempDb::new();
    let app = build_router(common::test_state_with_fetcher(&temp, Arc::new(Provider)).await);
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
    let profile = common::json(response).await;
    let id = profile["id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(common::authed(
            "GET",
            &format!("/api/profiles/{id}/proxies"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        common::json(response).await["proxies"][0]["type"],
        "__proto__"
    );

    for route in ["/", "/login", "/settings"] {
        let response = app
            .clone()
            .oneshot(Request::get(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("x-frame-options").is_none());
        assert!(response.headers().get("content-security-policy").is_none());
    }
}
