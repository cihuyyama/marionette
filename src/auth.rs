use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use sha2::{Digest, Sha256};

pub fn hash_key(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}

pub fn extract_bearer(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            let s = s.trim();
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
                .map(|t| t.trim().to_string())
        })
}

pub struct PoolAuth;
pub struct AdminAuth;

impl FromRequestParts<AppState> for PoolAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts).ok_or(AppError::Unauthorized)?;
        if token == state.config.api_key {
            return Ok(PoolAuth);
        }
        // optional hashed keys table
        let hash = hash_key(&token);
        let row = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(1) FROM api_keys WHERE key_hash = ? AND is_active = 1",
        )
        .bind(&hash)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
        if row > 0 {
            Ok(PoolAuth)
        } else {
            Err(AppError::Unauthorized)
        }
    }
}

impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts).ok_or(AppError::Unauthorized)?;
        if token == state.config.admin_key {
            Ok(AdminAuth)
        } else {
            Err(AppError::Unauthorized)
        }
    }
}

pub async fn ensure_admin(state: &AppState, auth_header: Option<&str>) -> AppResult<()> {
    let token = auth_header
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
                .map(|t| t.trim())
        })
        .ok_or(AppError::Unauthorized)?;
    if token == state.config.admin_key {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}
