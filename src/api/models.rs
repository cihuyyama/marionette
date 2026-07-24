use crate::auth::PoolAuth;
use crate::openai::default_models;
use axum::Json;

pub async fn list_models(_auth: PoolAuth) -> Json<crate::openai::ModelsResponse> {
    Json(default_models())
}
