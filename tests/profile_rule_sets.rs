//! Per-profile 自包含规则库(③ 托管规则库)验收测试:per-profile CRUD + 跨订阅同名不冲突、
//! 按订阅 token 隔离的公开托管与统一 404、被 `RULE-SET` 规则引用时注入到输出 `rule-providers:`
//! (指向 token 隔离链接)、remote cache 关闭直注上游、从全局 ② 库导入(含幽灵名/非法名拦截)、
//! 被引用规则集的改名/停用/删除保护、编辑后缓存内托管链接的更新与旧 token 链接不残留。
//! 用 fake fetcher。

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mihomo_subscription::app::build_router;
use serde_json::Value;
use tower::util::ServiceExt;

use common::{
    authed, cached_app, json, login, rule_api, sub_path, test_state_with_fetcher, text,
    BodyFetcher, TempDb,
};

const PROVIDER_YAML: &str =
    "proxies:\n  - { name: hk-1, type: ss, server: 1.2.3.4, port: 8388 }\nrules:\n  - MATCH,DIRECT\n";

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
    (id, token, sub_path(sub))
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
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
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
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
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

    // 文件名不符 / 前缀错 / token 错(P2 无 ads)/ 名不存在 / 路径段无法解码 → 统一 404。
    for path in [
        format!("/testprefix/api/sub/{token1}/r/ads/domain.text"),
        format!("/wrongprefix/api/sub/{token1}/r/ads/domain.yaml"),
        format!("/testprefix/api/sub/{token2}/r/ads/domain.yaml"),
        format!("/testprefix/api/sub/{token1}/r/nope/domain.yaml"),
        "/testprefix/api/sub/badtoken/r/ads/domain.yaml".to_string(),
        format!("/testprefix/api/sub/{token1}/r/ads/%FF%FF.yaml"),
    ] {
        let resp = get(path).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn referenced_rule_set_injected_with_token_scoped_link() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
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
async fn logic_nested_rule_set_reference_is_injected() {
    let temp = TempDb::new();
    // SUB-RULE 指向的子规则 `s` 由机场透传。
    let provider = "proxies:\n  - { name: hk-1, type: ss, server: 1.2.3.4, port: 8388 }\n\
                    sub-rules:\n  s:\n    - MATCH,DIRECT\nrules:\n  - MATCH,DIRECT\n";
    let app = build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(provider))).await);
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;

    // RULE-SET 嵌套在逻辑规则(AND/SUB-RULE)内,同样要被注入 rule-providers。
    put_rules(
        &app,
        &cookie,
        &p1,
        "AND,((RULE-SET,ads),(NETWORK,tcp)),DIRECT\nSUB-RULE,(RULE-SET,sub1,DIRECT),s\nMATCH,DIRECT",
    )
    .await;
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
        r#"{"name":"sub1","behavior":"classical","format":"text","content":"DOMAIN-SUFFIX,lan"}"#,
    )
    .await;

    let resp = app
        .clone()
        .oneshot(Request::get(&sub_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = text(resp).await;
    assert!(
        body.contains(&format!(
            "https://sub.example.com/testprefix/api/sub/{token1}/r/ads/domain.yaml"
        )),
        "逻辑规则内嵌套引用的规则集也要注入托管链接"
    );
    assert!(
        body.contains(&format!(
            "https://sub.example.com/testprefix/api/sub/{token1}/r/sub1/classical.text"
        )),
        "SUB-RULE 行内嵌套引用的规则集也要注入托管链接"
    );
}

#[tokio::test]
async fn remote_cache_off_injects_upstream_url() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
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
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
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
    let body = json(resp).await;
    assert_eq!(body["imported"].as_u64().unwrap(), 1);
    assert_eq!(body["regenerate"]["status"], "applied");

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
    assert_eq!(body["rules_content"], detail["rules"]["content"]);

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
async fn global_edits_reach_followers_at_their_existing_rule_links() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, _, sub1) = create_profile(&app, &cookie, "P1").await;
    let (p2, _, sub2) = create_profile(&app, &cookie, "P2").await;
    let initial = r#"{"name":"ads","behavior":"domain","format":"text","content":"old.example"}"#;
    let global = rule_api(&app, &cookie, "POST", "/api/rule-sets", initial).await;
    let global_path = format!("/api/rule-sets/{}", global["id"].as_str().unwrap());
    let import = r#"{"names":["ads"],"policy":"REJECT"}"#;
    for id in [&p1, &p2] {
        let result = rule_api(
            &app,
            &cookie,
            "POST",
            &format!("/api/profiles/{id}/rule-sets/import"),
            import,
        )
        .await;
        assert_eq!(result["imported"], 1);
    }
    let before = get_text(&app, &sub1).await;
    let updated = r#"{"name":"ads","behavior":"domain","format":"text","content":"new.example\nsecond.example"}"#;
    rule_api(&app, &cookie, "PUT", &global_path, updated).await;
    for sub in [&sub1, &sub2] {
        assert_eq!(
            get_text(&app, &format!("{sub}/r/ads/domain.text")).await,
            "new.example\nsecond.example\n"
        );
    }
    assert_eq!(get_text(&app, &sub1).await, before);
    let local_path = format!("/api/profiles/{p2}/rule-sets");
    let rows = rule_api(&app, &cookie, "GET", &local_path, "").await;
    assert_eq!(rows[0]["count"], 2);
    assert_eq!(rows[0]["global_rule_set_id"], global["id"]);
    let local = rule_api(
        &app,
        &cookie,
        "PUT",
        &format!("{local_path}/{}", rows[0]["id"].as_str().unwrap()),
        initial,
    )
    .await;
    assert!(local["global_rule_set_id"].is_null());

    // 全局改名后继续按 ID 同步，订阅里的引用名和策略各自保留。
    let renamed =
        r#"{"name":"renamed","behavior":"domain","format":"yaml","content":"final.example"}"#;
    rule_api(&app, &cookie, "PUT", &global_path, renamed).await;
    let config = get_text(&app, &sub1).await;
    assert!(config.contains(&format!("{sub1}/r/ads/domain.yaml")));
    assert!(config.contains("RULE-SET,ads,REJECT"));
    assert!(get_text(&app, &format!("{sub1}/r/ads/domain.yaml"))
        .await
        .contains("final.example"));
    assert_eq!(
        get_text(&app, &format!("{sub2}/r/ads/domain.text")).await,
        "old.example\n"
    );

    let deleted = app
        .clone()
        .oneshot(authed("DELETE", &global_path, &cookie, ""))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    rule_api(&app, &cookie, "POST", "/api/rule-sets", initial).await;
    let rows = rule_api(
        &app,
        &cookie,
        "GET",
        &format!("/api/profiles/{p1}/rule-sets"),
        "",
    )
    .await;
    assert!(rows[0]["global_rule_set_id"].is_null());
    assert_eq!(rows[0]["content"], "final.example");
}

