//! 应用设置:读取设置,以及重置全局公共路径前缀。

use std::sync::Arc;

use axum::{extract::State, response::IntoResponse, Json};
use serde::Serialize;

use crate::app::AppState;
use crate::error::ApiResult;
use crate::util::{now, random_path_prefix};

/// 返回给管理前端的设置视图(当前仅公共路径前缀)。
#[derive(Serialize)]
pub struct SettingsResponse {
    public_path_prefix: String,
}

/// 读取当前应用设置。
pub async fn get(State(state): State<Arc<AppState>>) -> ApiResult<impl IntoResponse> {
    Ok(Json(SettingsResponse {
        public_path_prefix: state.current_prefix(),
    }))
}

/// 重置全局公共路径前缀。所有 profile 的托管链接一次性改变,使全部旧链接失效
/// (见 `docs/architecture.md`);各缓存随即离线重生成,使其中的规则集托管链接改用新前缀。
pub async fn reset_public_path(State(state): State<Arc<AppState>>) -> ApiResult<impl IntoResponse> {
    let prefix = random_path_prefix();
    sqlx::query("UPDATE app_settings SET public_path_prefix = ?, updated_at = ? WHERE id = 1")
        .bind(&prefix)
        .bind(now())
        .execute(&state.db)
        .await?;
    // 公开端点校验读的是内存中的前缀,落库后必须同步,使新前缀免重启立即生效。
    state.set_prefix(prefix.clone());
    crate::generate::regenerate_all_from_cache(&state).await;
    Ok(Json(SettingsResponse {
        public_path_prefix: prefix,
    }))
}
