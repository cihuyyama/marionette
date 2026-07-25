use marionette::api;
use marionette::config::Config;
use marionette::db;
use marionette::state::AppState;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use axum::extract::DefaultBodyLimit;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,marionette=debug")),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env();
    tracing::info!(db = %config.db_path.display(), "connecting database");
    let pool = db::connect(&config.db_path).await?;
    let listen = config.listen_addr();
    let static_dir = config.static_dir.clone();
    let cors_origin = config.cors_origin.clone();
    let state = AppState::new(pool, config);
    marionette::workers::refresh::spawn(state.clone());

    let cors = if cors_origin == "*" {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        use axum::http::{HeaderValue, Method};
        let mut allowed_origins = Vec::new();
        for origin in cors_origin.split(',') {
            let o = origin.trim();
            if !o.is_empty() {
                if let Ok(val) = o.parse::<HeaderValue>() {
                    allowed_origins.push(val);
                }
            }
        }
        let mut extra_origins = Vec::new();
        for val in &allowed_origins {
            if let Ok(s) = val.to_str() {
                if s.contains("localhost") {
                    let alt = s.replace("localhost", "127.0.0.1");
                    if let Ok(alt_val) = alt.parse::<HeaderValue>() {
                        extra_origins.push(alt_val);
                    }
                } else if s.contains("127.0.0.1") {
                    let alt = s.replace("127.0.0.1", "localhost");
                    if let Ok(alt_val) = alt.parse::<HeaderValue>() {
                        extra_origins.push(alt_val);
                    }
                }
            }
        }
        allowed_origins.extend(extra_origins);

        CorsLayer::new()
            .allow_origin(allowed_origins)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers(Any)
    };

    let mut app = api::router(state)
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    if let Some(dir) = static_dir {
        if dir.exists() {
            tracing::info!(path = %dir.display(), "serving static dashboard");
            let index = dir.join("index.html");
            let serve = ServeDir::new(&dir)
                .not_found_service(ServeFile::new(index));
            app = app.fallback_service(serve);
        }
    }

    let addr: SocketAddr = listen.parse()?;
    tracing::info!(%addr, "marionette listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
