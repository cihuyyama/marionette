use crate::auth::{AdminAuth, PoolAuth};
use crate::db;
use crate::error::AppResult;
use crate::openai::default_models;
use crate::state::AppState;
use axum::{Json, extract::State};
use serde_json::{Value, json};

async fn models_payload(state: &AppState) -> AppResult<Value> {
    let mut data = serde_json::to_value(default_models().data)?
        .as_array()
        .cloned()
        .unwrap_or_default();
    for combo in db::list_model_combos(&state.pool).await? {
        if !combo.is_active {
            continue;
        }
        let targets: Vec<&str> = combo.targets.iter().map(|t| t.model.as_str()).collect();
        data.push(json!({
            "id": combo.id,
            "object": "model",
            "owned_by": "combo",
            "display_name": combo.name,
            "reasoning": false,
            "vision": false,
            "is_default": false,
            "targets": targets,
        }));
    }
    Ok(json!({ "object": "list", "data": data }))
}

pub async fn list_models(
    State(state): State<AppState>,
    _auth: PoolAuth,
) -> AppResult<Json<Value>> {
    Ok(Json(models_payload(&state).await?))
}

pub async fn list_models_admin(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    Ok(Json(models_payload(&state).await?))
}
