//! Profile CRUD acceptance tests: auth gating, source URL masking, the
//! write-only URL rule, name-conflict 409, custom node/group validation, and
//! reset-token / reset-public-path effects on the hosted link.

mod common;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::Router;
use mihomo_subscription::app::build_router;
use serde_json::Value;
use tower::util::ServiceExt;

use common::{authed, json, login, test_state, TempDb};

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
