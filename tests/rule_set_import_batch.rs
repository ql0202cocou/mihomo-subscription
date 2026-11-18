// SQLx emits query events on SQLite worker threads, so this test uses its own test binary.
mod common;

use std::{
    fs,
    sync::{Arc, Mutex},
};

use common::{cached_app, login, rule_api, BodyFetcher, TempDb};
use serde_json::json;

#[tokio::test]
async fn batch_links_read_the_draft_and_legacy_cache_once() {
    let db_file = TempDb::new();
    let (app, db) = cached_app(&db_file, Arc::new(BodyFetcher("proxies: []\nrules: []\n"))).await;
    let cookie = login(&app).await;
    let profile = rule_api(
        &app,
        &cookie,
        "POST",
        "/api/profiles",
        r#"{"name":"batch","source_url":"https://provider.example/sub"}"#,
    )
    .await;
    let id = profile["id"].as_str().unwrap();
    let base = format!("/api/profiles/{id}/rule-sets");
    let names: Vec<String> = (0..12).map(|n| format!("ads-{n}")).collect();
    let mut links = serde_json::Map::new();
    for name in &names {
        let mut body =
            json!({"name":name, "behavior":"domain","format":"text","content":"old.example"});
        let row = rule_api(&app, &cookie, "POST", &base, &body.to_string()).await;
        links.insert(name.clone(), row["id"].clone());
        body["format"] = json!("yaml");
        rule_api(&app, &cookie, "POST", "/api/rule-sets", &body.to_string()).await;
    }
    let draft = "RULE-SET,ads-0,REJECT\nMATCH,DIRECT";
    rule_api(
        &app,
        &cookie,
        "PUT",
        &format!("/api/profiles/{id}/rules"),
        &json!({"content":draft}).to_string(),
    )
    .await;
    sqlx::query(
        "UPDATE generated_cache SET rules_snapshot=NULL, provider_yaml=NULL WHERE profile_id=?",
    )
    .bind(id)
    .execute(&db)
    .await
    .unwrap();

    let log = TempDb::new();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_env_filter("sqlx::query=debug")
            .without_time()
            .with_ansi(false)
            .with_writer(Mutex::new(fs::File::create(&log.path).unwrap()))
            .finish(),
    )
    .unwrap();

    let result = rule_api(
        &app,
        &cookie,
        "POST",
        &format!("{base}/import"),
        &json!({"names":names,"policy":"DIRECT","link_existing":links}).to_string(),
    )
    .await;
    assert_eq!(result["imported"], names.len());
    let rules = result["rules_content"].as_str().unwrap();
    assert!(rules.starts_with("RULE-SET,ads-0,REJECT\n"));
    assert_eq!(rules.lines().count(), names.len() + 1);
    let queries = fs::read_to_string(&log.path).unwrap();
    assert_eq!(
        queries
            .matches("SELECT content FROM rulesets WHERE profile_id")
            .count(),
        1
    );
    assert_eq!(
        queries
            .matches("CASE WHEN rules_snapshot IS NULL THEN output_yaml END")
            .count(),
        1
    );
    let cache_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM generated_cache WHERE profile_id=?")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(cache_count, 0);
}
