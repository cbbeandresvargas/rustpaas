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
use tracing::info;

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

/// Authenticate a request via Bearer API key or session cookie.
/// Returns Some(user_id) on success, None if unauthenticated.
async fn authenticate(
    jar: &axum_extra::extract::cookie::CookieJar,
    headers: &axum::http::HeaderMap,
    db: &SqlitePool,
) -> Option<String> {
    if let Some(auth_header) = headers.get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(api_key) = auth_str.strip_prefix("Bearer ") {
                if let Ok(Some((uid,))) = sqlx::query_as::<_, (String,)>(
                    "SELECT id FROM users WHERE api_key = ?",
                )
                .bind(api_key)
                .fetch_optional(db)
                .await
                {
                    return Some(uid);
                }
            }
        }
    }

    if let Some(cookie) = jar.get(crate::dashboard::auth::SESSION_COOKIE_NAME) {
        let query = "SELECT u.id FROM users u \
                     JOIN sessions s ON u.id = s.user_id \
                     WHERE s.id = ? AND s.expires_at > ?";
        if let Ok(Some((uid,))) = sqlx::query_as::<_, (String,)>(query)
            .bind(cookie.value())
            .bind(chrono::Utc::now())
            .fetch_optional(db)
            .await
        {
            return Some(uid);
        }
    }

    None
}

const MAX_BINARY_SIZE: usize = 100 * 1024 * 1024; // 100 MB

/// Find an available port in the configured range
async fn allocate_port(config: &Config, db: &SqlitePool) -> anyhow::Result<u16> {
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
/// - `binary`: the compiled executable (max 100 MB)
/// - `name`: project name (lowercase alphanumeric + hyphens)
///
/// If the project already exists and is owned by the same user,
/// the binary is replaced and the process is restarted (re-deploy).
pub async fn deploy(
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let user_id = match authenticate(&jar, &headers, &state.db).await {
        Some(uid) => uid,
        None => return api_err(
            StatusCode::UNAUTHORIZED,
            "Authentication required. Provide a valid session cookie or Bearer API key.",
        ),
    };

    let mut binary_bytes: Option<bytes::Bytes> = None;
    let mut project_name: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("binary") => {
                match field.bytes().await {
                    Ok(b) => {
                        if b.len() > MAX_BINARY_SIZE {
                            return api_err(StatusCode::PAYLOAD_TOO_LARGE, "Binary exceeds 100 MB limit");
                        }
                        binary_bytes = Some(b);
                    }
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

    // Validate: only lowercase letters, digits, and interior hyphens
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return api_err(
            StatusCode::BAD_REQUEST,
            "Project name must use only lowercase letters, numbers, and hyphens, \
             and cannot start or end with a hyphen",
        );
    }

    // ── Re-deploy: update existing project ───────────────────────────────────
    let existing_project: Option<Project> = sqlx::query_as::<_, Project>(
        "SELECT id, name, subdomain, port, status, pid, user_id, created_at, updated_at \
         FROM projects WHERE name = ? LIMIT 1",
    )
    .bind(&name)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some(existing) = existing_project {
        if existing.user_id != user_id {
            return api_err(StatusCode::FORBIDDEN, "A project with this name is owned by another user");
        }

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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
        }

        let _ = runner::kill_process(&existing.id, &state.process_manager).await;
        let _ = Project::update_status(&state.db, &existing.id, ProjectStatus::Deploying, None).await;

        return match runner::spawn_process(&state.config, &existing, &state.process_manager).await {
            Ok(pid) => {
                let _ = Project::update_status(
                    &state.db,
                    &existing.id,
                    ProjectStatus::Running,
                    Some(pid as i64),
                ).await;
                info!("✅ Re-deployed project '{}' on port {}", name, existing.port);
                (
                    StatusCode::OK,
                    Json(DeployResponse {
                        project_id: existing.id,
                        name,
                        port: existing.port as u16,
                        pid,
                        message: "Re-deployed successfully".to_string(),
                    }),
                ).into_response()
            }
            Err(e) => {
                let _ = Project::update_status(&state.db, &existing.id, ProjectStatus::Error, None).await;
                api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to restart process: {}", e))
            }
        };
    }

    // ── New deploy ────────────────────────────────────────────────────────────
    let port = match allocate_port(&state.config, &state.db).await {
        Ok(p) => p,
        Err(e) => return api_err(StatusCode::SERVICE_UNAVAILABLE, format!("No ports available: {}", e)),
    };

    let project = Project::new(&name, &name, port, &user_id);
    if let Err(e) = project.insert(&state.db).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e));
    }

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

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
    }

    let bucket_name = format!("bucket-{}", project.name);
    if let Err(e) = s3::create_bucket(&state.config, &project.id, &bucket_name) {
        tracing::error!("Failed to create initial S3 bucket: {}", e);
    } else {
        let bucket = Bucket::new(&bucket_name, &project.id);
        let _ = bucket.insert(&state.db).await;
    }

    let _ = Project::update_status(&state.db, &project.id, ProjectStatus::Deploying, None).await;

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
// CRUD endpoints (all require authentication)
// ─────────────────────────────────────────────

