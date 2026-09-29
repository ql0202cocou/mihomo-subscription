//! Per-profile 自包含规则库(③ 托管规则库)验收测试:per-profile CRUD + 跨订阅同名不冲突、
//! 按订阅 token 隔离的公开托管与统一 404、被 `RULE-SET` 规则引用时注入到输出 `rule-providers:`
//! (指向 token 隔离链接)、remote cache 关闭直注上游、以及从全局 ② 库导入。用 fake fetcher。

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mihomo_subscription::app::build_router;
use mihomo_subscription::fetch::{FetchError, Fetched, RemoteFetcher};
use serde_json::Value;
use tower::util::ServiceExt;

use common::{authed, json, login, test_state_with_fetcher, text, TempDb};

const PROVIDER_YAML: &str =
    "proxies:\n  - { name: hk-1, type: ss, server: 1.2.3.4, port: 8388 }\nrules:\n  - MATCH,DIRECT\n";

#[derive(Clone, Default)]
struct FakeFetcher;

#[async_trait::async_trait]
impl RemoteFetcher for FakeFetcher {
    async fn fetch(&self, _url: &str) -> Result<Fetched, FetchError> {
        Ok(Fetched {
            body: PROVIDER_YAML.to_string(),
            subscription_userinfo: None,
        })
    }
}

/// 建 profile,返回 (id, token, 订阅相对路径)。
async fn create_profile(app: &Router, cookie: &str, name: &str) -> (String, String, String) {
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            "/api/profiles",
            cookie,
            &format!(r#"{{"name":"{name}","source_url":"https://provider.example/sub"}}"#),
        ))
        .await
        .unwrap();
    let profile = json(resp).await;
    let id = profile["id"].as_str().unwrap().to_string();
    let sub = profile["subscription_url"].as_str().unwrap();
    let token = sub.rsplit('/').next().unwrap().to_string();
    let sub_path = {
        let after = sub.split("://").nth(1).unwrap();
        after[after.find('/').unwrap()..].to_string()
    };
    (id, token, sub_path)
}

async fn create_rs(app: &Router, cookie: &str, profile_id: &str, body: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{profile_id}/rule-sets"),
            cookie,
            body,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    json(resp).await
}

