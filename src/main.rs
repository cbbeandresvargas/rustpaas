use anyhow::Result;
use tracing::info;

mod config;
mod db;
mod s3;
mod runner;
mod dashboard;
mod api;

// Required by askama for filter resolution
pub mod filters {}

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    let _ = dotenvy::dotenv();

    // Initialize tracing/logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mypaas=debug,tower_http=debug".into()),
        )
        .init();

    info!("🚀 Starting RustPaaS...");

    // Load configuration
    let config = Config::from_env()?;
    info!("📁 Data directory: {}", config.data_dir.display());

    // Ensure data directories exist
    config.ensure_dirs()?;

    // Initialize SQLite database
    let db_pool = db::init_pool(&config).await?;
    info!("🗃️  SQLite initialized at {}", config.db_path().display());

    // Initialize S3 engine
    let s3_handle = s3::start_server(&config).await?;
    info!("🪣  S3 engine listening on port {}", config.s3_port);

    // Build shared application state
    let state = api::AppState {
        config: config.clone(),
        db: db_pool,
        process_manager: runner::ProcessManager::new(),
    };

    // Build Axum router
    let app = router::build(state);

    // Start main HTTP server
    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("🌐 Dashboard & API listening on http://{}", addr);

    axum::serve(listener, app).await?;

    // Shutdown S3
    drop(s3_handle);

    Ok(())
}

mod router {
    use axum::{Router, routing::get};
    use tower_http::cors::CorsLayer;
    use tower_http::trace::TraceLayer;
    use tower_http::services::ServeDir;

    use crate::api::AppState;
    use crate::dashboard::handlers as dash;
    use crate::api::deploy;

    pub fn build(state: AppState) -> Router {
        // Dashboard routes
        let dashboard_routes = Router::new()
            .route("/", get(dash::index))
            .route("/projects/{id}", get(dash::project_detail))
            .route("/projects/{id}/logs", get(dash::project_logs));

        // REST API routes
        let api_routes = Router::new()
            .route("/projects", get(deploy::list_projects))
            .route("/projects/{id}", axum::routing::delete(deploy::delete_project))
            .route("/projects/{id}/restart", axum::routing::post(deploy::restart_project))
            .route("/projects/{id}/stop", axum::routing::post(deploy::stop_project))
            .route("/projects/{id}/backup", get(deploy::download_backup))
            .route("/deploy", axum::routing::post(deploy::deploy));

        Router::new()
            .merge(dashboard_routes)
            .nest("/api", api_routes)
            .nest_service("/static", ServeDir::new("static"))
            .layer(TraceLayer::new_for_http())
            .layer(CorsLayer::permissive())
            .with_state(state)
    }
}