#[tokio::test]
async fn reimport_after_global_rename_uses_local_alias_even_with_a_name_collision() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (profile, _, sub) = create_profile(&app, &cookie, "Alias").await;
    let base = format!("/api/profiles/{profile}/rule-sets");
    let import = format!("{base}/import");
    let definition =
        r#"{"name":"A","behavior":"domain","format":"text","content":"follow.example"}"#;
    let global = rule_api(&app, &cookie, "POST", "/api/rule-sets", definition).await;
    rule_api(
        &app,
        &cookie,
        "POST",
        &import,
        r#"{"names":["A"],"policy":"REJECT"}"#,
    )
    .await;
    let rows = rule_api(&app, &cookie, "GET", &base, "").await;
    rule_api(
        &app,
        &cookie,
        "PUT",
        &format!("/api/rule-sets/{}", global["id"].as_str().unwrap()),
        &definition.replace(r#""A""#, r#""B""#),
    )
    .await;
    let legacy_import = r#"{"names":["B"],"policy":"DIRECT"}"#;
    let result = rule_api(&app, &cookie, "POST", &import, legacy_import).await;
    assert_eq!(result["imported"], 0);
    let after_rename = rule_api(&app, &cookie, "GET", &base, "").await;
    assert_eq!(after_rename.as_array().unwrap().len(), 1);
    assert_eq!(after_rename[0]["id"], rows[0]["id"]);
    create_rs(
        &app,
        &cookie,
        &profile,
        r#"{"name":"B","behavior":"domain","format":"text","content":"own.example"}"#,
    )
    .await;

    let nested = "AND,((RULE-SET,A),(DOMAIN,one.example)),REJECT\nMATCH,DIRECT";
    put_rules(&app, &cookie, &profile, nested).await;
    let result = rule_api(&app, &cookie, "POST", &import, legacy_import).await;
    assert_eq!(result["imported"], 0);
    assert_eq!(result["rules_content"], nested);
    put_rules(&app, &cookie, &profile, "MATCH,DIRECT").await;
    let request = serde_json::json!({"names":["B"],"policy":"REJECT",
        "expected":{"B":{"global_id":global["id"],"local_id":rows[0]["id"]}}});
    let restored = rule_api(&app, &cookie, "POST", &import, &request.to_string()).await;
    assert_eq!(restored["rules_content"], "RULE-SET,A,REJECT\nMATCH,DIRECT");
    let rows_after = rule_api(&app, &cookie, "GET", &base, "").await;
    assert_eq!(rows_after.as_array().unwrap().len(), 2);
    assert!(rows_after[1]["global_rule_set_id"].is_null());
    assert_eq!(
        get_text(&app, &format!("{sub}/r/A/domain.text")).await,
        "follow.example\n"
    );
}

