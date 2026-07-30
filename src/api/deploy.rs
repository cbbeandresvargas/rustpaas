use std::sync::Arc;

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::config::Config;
use crate::db::models::{Bucket, Project, ProjectStatus};
use crate::runner::{self, ProcessManager};
use crate::s3;

// ─────────────────────────────────────────────
// Shared application state (injected by Axum)
// ─────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub db: SqlitePool,
    pub process_manager: Arc<Mutex<ProcessManager>>,
}

// ─────────────────────────────────────────────
// Response types
// ─────────────────────────────────────────────

#[derive(Serialize)]
struct ApiError {
    error: String,
}

#[derive(Serialize)]
struct DeployResponse {
    project_id: String,
    name: String,
    port: u16,
    pid: u32,
    message: String,
}

#[derive(Deserialize)]
pub struct DeployQuery {
    pub name: Option<String>,
    pub subdomain: Option<String>,
}

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

fn api_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ApiError { error: msg.into() })).into_response()
}

/// Find an available port in the configured range
async fn allocate_port(config: &Config, db: &SqlitePool) -> anyhow::Result<u16> {
    // Get all ports currently used by projects
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT port FROM projects")
        .fetch_all(db)
        .await?;
    let used: std::collections::HashSet<u16> = rows.iter().map(|(p,)| *p as u16).collect();

    for port in config.app_port_start..=config.app_port_end {
        if !used.contains(&port) {
            return Ok(port);
        }
    }
    anyhow::bail!("No available ports in range {}-{}", config.app_port_start, config.app_port_end)
}

// ─────────────────────────────────────────────
// Deploy endpoint
// ─────────────────────────────────────────────

/// POST /api/deploy
///
/// Accepts a multipart form with:
/// - `binary`: the compiled executable
/// - `name`: project name (also becomes the subdomain)
pub async fn deploy(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Response {
    let mut binary_bytes: Option<bytes::Bytes> = None;
    let mut project_name: Option<String> = None;

    // Parse multipart fields
    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("binary") => {
                match field.bytes().await {
                    Ok(b) => binary_bytes = Some(b),
                    Err(e) => return api_err(StatusCode::BAD_REQUEST, format!("Failed to read binary: {}", e)),
                }
            }
            Some("name") => {
                match field.text().await {
                    Ok(n) => project_name = Some(n.trim().to_lowercase().replace(' ', "-")),
                    Err(e) => return api_err(StatusCode::BAD_REQUEST, format!("Failed to read name: {}", e)),
                }
            }
            _ => {}
        }
    }

    let binary = match binary_bytes {
        Some(b) => b,
        None => return api_err(StatusCode::BAD_REQUEST, "Missing 'binary' field"),
    };

    let name = match project_name {
        Some(n) if !n.is_empty() => n,
        _ => return api_err(StatusCode::BAD_REQUEST, "Missing or empty 'name' field"),
    };

    // Allocate a port
    let port = match allocate_port(&state.config, &state.db).await {
        Ok(p) => p,
        Err(e) => return api_err(StatusCode::SERVICE_UNAVAILABLE, format!("No ports available: {}", e)),
    };

    // Create or find a dummy admin user (Phase 1: single-user mode)
    let user_id = ensure_admin_user(&state.db).await.unwrap_or_else(|_| "admin".to_string());

    // Create the project record
    let project = Project::new(&name, &name, port, &user_id);
    if let Err(e) = project.insert(&state.db).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e));
    }

    // Save binary to disk
    let bin_dir = state.config.project_bin_dir(&name);
    if let Err(e) = std::fs::create_dir_all(&bin_dir) {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create bin dir: {}", e));
    }

    #[cfg(windows)]
    let bin_path = bin_dir.join(format!("{}.exe", name));
    #[cfg(not(windows))]
    let bin_path = bin_dir.join(&name);

    if let Err(e) = std::fs::write(&bin_path, &binary) {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write binary: {}", e));
    }

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
    }

    // Create S3 bucket for this project
    let bucket_name = format!("bucket-{}", name);
    if let Err(e) = s3::create_bucket(&state.config, &bucket_name) {
        error!("Failed to create bucket: {}", e);
    } else {
        let bucket = Bucket::new(&bucket_name, &project.id);
        let _ = bucket.insert(&state.db).await;
    }

    // Update status to Deploying
    let _ = Project::update_status(&state.db, &project.id, ProjectStatus::Deploying, None).await;

    // Spawn the process
    match runner::spawn_process(&state.config, &project, &state.process_manager).await {
        Ok(pid) => {
            let _ = Project::update_status(
                &state.db,
                &project.id,
                ProjectStatus::Running,
                Some(pid as i64),
            ).await;

            info!("✅ Deployed project '{}' on port {}", name, port);

            (
                StatusCode::CREATED,
                Json(DeployResponse {
                    project_id: project.id,
                    name,
                    port,
                    pid,
                    message: "Deployed successfully".to_string(),
                }),
            ).into_response()
        }
        Err(e) => {
            let _ = Project::update_status(&state.db, &project.id, ProjectStatus::Error, None).await;
            api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to start process: {}", e))
        }
    }
}