async fn put_rules(app: &Router, cookie: &str, profile_id: &str, content: &str) {
    let body = serde_json::json!({ "content": content }).to_string();
    app.clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{profile_id}/rules"),
            cookie,
            &body,
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn crud_is_per_profile_and_name_unique_within_profile() {
    let temp = TempDb::new();
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(FakeFetcher)).await);
    let cookie = login(&app).await;
    let (p1, token1, _) = create_profile(&app, &cookie, "P1").await;
    let (p2, _token2, _) = create_profile(&app, &cookie, "P2").await;

    let created = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.doubleclick.net\n+.googleads.com"}"#,
    )
    .await;
    // 托管链接按订阅 token 隔离。
    assert_eq!(
        created["url"].as_str().unwrap(),
        format!("https://sub.example.com/testprefix/api/sub/{token1}/r/ads/domain.yaml")
    );
    assert_eq!(created["count"].as_u64().unwrap(), 2);
    let rsid = created["id"].as_str().unwrap().to_string();

    // 同一订阅内重名 → 409。
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets"),
            &cookie,
            r#"{"name":"ads","behavior":"ipcidr","format":"text"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // 另一订阅可用同名 → 201(per-profile 隔离)。
    create_rs(
        &app,
        &cookie,
        &p2,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.x"}"#,
    )
    .await;

    // P1 列表只含自己的一条。
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{p1}/rule-sets"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await.as_array().unwrap().len(), 1);

    // 删除 → 204,再删 → 404。
    let resp = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn public_serve_is_token_scoped_and_404s() {
    let temp = TempDb::new();
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(FakeFetcher)).await);
    let cookie = login(&app).await;
    let (p1, token1, _) = create_profile(&app, &cookie, "P1").await;
    let (_p2, token2, _) = create_profile(&app, &cookie, "P2").await;

    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.doubleclick.net"}"#,
    )
    .await;
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"direct","behavior":"classical","format":"text","content":"DOMAIN-SUFFIX,lan"}"#,
    )
    .await;

    let get = |path: String| {
        app.clone()
            .oneshot(Request::get(&path).body(Body::empty()).unwrap())
    };

    // yaml → payload 列表。
    let resp = get(format!("/testprefix/api/sub/{token1}/r/ads/domain.yaml"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = text(resp).await;
    assert!(body.contains("payload:") && body.contains("+.doubleclick.net"));

    // text → 逐行原样。
    let resp = get(format!(
        "/testprefix/api/sub/{token1}/r/direct/classical.text"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(text(resp).await.contains("DOMAIN-SUFFIX,lan"));

    // 文件名不符 / 前缀错 / token 错(P2 无 ads)/ 名不存在 → 统一 404。
    for path in [
        format!("/testprefix/api/sub/{token1}/r/ads/domain.text"),
        format!("/wrongprefix/api/sub/{token1}/r/ads/domain.yaml"),
        format!("/testprefix/api/sub/{token2}/r/ads/domain.yaml"),
        format!("/testprefix/api/sub/{token1}/r/nope/domain.yaml"),
        "/testprefix/api/sub/badtoken/r/ads/domain.yaml".to_string(),
    ] {
        let resp = get(path).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn referenced_rule_set_injected_with_token_scoped_link() {
    let temp = TempDb::new();
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(FakeFetcher)).await);
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;

    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"extra","behavior":"ipcidr","format":"text","content":"10.0.0.0/8"}"#,
    )
    .await;

    let resp = app
        .clone()
        .oneshot(Request::get(&sub_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = text(resp).await;
    assert!(body.contains("rule-providers:"));
    assert!(
        body.contains(&format!(
            "https://sub.example.com/testprefix/api/sub/{token1}/r/ads/domain.yaml"
        )),
        "ads 指向按订阅隔离的托管链接"
    );
    assert!(!body.contains("/r/extra/"), "未被引用的规则集不注入");
}

#[tokio::test]
async fn remote_cache_off_injects_upstream_url() {
    let temp = TempDb::new();
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(FakeFetcher)).await);
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;

    put_rules(&app, &cookie, &p1, "RULE-SET,direct,DIRECT\nMATCH,DIRECT").await;
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"direct","behavior":"classical","format":"text","source":"remote","cache":false,"url":"https://up.example/direct.txt"}"#,
    )
    .await;

    let resp = app
        .clone()
        .oneshot(Request::get(&sub_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = text(resp).await;
    assert!(
        body.contains("https://up.example/direct.txt"),
        "直注上游 URL"
    );
    assert!(
        !body.contains(&format!("/api/sub/{token1}/r/direct/")),
        "不指向面板托管"
    );

    // cache 关闭的远程集托管端点 404。
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/testprefix/api/sub/{token1}/r/direct/classical.text"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn import_from_global_copies_and_appends_rule() {
    let temp = TempDb::new();
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(FakeFetcher)).await);
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;

    // 全局 ② 库里有一条 manual 规则集。
    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"gads","behavior":"domain","format":"yaml","content":"+.g.example"}"#,
        ))
        .await
        .unwrap();

    // 导入到订阅 ③ + 追加 RULE-SET 规则行。
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets/import"),
            &cookie,
            r#"{"names":["gads"],"policy":"DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["imported"].as_u64().unwrap(), 1);

    // ③ 列表含 gads;profile 规则文本含 RULE-SET 行。
    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{p1}/rule-sets"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let list = json(resp).await;
    assert!(list.as_array().unwrap().iter().any(|r| r["name"] == "gads"));

    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/api/profiles/{p1}"), &cookie, ""))
        .await
        .unwrap();
    let detail = json(resp).await;
    assert!(detail["rules"]["content"]
        .as_str()
        .unwrap()
        .contains("RULE-SET,gads,DIRECT"));

    // 拉订阅:gads 注入 rule-providers,指向 token 隔离托管链接。
    let resp = app
        .clone()
        .oneshot(Request::get(&sub_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = text(resp).await;
    assert!(body.contains(&format!(
        "https://sub.example.com/testprefix/api/sub/{token1}/r/gads/domain.yaml"
    )));
}

/// 公开刷新间隔足够长的 app:公开拉取直接返回缓存而不回源重生成,用于验证「编辑后的缓存本身」。
async fn cached_app(temp: &TempDb) -> Router {
    let mut state = test_state_with_fetcher(temp, Arc::new(FakeFetcher)).await;
    Arc::get_mut(&mut state)
        .unwrap()
        .public_refresh_min_interval = Duration::from_secs(3600);
    build_router(state)
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
async fn import_injects_rule_provider_into_served_cache() {
    let temp = TempDb::new();
    let app = cached_app(&temp).await;
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;

    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"gads","behavior":"domain","format":"yaml","content":"+.g.example"}"#,
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets/import"),
            &cookie,
            r#"{"names":["gads"],"policy":"DIRECT"}"#,
        ))
        .await
        .unwrap();

    // 不回源:缓存里的规则行与 rule-providers 条目必须同时存在。
    let body = get_text(&app, &sub_path).await;
    assert!(body.contains("RULE-SET,gads,DIRECT"));
    assert!(body.contains(&format!(
        "https://sub.example.com/testprefix/api/sub/{token1}/r/gads/domain.yaml"
    )));
}