#[tokio::test]
async fn stale_import_selection_conflicts_and_rolls_back_the_whole_batch() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (profile, _, _) = create_profile(&app, &cookie, "Snapshot").await;
    let base = format!("/api/profiles/{profile}/rule-sets");
    let import = format!("{base}/import");
    let definition = |name: &str| {
        serde_json::json!({"name":name,"format":"text","behavior":"domain","content":"global.example"}).to_string()
    };
    let a = rule_api(&app, &cookie, "POST", "/api/rule-sets", &definition("A")).await;
    let b = rule_api(&app, &cookie, "POST", "/api/rule-sets", &definition("B")).await;
    let request = serde_json::json!({"names":["B","A"], "policy":"DIRECT", "expected":{
        "A":{"global_id":a["id"],"local_id":null}, "B":{"global_id":b["id"],"local_id":null}
    }});
    let local = create_rs(
        &app,
        &cookie,
        &profile,
        &definition("A").replace("global.example", "local.example"),
    )
    .await;
    let post = |body: serde_json::Value| {
        app.clone()
            .oneshot(authed("POST", &import, &cookie, &body.to_string()))
    };
    assert_eq!(
        post(request.clone()).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let rows = rule_api(&app, &cookie, "GET", &base, "").await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0], local);
    let detail = rule_api(
        &app,
        &cookie,
        "GET",
        &format!("/api/profiles/{profile}"),
        "",
    )
    .await;
    assert!(!detail["rules"]["content"]
        .as_str()
        .unwrap()
        .contains("RULE-SET"));

    // 即使本地 ID 相同，「原先已关联」也不能悄悄降级成引用独立定义。
    let mut expected_link = request.clone();
    expected_link["expected"]["A"]["local_id"] = local["id"].clone();
    assert_eq!(
        post(expected_link.clone()).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    expected_link["link_existing"] = serde_json::json!({"A":local["id"]});
    assert_eq!(post(expected_link).await.unwrap().status(), StatusCode::OK);

    // 全局项在窗口打开后被同名重建，按名字和本地 ID 仍无法识别这种替换。
    let deleted = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/api/rule-sets/{}", a["id"].as_str().unwrap()),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    rule_api(&app, &cookie, "POST", "/api/rule-sets", &definition("A")).await;
    let stale_global = serde_json::json!({"names":["A"],"policy":"DIRECT","expected":{"A":{"global_id":a["id"],"local_id":local["id"]}}});
    assert_eq!(
        post(stale_global).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let partial = serde_json::json!({"names":["A","B"],"policy":"DIRECT","expected":{"B":{"global_id":b["id"]}}});
    assert_eq!(
        post(partial).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn recreating_a_global_set_restores_followers_and_respects_local_edits() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (id, _, sub) = create_profile(&app, &cookie, "Following").await;
    let (custom_id, _, custom_sub) = create_profile(&app, &cookie, "Customized").await;
    let original = r#"{"name":"A","behavior":"domain","format":"text","content":"old.example"}"#;
    let updated = r#"{"name":"A","behavior":"domain","format":"yaml","content":"new.example"}"#;
    let import = r#"{"names":["A"],"policy":"REJECT"}"#;
    let global = rule_api(&app, &cookie, "POST", "/api/rule-sets", original).await;
    let base = format!("/api/profiles/{id}/rule-sets");
    let custom_base = format!("/api/profiles/{custom_id}/rule-sets");
    // 历史规则草稿已有引用，只需补齐定义，已有策略优先于导入策略。
    let draft = "RULE-SET,A,DIRECT\nMATCH,REJECT";
    put_rules(&app, &cookie, &id, draft).await;
    for list in [&base, &custom_base] {
        let result = rule_api(&app, &cookie, "POST", &format!("{list}/import"), import).await;
        assert_eq!(result["imported"], 1);
    }
    let local_id = rule_api(&app, &cookie, "GET", &base, "").await[0]["id"].clone();
    let deleted = app
        .clone()
        .oneshot(authed(
            "DELETE",
            &format!("/api/rule-sets/{}", global["id"].as_str().unwrap()),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        get_text(&app, &format!("{sub}/r/A/domain.text")).await,
        "old.example\n"
    );
    let custom = rule_api(&app, &cookie, "GET", &custom_base, "").await;
    rule_api(
        &app,
        &cookie,
        "PUT",
        &format!("{custom_base}/{}", custom[0]["id"].as_str().unwrap()),
        &original.replace("old.example", "mine.example"),
    )
    .await;

    let recreated = rule_api(&app, &cookie, "POST", "/api/rule-sets", updated).await;
    assert_ne!(recreated["id"], global["id"]);
    let followed = rule_api(&app, &cookie, "GET", &base, "").await;
    assert_eq!(followed[0]["id"], local_id);
    assert_eq!(followed[0]["global_rule_set_id"], recreated["id"]);
    let config = get_text(&app, &sub).await;
    assert!(config.contains(&format!("{sub}/r/A/domain.yaml")));
    assert!(get_text(&app, &format!("{sub}/r/A/domain.yaml"))
        .await
        .contains("new.example"));
    let result = rule_api(&app, &cookie, "POST", &format!("{base}/import"), import).await;
    assert_eq!(result["imported"], 0);
    assert_eq!(result["rules_content"], draft);

    // 只删规则行时订阅仍跟随，再导入仅补回一条引用。
    put_rules(&app, &cookie, &id, "MATCH,DIRECT").await;
    let result = rule_api(&app, &cookie, "POST", &format!("{base}/import"), import).await;
    assert_eq!(result["rules_content"], "RULE-SET,A,REJECT\nMATCH,DIRECT");
    rule_api(
        &app,
        &cookie,
        "PUT",
        &format!("/api/rule-sets/{}", recreated["id"].as_str().unwrap()),
        &updated.replace("new.example", "next.example"),
    )
    .await;
    assert!(get_text(&app, &format!("{sub}/r/A/domain.yaml"))
        .await
        .contains("next.example"));
    assert_eq!(
        get_text(&app, &format!("{custom_sub}/r/A/domain.text")).await,
        "mine.example\n"
    );
}

#[tokio::test]
async fn existing_independent_definition_can_follow_without_changing_its_rule() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, _, sub) = create_profile(&app, &cookie, "P1").await;
    let base = format!("/api/profiles/{p1}/rule-sets");
    let import_path = format!("{base}/import");
    let local = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"my.example"}"#,
    )
    .await;
    let rules = "DOMAIN,first.example,DIRECT\nRULE-SET,ads,REJECT\nMATCH,DIRECT";
    put_rules(&app, &cookie, &p1, rules).await;
    let remote = r#"{"name":"ads","behavior":"domain","format":"text","source":"remote","url":"https://rules.example/first","cache":false}"#;
    let global = rule_api(&app, &cookie, "POST", "/api/rule-sets", remote).await;
    let global_path = format!("/api/rule-sets/{}", global["id"].as_str().unwrap());
    let plain = r#"{"names":["ads"],"policy":"DIRECT"}"#;
    assert_eq!(
        rule_api(&app, &cookie, "POST", &import_path, plain).await["imported"],
        0
    );
    assert!(get_text(&app, &format!("{sub}/r/ads/domain.yaml"))
        .await
        .contains("my.example"));

    let request =
        serde_json::json!({"names":["ads"],"policy":"DIRECT","link_existing":{"ads":local["id"]}})
            .to_string();
    let result = rule_api(&app, &cookie, "POST", &import_path, &request).await;
    assert_eq!(result["imported"], 1);
    assert_eq!(result["rules_content"], rules);
    assert_eq!(result["regenerate"]["status"], "applied");
    assert!(get_text(&app, &sub)
        .await
        .contains("https://rules.example/first"));
    assert_eq!(
        rule_api(&app, &cookie, "POST", &import_path, &request).await["imported"],
        0
    );

    rule_api(
        &app,
        &cookie,
        "PUT",
        &global_path,
        &remote.replace("/first", "/second"),
    )
    .await;
    let config = get_text(&app, &sub).await;
    assert!(config.contains("https://rules.example/second"));
    assert!(!config.contains("https://rules.example/first"));
    let rows = rule_api(&app, &cookie, "GET", &base, "").await;
    assert_eq!(rows[0]["id"], local["id"]);
    assert_eq!(rows[0]["global_rule_set_id"], global["id"]);
}

