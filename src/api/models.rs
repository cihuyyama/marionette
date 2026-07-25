use crate::auth::{AdminAuth, PoolAuth};
use crate::openai::default_models;
use axum::Json;

pub async fn list_models(_auth: PoolAuth) -> Json<crate::openai::ModelsResponse> {
    Json(default_models())
}

pub async fn list_models_admin(_auth: AdminAuth) -> Json<crate::openai::ModelsResponse> {
    Json(default_models())
}
