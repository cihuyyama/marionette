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

pub struct PoolAuth {
    /// `None` for the env master key (MARIONETTE_API_KEY), `Some(id)` for a
    /// database-backed key from `api_keys`.
    pub key_id: Option<String>,
}
pub struct AdminAuth;

impl FromRequestParts<AppState> for PoolAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_bearer(parts).ok_or(AppError::Unauthorized)?;
        if token == state.config.api_key {
            return Ok(PoolAuth { key_id: None });
        }
        let hash = hash_key(&token);
        let key_id = crate::db::get_api_key_id_by_hash(&state.pool, &hash)
            .await
            .ok()
            .flatten();
        match key_id {
            Some(id) => Ok(PoolAuth { key_id: Some(id) }),
            None => Err(AppError::Unauthorized),
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