#[tokio::test]
async fn import_injects_rule_provider_into_served_cache() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
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
async fn import_inserts_rule_before_the_fallback_match() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, _, sub_path) = create_profile(&app, &cookie, "P1").await;
    put_rules(&app, &cookie, &p1, "MATCH,DIRECT").await;
    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"gads","behavior":"domain","format":"yaml","content":"+.g.example"}"#,
        ))
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets/import"),
            &cookie,
            r#"{"names":["gads"],"policy":"REJECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    // 客户端按规则顺序匹配:RULE-SET 必须在兜底 MATCH 之前,否则永远不会命中。
    let body = get_text(&app, &sub_path).await;
    let root = mihomo_subscription::yaml::parse_limited(&body).unwrap();
    let rules = root.get("rules").unwrap().as_sequence().unwrap();
    assert_eq!(rules[0].as_str(), Some("RULE-SET,gads,REJECT"));
    assert_eq!(rules[1].as_str(), Some("MATCH,DIRECT"));
    // 库内文本同样 MATCH 收尾(编辑器钉底显示与后端一致)。
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/api/profiles/{p1}"), &cookie, ""))
        .await
        .unwrap();
    assert_eq!(
        json(resp).await["rules"]["content"],
        "RULE-SET,gads,REJECT\nMATCH,DIRECT"
    );
}

