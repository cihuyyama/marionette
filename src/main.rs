use marionette::api;
use marionette::config::Config;
use marionette::db;
use marionette::state::AppState;
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
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

    let cors = if cors_origin == "*" {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        use axum::http::{HeaderValue, Method};
        CorsLayer::new()
            .allow_origin(cors_origin.parse::<HeaderValue>()?)
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
