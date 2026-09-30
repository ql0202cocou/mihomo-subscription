//! 应用设置端点验收测试:`GET /api/settings`。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use mihomo_subscription::app::build_router;
use tower::util::ServiceExt;

use common::{json, login, test_state, TempDb};

#[tokio::test]
async fn get_settings_returns_public_path_prefix() {
    let temp = TempDb::new();
    let app = build_router(test_state(&temp).await);

    // 未认证 → 401。
    let resp = app
        .clone()
        .oneshot(Request::get("/api/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let cookie = login(&app).await;
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/settings")
                .header(header::COOKIE, &cookie)
                .header(header::HOST, "sub.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 测试状态固定前缀(见 tests/common/mod.rs)。
    let body = json(resp).await;
    assert_eq!(body["public_path_prefix"].as_str().unwrap(), "testprefix");
}
