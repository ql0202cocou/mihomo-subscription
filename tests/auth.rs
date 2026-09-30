//! Auth and routing acceptance tests: unauthenticated management access is
//! rejected, login issues a usable session, logout invalidates it, the public
//! `/health` needs no auth, and the Origin check blocks cross-site posts.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use mihomo_subscription::app::build_router;
use tower::util::ServiceExt;

use common::{test_state, TempDb};

fn login_body(username: &str, password: &str) -> Body {
    Body::from(format!(
        r#"{{"username":"{username}","password":"{password}"}}"#
    ))
}

fn set_cookie_value(resp_headers: &header::HeaderMap) -> String {
    let raw = resp_headers
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present")
        .to_str()
        .unwrap();
    // "session=<id>; HttpOnly; ..." -> "session=<id>"
    raw.split(';').next().unwrap().to_string()
}

fn same_origin_post(path: &str, body: Body) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "sub.example.com")
        .header(header::ORIGIN, "https://sub.example.com")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn health_requires_no_auth() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn management_api_without_session_is_401() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = app
        .oneshot(
            Request::get("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_with_wrong_credentials_is_401() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = app
        .oneshot(same_origin_post(
            "/api/auth/login",
            login_body("admin", "wrong"),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_then_access_session_then_logout() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    // Log in.
    let resp = app
        .clone()
        .oneshot(same_origin_post(
            "/api/auth/login",
            login_body("admin", "s3cret"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let cookie = set_cookie_value(resp.headers());

    // Authenticated session call returns the username.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/auth/session")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("\"admin\""));

    // Log out, then the same cookie is rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/auth/logout")
                .header(header::COOKIE, &cookie)
                .header(header::HOST, "sub.example.com")
                .header(header::ORIGIN, "https://sub.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(
            Request::get("/api/auth/session")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repeated_failed_logins_are_rate_limited() {
    let temp = TempDb::new();
    // Tighten the login limiter for this test.
    let mut state = test_state(&temp).await;
    {
        use mihomo_subscription::rate_limit::RateLimiter;
        use std::sync::Arc;
        use std::time::Duration;
        let s = Arc::get_mut(&mut state).unwrap();
        s.login_limiter = Arc::new(RateLimiter::new(3, Duration::from_secs(60)));
    }
    let app = build_router(state);

    // First 3 attempts hit the handler (401 for wrong creds); the 4th is
    // blocked by the limiter (429) before reaching credential checks.
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(same_origin_post(
                "/api/auth/login",
                login_body("admin", "wrong"),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    let resp = app
        .oneshot(same_origin_post(
            "/api/auth/login",
            login_body("admin", "wrong"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn cross_site_origin_on_login_is_forbidden() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = app
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::HOST, "sub.example.com")
                .header(header::ORIGIN, "https://evil.example.org")
                .body(login_body("admin", "s3cret"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn wrong_scheme_origin_on_login_is_forbidden() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = app
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::HOST, "sub.example.com")
                .header(header::ORIGIN, "http://sub.example.com")
                .body(login_body("admin", "s3cret"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn missing_origin_on_state_change_is_forbidden() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    let resp = app
        .oneshot(
            Request::post("/api/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::HOST, "sub.example.com")
                .body(login_body("admin", "s3cret"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