#[tokio::test]
async fn reset_token_rewrites_hosted_rule_set_links_in_cache() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
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
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
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
async fn rotation_after_invalid_edit_migrates_links_with_last_valid_rules() {
    for rotate_prefix in [false, true] {
        let temp = TempDb::new();
        let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
        let cookie = login(&app).await;
        let (p1, old_token, old_path) = create_profile(&app, &cookie, "P1").await;
        create_rs(
            &app,
            &cookie,
            &p1,
            r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
        )
        .await;
        put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;
        // 保存一份未通过校验的规则草稿:编辑入库,缓存保留上一份合法输出。
        let resp = app
            .clone()
            .oneshot(authed(
                "PUT",
                &format!("/api/profiles/{p1}/rules"),
                &cookie,
                r#"{"content":"MATCH,missing-policy"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(json(resp).await["regenerate"]["status"], "invalid");

        let (new_token, prefix) = if rotate_prefix {
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
            (
                old_token.clone(),
                json(resp).await["public_path_prefix"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        } else {
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
            assert_eq!(resp.status(), StatusCode::OK);
            let body = json(resp).await;
            assert_eq!(body["regenerate"]["status"], "applied");
            (
                body["token"].as_str().unwrap().to_string(),
                "testprefix".to_string(),
            )
        };
        let new_path = format!("/{prefix}/api/sub/{new_token}");
        assert_ne!(new_path, old_path);

        // 新订阅继续下发最后一份合法配置,其中的托管链接已迁移到新能力地址。
        let body = get_text(&app, &new_path).await;
        assert!(body.contains(&format!(
            "https://sub.example.com/{prefix}/api/sub/{new_token}/r/ads/domain.yaml"
        )));
        assert!(
            !body.contains(&format!("{old_path}/r/ads/")),
            "缓存中不再残留已撤销的托管链接"
        );
        // 规则草稿保持已存(未通过校验),不被回退改写。
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/api/profiles/{p1}"), &cookie, ""))
            .await
            .unwrap();
        assert_eq!(json(resp).await["rules"]["content"], "MATCH,missing-policy");
    }
}

#[tokio::test]
async fn rotation_without_valid_rules_clears_cached_subscription() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
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
    put_rules(&app, &cookie, &p1, "MATCH,missing-policy").await;
    // 模拟遗留的不一致数据:缓存快照的规则仍引用某规则集,定义却已不在库中(引用保护上线前
    // 的数据可能如此),草稿与快照都无法产出合法输出。
    sqlx::query("DELETE FROM profile_rule_sets WHERE id = ?")
        .bind(rs["id"].as_str().unwrap())
        .execute(&db)
        .await
        .unwrap();

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
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["regenerate"]["status"], "invalid");
    let new_token = body["token"].as_str().unwrap();

    // 缓存已清空:不再下发含已撤销托管地址的配置;公开拉取回源后校验仍失败 → 503。
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/testprefix/api/sub/{new_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn generate_rejects_rule_referencing_disabled_rule_set() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
    let cookie = login(&app).await;
    let (p1, _, _) = create_profile(&app, &cookie, "P1").await;
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;

    // 未被引用时可以停用;之后规则行再引用它,生成必须报错,而不是静默产出缺少 rule-providers 的配置。
    let rsid = rs["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example","enabled":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;
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

#[tokio::test]
async fn import_rejects_malformed_policy_without_writing() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
    let cookie = login(&app).await;
    let (p1, _, _) = create_profile(&app, &cookie, "P1").await;
    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"gads","behavior":"domain","format":"yaml","content":"+.g.example"}"#,
        ))
        .await
        .unwrap();

    for policy in ["", "  ", "DIRECT\nMATCH,REJECT", "A,B"] {
        let body = serde_json::json!({ "names": ["gads"], "policy": policy }).to_string();
        let resp = app
            .clone()
            .oneshot(authed(
                "POST",
                &format!("/api/profiles/{p1}/rule-sets/import"),
                &cookie,
                &body,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "policy {policy:?}");
    }

    // 拒绝发生在任何写入之前:③ 为空、规则文本未被追加。
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
    assert!(json(resp).await.as_array().unwrap().is_empty());
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/api/profiles/{p1}"), &cookie, ""))
        .await
        .unwrap();
    assert_eq!(json(resp).await["rules"]["content"], "");
}

#[tokio::test]
async fn referenced_rule_set_is_protected_from_rename_disable_and_delete() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
    let cookie = login(&app).await;
    let (p1, _, _) = create_profile(&app, &cookie, "P1").await;
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    let rsid = rs["id"].as_str().unwrap().to_string();
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;

    // 被引用的规则集改名/停用 → 409。
    for body in [
        r#"{"name":"ads2","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example","enabled":false}"#,
    ] {
        let resp = app
            .clone()
            .oneshot(authed(
                "PUT",
                &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
                &cookie,
                body,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{body}");
    }
    let delete = || {
        app.clone().oneshot(authed(
            "DELETE",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            "",
        ))
    };
    assert_eq!(delete().await.unwrap().status(), StatusCode::CONFLICT);

    // 逻辑规则内嵌套的引用同样受保护。
    put_rules(
        &app,
        &cookie,
        &p1,
        "AND,((RULE-SET,ads),(NETWORK,tcp)),DIRECT\nMATCH,DIRECT",
    )
    .await;
    assert_eq!(delete().await.unwrap().status(), StatusCode::CONFLICT);

    // 当前草稿不再引用、但产出缓存的快照仍引用时同样 409(草稿未通过校验,缓存保留上一份
    // 合法输出,删除会让正在下发的配置失去定义)。
    put_rules(&app, &cookie, &p1, "MATCH,missing-policy").await;
    assert_eq!(delete().await.unwrap().status(), StatusCode::CONFLICT);

    // 引用随合法草稿重生成而真正移除后,可删除。
    put_rules(&app, &cookie, &p1, "MATCH,DIRECT").await;
    assert_eq!(delete().await.unwrap().status(), StatusCode::NO_CONTENT);
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
    assert!(json(resp).await.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn create_completing_a_reference_regenerates_cache() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;

    // 规则先引用了一个尚无定义的名字:草稿不合法,缓存保留上一份(空规则)输出。
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rules"),
            &cookie,
            r#"{"content":"RULE-SET,ads,DIRECT\nMATCH,DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await["regenerate"]["status"], "invalid");

    // 补齐定义:创建即触发离线重生成,缓存内的订阅立即带出新规则与托管链接(不回源)。
    let created = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    assert_eq!(created["regenerate"]["status"], "applied");
    let body = get_text(&app, &sub_path).await;
    assert!(body.contains("RULE-SET,ads,DIRECT"));
    assert!(body.contains(&format!(
        "https://sub.example.com/testprefix/api/sub/{token1}/r/ads/domain.yaml"
    )));

    // 未被引用的创建不触发重生成:响应无 regenerate 字段。
    let extra = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"extra","behavior":"ipcidr","format":"text","content":"10.0.0.0/8"}"#,
    )
    .await;
    assert!(extra.get("regenerate").is_none());
}

#[tokio::test]
async fn update_renewing_behavior_or_format_rewrites_cached_links() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;
    let before = get_text(&app, &sub_path).await;
    assert!(before.contains("/r/ads/domain.yaml"));

    // behavior/format 是托管链接的一部分:更新后缓存离线重生成,链接换新。
    let rsid = rs["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            r#"{"name":"ads","behavior":"ipcidr","format":"text","content":"10.0.0.0/8"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "applied");

    let body = get_text(&app, &sub_path).await;
    assert!(body.contains(&format!(
        "https://sub.example.com/testprefix/api/sub/{token1}/r/ads/ipcidr.text"
    )));
    assert!(!body.contains("/r/ads/domain.yaml"), "缓存中不再残留旧链接");

    // 新托管地址直接可取到内容;旧地址 404。
    let served = get_text(
        &app,
        &format!("/testprefix/api/sub/{token1}/r/ads/ipcidr.text"),
    )
    .await;
    assert!(served.contains("10.0.0.0/8"));
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/testprefix/api/sub/{token1}/r/ads/domain.yaml"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_keeps_last_valid_cache_when_rules_stay_invalid() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, _, sub_path) = create_profile(&app, &cookie, "P1").await;
    let baseline = get_text(&app, &sub_path).await;

    // 规则引用了两个尚无定义的名字:草稿不合法,缓存保留上一份合法输出。
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rules"),
            &cookie,
            r#"{"content":"RULE-SET,ads,DIRECT\nRULE-SET,missing,DIRECT\nMATCH,DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await["regenerate"]["status"], "invalid");

    // 只补齐其中一个:重生成仍不通过,旧缓存不依赖新定义,继续下发而不是被删除。
    let created = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    assert_eq!(created["regenerate"]["status"], "invalid");
    let cached: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM generated_cache WHERE profile_id = ?")
            .bind(&p1)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(cached, 1);
    assert_eq!(get_text(&app, &sub_path).await, baseline);
}

#[tokio::test]
async fn update_of_draft_only_definition_keeps_last_valid_cache() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, _, sub_path) = create_profile(&app, &cookie, "P1").await;
    let baseline = get_text(&app, &sub_path).await;

    // 草稿引用 ads 与一个没有定义的名字:不合法,缓存仍是不含 ads 的上一份输出。
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rules"),
            &cookie,
            r#"{"content":"RULE-SET,ads,DIRECT\nRULE-SET,missing,DIRECT\nMATCH,DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await["regenerate"]["status"], "invalid");
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;

    // 改 ads 的 format:它只被草稿引用,旧缓存里没有它的托管链接,重生成失败后缓存照旧下发。
    let rsid = rs["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            r#"{"name":"ads","behavior":"domain","format":"text","content":"+.ad.example"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "invalid");
    let cached: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM generated_cache WHERE profile_id = ? AND capability_hash IS NOT NULL",
    )
    .bind(&p1)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(cached, 1);
    assert_eq!(get_text(&app, &sub_path).await, baseline);
}

