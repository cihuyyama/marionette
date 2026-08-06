use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "marionette",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Resource usage of the marionette process itself plus any running
/// automation subprocesses (farm python + browsers). No auth: contains
/// no secrets, only counters.
pub async fn usage(State(state): State<AppState>) -> Json<Value> {
    let snap = match state.usage.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => None,
    };
    match snap {
        Some(s) => Json(serde_json::to_value(s).unwrap_or(json!({"status":"pending"}))),
        None => Json(json!({"status": "pending"})),
    }
}