#[tokio::test]
async fn reset_token_rewrites_hosted_rule_set_links_in_cache() {
    let temp = TempDb::new();
    let app = cached_app(&temp).await;
    let cookie = login(&app).await;
    let (p1, old_token, _) = create_profile(&app, &cookie, "P1").await;
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;

    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/reset-token"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let new_token = json(resp).await["token"].as_str().unwrap().to_string();

    let body = get_text(&app, &format!("/testprefix/api/sub/{new_token}")).await;
    assert!(body.contains(&format!(
        "https://sub.example.com/testprefix/api/sub/{new_token}/r/ads/domain.yaml"
    )));
    assert!(!body.contains(&old_token), "缓存中不再残留旧 token 链接");
}

#[tokio::test]
async fn reset_public_path_rewrites_hosted_rule_set_links_in_cache() {
    let temp = TempDb::new();
    let app = cached_app(&temp).await;
    let cookie = login(&app).await;
    let (p1, token1, _) = create_profile(&app, &cookie, "P1").await;
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;

    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            "/api/settings/reset-public-path",
            &cookie,
            "",
        ))
        .await
        .unwrap();
    let prefix = json(resp).await["public_path_prefix"]
        .as_str()
        .unwrap()
        .to_string();

    let body = get_text(&app, &format!("/{prefix}/api/sub/{token1}")).await;
    assert!(body.contains(&format!(
        "https://sub.example.com/{prefix}/api/sub/{token1}/r/ads/domain.yaml"
    )));
    assert!(!body.contains("/testprefix/"), "缓存中不再残留旧前缀链接");
}

#[tokio::test]
async fn generate_rejects_rule_referencing_disabled_rule_set() {
    let temp = TempDb::new();
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(FakeFetcher)).await);
    let cookie = login(&app).await;
    let (p1, _, _) = create_profile(&app, &cookie, "P1").await;
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;

    // 停用后规则行仍引用它:生成必须报错,而不是静默产出缺少 rule-providers 的配置。
    let rsid = rs["id"].as_str().unwrap();
    app.clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example","enabled":false}"#,
        ))
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/generate"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let details = json(resp).await["error"]["details"].to_string();
    assert!(details.contains("unknown rule-set `ads`"), "{details}");
}