/// GET /api/projects — List all projects
pub async fn list_projects(
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }
    match Project::find_all(&state.db).await {
        Ok(projects) => Json(projects).into_response(),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// DELETE /api/projects/:id — Delete a project, stop its process, and clean up files
pub async fn delete_project(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let _ = runner::kill_process(&id, &state.process_manager).await;

    if let Err(e) = Project::delete(&state.db, &id).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    // Clean up app directory (binary, data, logs) and S3 storage
    let app_dir = state.config.apps_dir().join(&project.name);
    let _ = tokio::fs::remove_dir_all(&app_dir).await;
    let s3_dir = state.config.storage_dir().join(&project.id);
    let _ = tokio::fs::remove_dir_all(&s3_dir).await;

    (StatusCode::OK, Json(serde_json::json!({"deleted": id}))).into_response()
}

/// POST /api/projects/:id/restart
pub async fn restart_project(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let _ = runner::kill_process(&id, &state.process_manager).await;

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
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

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
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let db_path = state.config.project_data_dir(&project.name).join("app.db");
    if !db_path.exists() {
        return api_err(StatusCode::NOT_FOUND, "No database found for this project");
    }

    let backup_path = std::env::temp_dir().join(format!("{}-backup.db", project.name));
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
                    let filename = format!("attachment; filename=\"{}-backup.db\"", project.name);
                    (
                        StatusCode::OK,
                        [
                            ("Content-Type", "application/octet-stream"),
                            ("Content-Disposition", filename.as_str()),
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

/// GET /api/projects/:id/env — Get the raw .env file contents
pub async fn get_env(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let env_path = state.config.project_data_dir(&project.name).join(".env");
    let content = tokio::fs::read_to_string(&env_path).await.unwrap_or_default();
    (StatusCode::OK, content).into_response()
}

#[derive(Deserialize)]
pub struct UpdateEnvPayload {
    pub env_content: String,
}

/// POST /api/projects/:id/env — Update the .env file and restart the project
pub async fn update_env(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
    Json(payload): Json<UpdateEnvPayload>,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let data_dir = state.config.project_data_dir(&project.name);
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create data dir: {}", e));
    }

    let env_path = data_dir.join(".env");
    if let Err(e) = tokio::fs::write(&env_path, &payload.env_content).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save .env file: {}", e));
    }

    let _ = runner::kill_process(&id, &state.process_manager).await;
    match runner::spawn_process(&state.config, &project, &state.process_manager).await {
        Ok(pid) => {
            let _ = Project::update_status(&state.db, &id, ProjectStatus::Running, Some(pid as i64)).await;
            (StatusCode::OK, Json(serde_json::json!({"updated": true, "restarted": true}))).into_response()
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Saved but restart failed: {}", e)),
    }
}

// ─────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────

pub async fn ensure_admin_user(db: &SqlitePool) -> anyhow::Result<String> {
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM users WHERE username = 'admin' LIMIT 1")
        .fetch_optional(db)
        .await?;

    // If admin already exists, leave the password untouched
    if let Some((id,)) = existing {
        return Ok(id);
    }

    let user = crate::db::models::User::new("admin", "admin123");
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, api_key, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&user.id)
    .bind(&user.username)
    .bind(&user.password_hash)
    .bind(&user.api_key)
    .bind(user.created_at)
    .execute(db)
    .await?;

    tracing::info!("⚠️  Created default admin user: admin / admin123");

    Ok(user.id)
}
