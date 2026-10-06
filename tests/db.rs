//! Verifies that the per-connection `foreign_keys` pragma is actually applied
//! across the pool: deleting a profile must cascade to every child table. If
//! the pragma were set only once (not per connection), this would fail
//! intermittently or leave orphan rows.

mod common;

use common::TempDb;
use mihomo_subscription::db;
use sqlx::SqlitePool;

async fn seed_profile_with_children(pool: &SqlitePool, profile_id: &str) {
    let now = "2026-06-12T00:00:00Z";
    sqlx::query(
        "INSERT INTO profiles (id, name, source_url, token, created_at, updated_at)
         VALUES (?, 'p', 'https://example.com/sub?token=x', 'tok', ?, ?)",
    )
    .bind(profile_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO rulesets (id, profile_id, content, updated_at) VALUES (?, ?, 'MATCH,DIRECT', ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(profile_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO custom_groups (id, profile_id, name, group_type, members, created_at, updated_at)
         VALUES (?, ?, 'g', 'select', '[\"DIRECT\"]', ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(profile_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO generated_cache (profile_id, content_hash, output_yaml, generated_at)
         VALUES (?, 'h', 'proxies: []', ?)",
    )
    .bind(profile_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn count(pool: &SqlitePool, table: &str, profile_id: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE profile_id = ?");
    // table 只来自测试里写死的表名。
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
        .bind(profile_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn delete_profile_cascades_to_all_children() {
    let temp = TempDb::new();
    let pool = db::init(&temp.path).await.unwrap();
    let profile_id = uuid::Uuid::new_v4().to_string();

    seed_profile_with_children(&pool, &profile_id).await;

    for table in ["rulesets", "custom_groups", "generated_cache"] {
        assert_eq!(count(&pool, table, &profile_id).await, 1, "seed {table}");
    }

    sqlx::query("DELETE FROM profiles WHERE id = ?")
        .bind(&profile_id)
        .execute(&pool)
        .await
        .unwrap();

    for table in ["rulesets", "custom_groups", "generated_cache"] {
        assert_eq!(
            count(&pool, table, &profile_id).await,
            0,
            "cascade should remove rows from {table}"
        );
    }
}

#[tokio::test]
async fn foreign_keys_pragma_is_on_for_pooled_connections() {
    let temp = TempDb::new();
    let pool = db::init(&temp.path).await.unwrap();

    // Hit several pooled connections; each must report foreign_keys = 1.
    for _ in 0..10 {
        let on: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(on, 1, "foreign_keys must be ON for every pooled connection");
    }
}

#[tokio::test]
async fn ensure_public_path_prefix_is_idempotent() {
    let temp = TempDb::new();
    let pool = db::init(&temp.path).await.unwrap();

    let first = db::ensure_public_path_prefix(&pool, Some("seeded-prefix".into()))
        .await
        .unwrap();
    assert_eq!(first, "seeded-prefix");

    // A later call must return the persisted value, ignoring a new seed.
    let second = db::ensure_public_path_prefix(&pool, Some("different".into()))
        .await
        .unwrap();
    assert_eq!(second, "seeded-prefix");
}

#[tokio::test]
async fn refresh_migration_preserves_existing_profiles_and_mirror_cache() {
    let temp = TempDb::new();
    let pool = SqlitePool::connect_with(db::connect_options(&temp.path))
        .await
        .unwrap();
    let migrations = sqlx::migrate!("./migrations");
    let old = sqlx::migrate::Migrator::with_migrations(
        migrations
            .iter()
            .filter(|m| m.version <= 12)
            .cloned()
            .collect(),
    );
    old.run(&pool).await.unwrap();
    seed_profile_with_children(&pool, "profile").await;
    sqlx::query(
        "INSERT INTO profile_rule_sets (id, profile_id, name, behavior, format, source, url, \
         cached_body, cached_at, last_fetch_status, created_at, updated_at) \
         VALUES ('mirror', 'profile', 'ads', 'domain', 'text', 'remote', \
         'https://provider.example/rules', ?, '2026-10-05T00:00:00Z', 'success', 'now', 'now')",
    )
    .bind(b"+.example.org\n".as_slice())
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let upgraded = db::init(&temp.path).await.unwrap();
    let profile: (String, Option<String>) =
        sqlx::query_as("SELECT token, public_refresh_at FROM profiles WHERE id = 'profile'")
            .fetch_one(&upgraded)
            .await
            .unwrap();
    assert_eq!(profile, ("tok".into(), None));
    let mirror: (Vec<u8>, i64, Option<String>, String) = sqlx::query_as(
        "SELECT cached_body, revision, last_fetch_at, last_fetch_status FROM profile_rule_sets WHERE id = 'mirror'",
    ).fetch_one(&upgraded).await.unwrap();
    assert_eq!(
        mirror,
        (b"+.example.org\n".to_vec(), 0, None, "success".into())
    );
    assert_eq!(count(&upgraded, "generated_cache", "profile").await, 1);
    sqlx::query("DELETE FROM profiles WHERE id = 'profile'")
        .execute(&upgraded)
        .await
        .unwrap();
    assert_eq!(count(&upgraded, "profile_rule_sets", "profile").await, 0);
}