// ─────────────────────────────────────────────
// CRUD endpoints
// ─────────────────────────────────────────────

/// GET /api/projects — List all projects
pub async fn list_projects(State(state): State<AppState>) -> Response {
    match Project::find_all(&state.db).await {
        Ok(projects) => Json(projects).into_response(),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// DELETE /api/projects/:id — Delete a project and stop its process
pub async fn delete_project(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    // Kill the process if running
    let _ = runner::kill_process(&id, &state.process_manager).await;

    // Delete from DB
    match Project::delete(&state.db, &id).await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({"deleted": id}))).into_response(),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /api/projects/:id/restart
pub async fn restart_project(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    // Kill existing process
    let _ = runner::kill_process(&id, &state.process_manager).await;

    // Respawn
    match runner::spawn_process(&state.config, &project, &state.process_manager).await {
        Ok(pid) => {
            let _ = Project::update_status(&state.db, &id, ProjectStatus::Running, Some(pid as i64)).await;
            (StatusCode::OK, Json(serde_json::json!({"restarted": true, "pid": pid}))).into_response()
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Restart failed: {}", e)),
    }
}

/// POST /api/projects/:id/stop
pub async fn stop_project(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(e) = runner::kill_process(&id, &state.process_manager).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let _ = Project::update_status(&state.db, &id, ProjectStatus::Stopped, None).await;
    (StatusCode::OK, Json(serde_json::json!({"stopped": true}))).into_response()
}

/// GET /api/projects/:id/backup — Download a SQLite backup of the project's DB
pub async fn download_backup(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let db_path = state.config.project_data_dir(&project.name).join("app.db");
    if !db_path.exists() {
        return api_err(StatusCode::NOT_FOUND, "No database found for this project");
    }

    // Create a safe backup using SQLite's VACUUM INTO
    let backup_path = std::env::temp_dir().join(format!("{}-backup.db", project.name));

    // Use rusqlite for the safe backup
    let backup_path_str = backup_path.to_string_lossy().to_string();
    let db_path_str = db_path.to_string_lossy().to_string();

    let result = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(&db_path_str)?;
        conn.execute_batch(&format!("VACUUM INTO '{}'", backup_path_str.replace('\'', "''")))
            .map_err(|e| anyhow::anyhow!("Backup failed: {}", e))
    })
    .await;

    match result {
        Ok(Ok(_)) => {
            match tokio::fs::read(&backup_path).await {
                Ok(bytes) => {
                    let filename = format!("{}-backup.db", project.name);
                    (
                        StatusCode::OK,
                        [
                            ("Content-Type", "application/octet-stream"),
                            ("Content-Disposition", &format!("attachment; filename=\"{}\"", filename)),
                        ],
                        bytes,
                    ).into_response()
                }
                Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to read backup: {}", e)),
            }
        }
        Ok(Err(e)) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Backup task failed: {}", e)),
    }
}

// ─────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────

pub async fn ensure_admin_user(db: &SqlitePool) -> anyhow::Result<String> {
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM users WHERE username = 'admin' LIMIT 1")
        .fetch_optional(db)
        .await?;

    let admin123_hash = bcrypt::hash("admin123", bcrypt::DEFAULT_COST).unwrap_or_default();

    if let Some((id,)) = existing {
        // Update the password in case it was created in an older phase with placeholder
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(&admin123_hash)
            .bind(&id)
            .execute(db)
            .await?;
        return Ok(id);
    }

    let user = crate::db::models::User::new("admin", "admin123");
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&user.id)
    .bind(&user.username)
    .bind(&user.password_hash)
    .bind(user.created_at)
    .execute(db)
    .await?;

    tracing::info!("⚠️ Created default admin user: admin / admin123");

    Ok(user.id)
}
