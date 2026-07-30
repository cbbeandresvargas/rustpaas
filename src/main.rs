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
                .unwrap_or_else(|_| "mypaas=debug,tower_http=info".into()),
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

    // Ensure the default admin user exists
    crate::api::deploy::ensure_admin_user(&db_pool).await.expect("Failed to ensure admin user exists");

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
    let app = router::build(state.clone());

    // Auto-suspend background task (Scale-to-zero)
    let pm = state.process_manager.clone();
    let db = state.db.clone();
    let timeout = config.suspend_timeout_mins * 60;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let mut to_kill = vec![];
            
            {
                let mgr = pm.lock().await;
                for pid in mgr.active_projects() {
                    if let Some(idle) = mgr.idle_seconds(&pid) {
                        if idle > timeout {
                            to_kill.push(pid);
                        }
                    }
                }
            }

            for pid in to_kill {
                tracing::info!("♻️ Auto-suspending project {} due to inactivity", pid);
                let _ = runner::kill_process(&pid, &pm).await;
                let _ = db::models::Project::update_status(
                    &db, 
                    &pid, 
                    db::models::ProjectStatus::Suspended, 
                    None
                ).await;
            }
        }
    });

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

    use crate::dashboard::auth;

    pub fn build(state: AppState) -> Router {
        // Auth routes
        let auth_routes = Router::new()
            .route("/login", get(auth::login_page).post(auth::login_post))
            .route("/logout", axum::routing::post(auth::logout_post));

        // Dashboard & management routes (all prefixed with /dashboard)
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

        // The proxy fallback handles two cases:
        //   1. Requests with a recognized app subdomain (Host: myapp.localhost)
        //      → forwarded to the app's local port
        //   2. Any other unmatched route → 404 "No subdomain found"
        //
        // Because axum evaluates routes before fallback, known dashboard paths
        // (/, /projects/*, /api/*, /static/*) always win for the main domain.
        // Subdomain traffic hits the fallback because the dashboard routes only
        // match specific paths; a subdomain request to "/" still matches the
        // dashboard route. To solve this properly we use the subdomain-aware
        // handler approach: the proxy_handler checks the Host header itself.
        Router::new()
            .route("/", get(dash::landing))
            .route("/docs", get(dash::docs))
            .merge(auth_routes)
            .nest("/dashboard", dashboard_routes)
            .nest("/api", api_routes)
            .nest_service("/static", ServeDir::new("static"))
            // Fallback for unknown paths — proxy checks Host header
            .fallback(crate::runner::proxy::proxy_handler)
            .layer(TraceLayer::new_for_http())
            .layer(CorsLayer::permissive())
            .with_state(state)
    }
}