#[tokio::test]
async fn update_of_injected_definition_drops_cache_when_rules_are_invalid() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
    )
    .await;
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;
    assert!(get_text(&app, &sub_path)
        .await
        .contains("/r/ads/domain.yaml"));

    // 当前规则草稿不合法:缓存仍是含 ads 托管链接的上一份输出。
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rules"),
            &cookie,
            r#"{"content":"RULE-SET,ads,DIRECT\nRULE-SET,missing,DIRECT\nMATCH,DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await["regenerate"]["status"], "invalid");

    // 改 format 使旧缓存里的托管链接失效,而当前规则又无法重建:缓存被删除,不再下发旧输出。
    let rsid = rs["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            r#"{"name":"ads","behavior":"domain","format":"text","content":"+.ad.example"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["regenerate"]["status"], "invalid");
    let cached: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM generated_cache WHERE profile_id = ?")
            .bind(&p1)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(cached, 0);

    // 无缓存时公开拉取回源,当前规则仍不合法 → 503;旧托管链接随定义变更 404。
    let resp = app
        .clone()
        .oneshot(Request::get(&sub_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/testprefix/api/sub/{token1}/r/ads/domain.yaml"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn manual_content_update_keeps_cache_and_serves_new_content() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, token1, sub_path) = create_profile(&app, &cookie, "P1").await;
    let rs = create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.a.example"}"#,
    )
    .await;
    put_rules(&app, &cookie, &p1, "RULE-SET,ads,DIRECT\nMATCH,DIRECT").await;
    let baseline = get_text(&app, &sub_path).await;
    let hash_before: Option<String> =
        sqlx::query_scalar("SELECT capability_hash FROM generated_cache WHERE profile_id = ?")
            .bind(&p1)
            .fetch_one(&db)
            .await
            .unwrap();

    // 纯内容更新不影响订阅输出:不重生成、不把缓存标脏。
    let rsid = rs["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(authed(
            "PUT",
            &format!("/api/profiles/{p1}/rule-sets/{rsid}"),
            &cookie,
            r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.b.example"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let updated = json(resp).await;
    assert!(
        updated.get("regenerate").is_none(),
        "纯内容更新不做无谓重生成"
    );

    assert_eq!(get_text(&app, &sub_path).await, baseline, "订阅输出不变");
    let hash_after: Option<String> =
        sqlx::query_scalar("SELECT capability_hash FROM generated_cache WHERE profile_id = ?")
            .bind(&p1)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(hash_before.is_some());
    assert_eq!(hash_after, hash_before, "缓存上下文未被标脏");

    // 托管内容按库内最新定义即时下发。
    let served = get_text(
        &app,
        &format!("/testprefix/api/sub/{token1}/r/ads/domain.yaml"),
    )
    .await;
    assert!(served.contains("+.b.example"));
}

#[tokio::test]
async fn legacy_cache_rotation_does_not_serve_revoked_token_links() {
    let temp = TempDb::new();
    let (app, db) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
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

    // 模拟升级前生成的缓存:无规则快照、无来源/能力上下文,无法离线迁移其中的托管链接。
    sqlx::query(
        "UPDATE generated_cache SET rules_snapshot = NULL, source_revision = NULL, \
         capability_hash = NULL WHERE profile_id = ?",
    )
    .bind(&p1)
    .execute(&db)
    .await
    .unwrap();

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
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    // 无法离线迁移的缓存被清空(下次公开拉取回源重建),而不是继续下发带旧 token 链接的输出。
    assert_eq!(body["regenerate"]["status"], "pending");
    let new_token = body["token"].as_str().unwrap().to_string();

    let served = get_text(&app, &format!("/testprefix/api/sub/{new_token}")).await;
    assert!(served.contains(&format!(
        "https://sub.example.com/testprefix/api/sub/{new_token}/r/ads/domain.yaml"
    )));
    assert!(
        !served.contains(&old_token),
        "下发的配置中不残留旧 token 的托管链接"
    );
    // 旧 token 一律 404(订阅与托管端点)。
    for path in [
        format!("/testprefix/api/sub/{old_token}"),
        format!("/testprefix/api/sub/{old_token}/r/ads/domain.yaml"),
    ] {
        let resp = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn import_skips_names_missing_from_the_global_library() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
    let cookie = login(&app).await;
    let (p1, _, _) = create_profile(&app, &cookie, "P1").await;
    // ② 里只有 real;ghost 不存在。
    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"real","behavior":"domain","format":"yaml","content":"+.r.example"}"#,
        ))
        .await
        .unwrap();

    let import = |body: String| {
        app.clone().oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets/import"),
            &cookie,
            &body,
        ))
    };
    let rules_content = || async {
        let resp = app
            .clone()
            .oneshot(authed("GET", &format!("/api/profiles/{p1}"), &cookie, ""))
            .await
            .unwrap();
        json(resp).await["rules"]["content"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // 混合导入:幽灵名与重复名不产生定义,也不追加规则行。
    let resp = import(r#"{"names":["ghost","real","real"],"policy":"DIRECT"}"#.to_string())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["imported"], 1);
    assert_eq!(body["regenerate"]["status"], "applied");
    let content = rules_content().await;
    assert!(content.contains("RULE-SET,real,DIRECT"));
    assert!(!content.contains("ghost"));
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
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "real");

    // 纯幽灵名导入:不复制、不改规则、不重生成。
    let resp = import(r#"{"names":["ghost"],"policy":"DIRECT"}"#.to_string())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["imported"], 0);
    assert!(body["regenerate"].is_null());
    assert_eq!(body["rules_content"].as_str().unwrap(), content);
    assert_eq!(rules_content().await, content);

    // 已存在且已被引用的定义再次导入:不重复复制、不重复追加规则行(策略保持 DIRECT)。
    let resp = import(r#"{"names":["real"],"policy":"REJECT"}"#.to_string())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json(resp).await;
    assert_eq!(body["imported"], 0);
    assert!(body["regenerate"].is_null());
    assert_eq!(body["rules_content"].as_str().unwrap(), content);
    assert_eq!(rules_content().await, content);
}

#[tokio::test]
async fn import_does_not_reference_a_disabled_local_definition() {
    let temp = TempDb::new();
    let (app, _) = cached_app(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let cookie = login(&app).await;
    let (p1, _, sub_path) = create_profile(&app, &cookie, "P1").await;
    let baseline = get_text(&app, &sub_path).await;
    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.ad.example"}"#,
        ))
        .await
        .unwrap();
    // 本订阅已有同名定义,但处于停用:生成不认它,导入不能再为它追加引用。
    create_rs(
        &app,
        &cookie,
        &p1,
        r#"{"name":"ads","behavior":"domain","format":"yaml","content":"+.own.example","enabled":false}"#,
    )
    .await;

    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets/import"),
            &cookie,
            r#"{"names":["ads"],"policy":"DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = json(resp).await;
    assert_eq!(result["imported"], 0);
    assert!(result["regenerate"].is_null());
    assert!(!result["rules_content"]
        .as_str()
        .unwrap()
        .contains("RULE-SET,ads"));
    assert_eq!(get_text(&app, &sub_path).await, baseline);
}

#[tokio::test]
async fn import_skips_unsupported_legacy_definitions() {
    let temp = TempDb::new();
    let state = test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (profile_id, _, _) = create_profile(&app, &cookie, "P1").await;
    for (id, behavior, format, url) in [
        ("legacy-mrs", "classical", "mrs", "https://up.example/rules"),
        ("legacy-url", "domain", "text", "https://"),
    ] {
        sqlx::query(
            "INSERT INTO rule_sets (id, name, behavior, format, source, url, interval_hours, cache, enabled, position, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'remote', ?, 24, 0, 1, 0, 'now', 'now')",
        )
        .bind(id)
        .bind(id)
        .bind(behavior)
        .bind(format)
        .bind(url)
        .execute(&state.db)
        .await
        .unwrap();
    }

    let resp = app
        .clone()
        .oneshot(authed(
            "POST",
            &format!("/api/profiles/{profile_id}/rule-sets/import"),
            &cookie,
            r#"{"names":["legacy-mrs","legacy-url"],"policy":"DIRECT"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let result = json(resp).await;
    assert_eq!(result["imported"], 0);
    assert!(result["regenerate"].is_null());

    let resp = app
        .clone()
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{profile_id}/rule-sets"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert!(json(resp).await.as_array().unwrap().is_empty());
    let resp = app
        .oneshot(authed(
            "GET",
            &format!("/api/profiles/{profile_id}"),
            &cookie,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(json(resp).await["rules"]["content"], "");
}

#[tokio::test]
async fn legacy_invalid_rule_providers_are_rejected() {
    let temp = TempDb::new();
    let state = test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await;
    let app = build_router(state.clone());
    let cookie = login(&app).await;
    let (profile_id, _, _) = create_profile(&app, &cookie, "P1").await;
    for (id, name, behavior, format, source, url, cache) in [
        (
            "bad-url",
            "bad-url",
            "domain",
            "text",
            "remote",
            Some("https://"),
            false,
        ),
        (
            "bad-mrs",
            "bad-mrs",
            "classical",
            "mrs",
            "remote",
            Some("https://up.example/rules"),
            false,
        ),
        ("dot", ".", "domain", "yaml", "manual", None, true),
    ] {
        sqlx::query(
            "INSERT INTO profile_rule_sets (id, profile_id, name, behavior, format, source, content, url, interval_hours, cache, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, '', ?, 24, ?, 1, 'now', 'now')",
        )
        .bind(id)
        .bind(&profile_id)
        .bind(name)
        .bind(behavior)
        .bind(format)
        .bind(source)
        .bind(url)
        .bind(cache)
        .execute(&state.db)
        .await
        .unwrap();
    }

    for (name, expected) in [
        ("bad-url", "invalid upstream URL"),
        ("bad-mrs", "unsupported classical MRS"),
        (".", "invalid name"),
    ] {
        let body = serde_json::json!({ "content": format!("RULE-SET,{name},DIRECT") }).to_string();
        let resp = app
            .clone()
            .oneshot(authed(
                "PUT",
                &format!("/api/profiles/{profile_id}/rules"),
                &cookie,
                &body,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let result = json(resp).await;
        assert_eq!(result["regenerate"]["status"], "invalid");
        assert!(result["regenerate"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error.as_str().unwrap().contains(expected)));
    }
}

#[tokio::test]
async fn import_validates_names_before_writing() {
    let temp = TempDb::new();
    let app =
        build_router(test_state_with_fetcher(&temp, Arc::new(BodyFetcher(PROVIDER_YAML))).await);
    let cookie = login(&app).await;
    let (p1, _, _) = create_profile(&app, &cookie, "P1").await;
    app.clone()
        .oneshot(authed(
            "POST",
            "/api/rule-sets",
            &cookie,
            r#"{"name":"ok","behavior":"domain","format":"yaml","content":"+.ok.example"}"#,
        ))
        .await
        .unwrap();

    let import = |body: String| {
        app.clone().oneshot(authed(
            "POST",
            &format!("/api/profiles/{p1}/rule-sets/import"),
            &cookie,
            &body,
        ))
    };
    // 任一名字非法 → 400,整批不写入。
    let resp = import(r#"{"names":["ok","a/b"],"policy":"DIRECT"}"#.to_string())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // 名字数量超限 → 400。
    let many: Vec<String> = (0..=1000).map(|i| format!("n{i}")).collect();
    let body = serde_json::json!({ "names": many, "policy": "DIRECT" }).to_string();
    let resp = import(body).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 两次拒绝都发生在任何写入之前:③ 为空、规则文本未变。
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
    assert!(json(resp).await.as_array().unwrap().is_empty());
    let resp = app
        .clone()
        .oneshot(authed("GET", &format!("/api/profiles/{p1}"), &cookie, ""))
        .await
        .unwrap();
    assert_eq!(json(resp).await["rules"]["content"], "");

    // 合法导入仍正常工作。
    let resp = import(r#"{"names":["ok"],"policy":"DIRECT"}"#.to_string())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await["imported"], 1);
}
