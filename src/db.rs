//! 数据库初始化:连接池、迁移,以及 app-settings 的种子。
//!
//! 按 `docs/architecture.md`「数据模型」,`foreign_keys` 与 `busy_timeout` 是每连接 pragma。它们配置在
//! `SqliteConnectOptions` 上,SQLx 会对它打开的 *每个* 物理连接下发——等价于 after-connect 钩子
//! 的惯用做法——故 `ON DELETE CASCADE` 不会在池中某些连接上被静默禁用。`journal_mode = WAL`
//! 只设一次,随数据库文件持久化。

use std::{error::Error, time::Duration};

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::util::random_path_prefix;

type DbResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// 为数据库文件路径构建规范的每连接选项。
pub fn connect_options(path: &str) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true)
}

/// 用给定选项打开连接池并运行待执行的迁移。
pub async fn init_with(options: SqliteConnectOptions) -> DbResult<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// 为 `path` 处的数据库文件打开连接池并运行待执行的迁移。
pub async fn init(path: &str) -> DbResult<SqlitePool> {
    init_with(connect_options(path)).await
}

/// 确保单行 `app_settings` 存在并返回公共路径前缀。首次启动时:`env_seed` 存在且非空则用它做种子,
/// 否则生成随机的 URL-safe 值。之后的启动返回已持久化的值(可能在运行时被重置过)。
pub async fn ensure_public_path_prefix(
    pool: &SqlitePool,
    env_seed: Option<String>,
) -> DbResult<String> {
    if let Some((prefix,)) =
        sqlx::query_as::<_, (String,)>("SELECT public_path_prefix FROM app_settings WHERE id = 1")
            .fetch_optional(pool)
            .await?
    {
        return Ok(prefix);
    }

    let prefix = env_seed
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(random_path_prefix);
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO app_settings (id, public_path_prefix, updated_at) VALUES (1, ?, ?)")
        .bind(&prefix)
        .bind(&now)
        .execute(pool)
        .await?;
    Ok(prefix)
}
