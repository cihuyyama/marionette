pub mod admin;
pub mod chat;
pub mod health;
pub mod models;

use crate::state::AppState;
use axum::{
    Router,
    routing::{get, post},
};

pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health::health))
        .route("/v1/models", get(models::list_models))
        .route("/v1/chat/completions", post(chat::chat_completions));

    let admin = Router::new()
        .route("/admin/stats", get(admin::stats))
        .route("/admin/accounts", get(admin::list_accounts).post(admin::import_accounts))
        .route(
            "/admin/accounts/{id}",
            get(admin::get_account)
                .patch(admin::patch_account)
                .delete(admin::delete_account),
        )
        .route("/admin/accounts/{id}/refresh", post(admin::refresh_account));

    Router::new()
        .merge(public)
        .merge(admin)
        .with_state(state)
}
