pub mod admin;
pub mod chat;
pub mod health;
pub mod images;
pub mod models;
pub mod proxies;

use crate::state::AppState;
use axum::{
    Router,
    routing::{get, patch, post},
};

pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health::health))
        .route("/v1/models", get(models::list_models))
        .route("/v1/chat/completions", post(chat::chat_completions))
        .route(
            "/v1/images/generations",
            post(images::images_generations),
        )
        .route("/images/generations", post(images::images_generations))
        .route("/v1/images/edits", post(images::images_edits))
        .route("/images/edits", post(images::images_edits));

    let admin = Router::new()
        .route("/admin/stats", get(admin::stats))
        .route("/admin/connection", get(admin::connection))
        .route("/admin/models", get(models::list_models_admin))
        .route("/admin/usage", get(admin::usage))
        .route("/admin/requests", get(admin::list_requests))
        .route("/admin/requests/{id}", get(admin::get_request))
        .route("/admin/providers", get(admin::list_provider_settings))
        .route(
            "/admin/providers/{provider}",
            patch(admin::patch_provider_settings),
        )
        .route("/admin/accounts", get(admin::list_accounts).post(admin::import_accounts))
        .route(
            "/admin/accounts/{id}",
            get(admin::get_account)
                .patch(admin::patch_account)
                .delete(admin::delete_account),
        )
        .route("/admin/accounts/{id}/refresh", post(admin::refresh_account))
        .route("/admin/accounts/{id}/grok-billing", get(admin::grok_billing))
        .route("/admin/accounts/refresh-all", post(admin::refresh_all))
        .route("/admin/refresh", get(admin::refresh_status))
        .route("/admin/refresh/jobs/{id}", get(admin::refresh_get_job))
        .route("/admin/refresh/cancel", post(admin::refresh_cancel))
        .route(
            "/admin/proxies",
            get(proxies::list_proxies).post(proxies::import_proxies),
        )
        .route("/admin/proxies/check", post(proxies::check_all))
        .route("/admin/proxies/assign", post(proxies::assign))
        .route(
            "/admin/proxies/settings",
            get(proxies::get_settings).patch(proxies::update_settings),
        )
        .route(
            "/admin/proxies/{id}",
            patch(proxies::toggle_proxy).delete(proxies::delete_proxy),
        )
        .route("/admin/proxies/{id}/check", post(proxies::check_one))
        .route(
            "/admin/accounts/{id}/proxy",
            axum::routing::delete(proxies::clear_assignments),
        )
        .route("/admin/accounts/{id}/inject", post(admin::inject_account))
        .route(
            "/admin/accounts/{id}/claim-trial",
            post(admin::claim_trial_account),
        )
        .route(
            "/admin/providers/qoder/warmup",
            post(admin::warmup_qoder_accounts),
        )
        .route(
            "/admin/providers/qoder/inject",
            post(admin::inject_bulk),
        )
        .route("/admin/inject/jobs/{id}", get(admin::inject_get_job))
        .route(
            "/admin/inject/jobs/{id}/events",
            get(admin::inject_events),
        )
        .route(
            "/admin/inject/jobs/{id}/cancel",
            post(admin::inject_cancel),
        )
        .route(
            "/admin/inject/jobs/{id}/refresh",
            post(admin::inject_finish_refresh),
        )
        .route("/admin/farm", get(admin::farm_status))
        .route("/admin/farm/start", post(admin::farm_start))
        .route("/admin/farm/jobs/{id}", get(admin::farm_get_job))
        .route("/admin/farm/jobs/{id}/events", get(admin::farm_events))
        .route("/admin/farm/jobs/{id}/cancel", post(admin::farm_cancel))
        .route("/admin/farm/jobs/{id}/import", post(admin::farm_import))
        .route(
            "/admin/farm/jobs/{id}/retry-failed",
            post(admin::farm_retry_failed),
        );

    Router::new()
        .merge(public)
        .merge(admin)
        .with_state(state)
}
