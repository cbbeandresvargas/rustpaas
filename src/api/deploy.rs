use std::sync::Arc;

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Semaphore};
use tracing::info;

use crate::config::Config;
use crate::db::models::{Bucket, DeployHistory, Project, ProjectStatus};
use crate::runner::{self, ProcessManager};
use crate::s3;

// ─────────────────────────────────────────────
// Shared application state
// ─────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub db: SqlitePool,
    pub process_manager: Arc<Mutex<ProcessManager>>,
    /// Limits concurrent deploys to 1 to prevent race conditions
    pub deploy_semaphore: Arc<Semaphore>,
}

// ─────────────────────────────────────────────
// Response / request types
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

#[derive(Deserialize)]
pub struct UpdateEnvPayload {
    pub env_content: String,
}

#[derive(Deserialize)]
pub struct CustomDomainPayload {
    /// Set to Some("") or None to remove the custom domain
    pub domain: Option<String>,
}

#[derive(Deserialize)]
pub struct ResourceLimitsPayload {
    /// RAM limit in MB (null to remove limit)
    pub ram_limit_mb: Option<i64>,
}

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

fn api_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ApiError { error: msg.into() })).into_response()
}

/// Authenticate a request via Bearer API key or session cookie.
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
const MAX_DEPLOY_HISTORY: i64 = 3;

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

/// Write the binary to `bin/{name}`, make it executable, return the path.
fn write_binary(bin_dir: &std::path::Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(bin_dir)?;

    #[cfg(windows)]
    let bin_path = bin_dir.join(format!("{}.exe", name));
    #[cfg(not(windows))]
    let bin_path = bin_dir.join(name);

    std::fs::write(&bin_path, bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
    }

    Ok(())
}

// ─────────────────────────────────────────────
// POST /api/deploy
// ─────────────────────────────────────────────

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

    // Acquire deploy lock — only 1 concurrent deploy allowed
    let _permit = match state.deploy_semaphore.try_acquire() {
        Ok(p) => p,
        Err(_) => return api_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "A deploy is already in progress. Please wait and try again.",
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
                            return api_err(StatusCode::PAYLOAD_TOO_LARGE, "Upload exceeds 100 MB limit");
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

    let is_bundle = binary.len() >= 2 && binary[0] == 0x1f && binary[1] == 0x8b;

    // ── Re-deploy ────────────────────────────────────────────────────────────
    let existing: Option<Project> = sqlx::query_as::<_, Project>(
        "SELECT id, name, subdomain, port, status, pid, user_id, created_at, updated_at, \
         custom_domain, ram_limit_mb FROM projects WHERE name = ? LIMIT 1",
    )
    .bind(&name)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    if let Some(existing) = existing {
        if existing.user_id != user_id {
            return api_err(StatusCode::FORBIDDEN, "A project with this name is owned by another user");
        }

        let bin_dir = state.config.project_bin_dir(&name);
        let deploys_dir = state.config.project_deploys_dir(&name);

        // Save current binary snapshot for rollback before overwriting
        match runner::save_deploy_snapshot(&bin_dir, &deploys_dir, &name, is_bundle, &binary) {
            Ok(snapshot_path) => {
                if let Ok(old_paths) = DeployHistory::prune(&state.db, &existing.id, MAX_DEPLOY_HISTORY).await {
                    runner::delete_snapshots(&old_paths);
                }
                let _ = DeployHistory::insert(&state.db, &existing.id, &snapshot_path, is_bundle).await;
            }
            Err(e) => tracing::warn!("Failed to save deploy snapshot: {}", e),
        }

        // Write new binary or extract bundle
        if is_bundle {
            let workspace_dir = state.config.project_workspace_dir(&name);
            match runner::extract_bundle(&binary, &workspace_dir, &name) {
                Ok(bin_path) => {
                    // Copy the extracted binary into bin/ so find_binary() can locate it
                    let bin_dir = state.config.project_bin_dir(&name);
                    let _ = std::fs::create_dir_all(&bin_dir);
                    let dest = bin_dir.join(&name);
                    let _ = std::fs::copy(&bin_path, &dest);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
                    }
                }
                Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to extract bundle: {}", e)),
            }
        } else if let Err(e) = write_binary(&bin_dir, &name, &binary) {
            return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write binary: {}", e));
        }

        let _ = runner::kill_process(&existing.id, &state.process_manager).await;
        let _ = Project::update_status(&state.db, &existing.id, ProjectStatus::Deploying, None).await;

        return match runner::spawn_process(&state.config, &existing, &state.process_manager).await {
            Ok(pid) => {
                let _ = Project::update_status(&state.db, &existing.id, ProjectStatus::Running, Some(pid as i64)).await;
                info!("✅ Re-deployed '{}' on port {}", name, existing.port);
                (StatusCode::OK, Json(DeployResponse {
                    project_id: existing.id,
                    name,
                    port: existing.port as u16,
                    pid,
                    message: "Re-deployed successfully".to_string(),
                })).into_response()
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

    if is_bundle {
        let workspace_dir = state.config.project_workspace_dir(&name);
        match runner::extract_bundle(&binary, &workspace_dir, &name) {
            Ok(bin_path) => {
                let _ = std::fs::create_dir_all(&bin_dir);
                let dest = bin_dir.join(&name);
                let _ = std::fs::copy(&bin_path, &dest);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
                }
            }
            Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to extract bundle: {}", e)),
        }
    } else if let Err(e) = write_binary(&bin_dir, &name, &binary) {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write binary: {}", e));
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
            let _ = Project::update_status(&state.db, &project.id, ProjectStatus::Running, Some(pid as i64)).await;
            info!("✅ Deployed '{}' on port {}", name, port);
            (StatusCode::CREATED, Json(DeployResponse {
                project_id: project.id,
                name,
                port,
                pid,
                message: "Deployed successfully".to_string(),
            })).into_response()
        }
        Err(e) => {
            let _ = Project::update_status(&state.db, &project.id, ProjectStatus::Error, None).await;
            api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to start process: {}", e))
        }
    }
}

// ─────────────────────────────────────────────
// Project CRUD (all authenticated)
// ─────────────────────────────────────────────

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

    let app_dir = state.config.apps_dir().join(&project.name);
    let _ = tokio::fs::remove_dir_all(&app_dir).await;
    let s3_dir = state.config.storage_dir().join(&project.id);
    let _ = tokio::fs::remove_dir_all(&s3_dir).await;

    (StatusCode::OK, Json(serde_json::json!({"deleted": id}))).into_response()
}

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
    }).await;

    match result {
        Ok(Ok(_)) => match tokio::fs::read(&backup_path).await {
            Ok(bytes) => {
                let cd = format!("attachment; filename=\"{}-backup.db\"", project.name);
                (StatusCode::OK, [
                    ("Content-Type", "application/octet-stream"),
                    ("Content-Disposition", cd.as_str()),
                ], bytes).into_response()
            }
            Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to read backup: {}", e)),
        },
        Ok(Err(e)) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Backup task failed: {}", e)),
    }
}

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

    if let Err(e) = tokio::fs::write(data_dir.join(".env"), &payload.env_content).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to save .env: {}", e));
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
// Deploy history / rollback
// ─────────────────────────────────────────────

/// GET /api/projects/:id/deploys
pub async fn list_deploys(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    match DeployHistory::find_by_project(&state.db, &id).await {
        Ok(history) => Json(history).into_response(),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// POST /api/projects/:id/rollback/:deploy_id
pub async fn rollback_deploy(
    Path((id, deploy_id)): Path<(String, String)>,
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

    let entry = match DeployHistory::find_by_id(&state.db, &deploy_id).await {
        Ok(Some(e)) if e.project_id == id => e,
        Ok(_) => return api_err(StatusCode::NOT_FOUND, "Deploy entry not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let snapshot_path = std::path::Path::new(&entry.binary_path);
    if !snapshot_path.exists() {
        return api_err(StatusCode::NOT_FOUND, "Snapshot file not found on disk");
    }

    let bin_dir = state.config.project_bin_dir(&project.name);

    if entry.is_bundle {
        let bytes = match std::fs::read(snapshot_path) {
            Ok(b) => b,
            Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to read snapshot: {}", e)),
        };
        let workspace_dir = state.config.project_workspace_dir(&project.name);
        if let Err(e) = runner::extract_bundle(&bytes, &workspace_dir, &project.name) {
            return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to extract bundle: {}", e));
        }
        let dest = bin_dir.join(&project.name);
        let _ = std::fs::create_dir_all(&bin_dir);
        if let Some(bin_in_workspace) = std::fs::read_dir(&workspace_dir)
            .ok()
            .and_then(|mut rd| rd.find_map(|e| {
                let e = e.ok()?;
                let p = e.path();
                if p.file_name()?.to_str()? == project.name { Some(p) } else { None }
            }))
        {
            let _ = std::fs::copy(bin_in_workspace, &dest);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
        }
    } else {
        let dest = bin_dir.join(&project.name);
        let _ = std::fs::create_dir_all(&bin_dir);
        if let Err(e) = std::fs::copy(snapshot_path, &dest) {
            return api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to restore binary: {}", e));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
        }
    }

    let _ = runner::kill_process(&id, &state.process_manager).await;
    let _ = Project::update_status(&state.db, &id, ProjectStatus::Deploying, None).await;

    match runner::spawn_process(&state.config, &project, &state.process_manager).await {
        Ok(pid) => {
            let _ = Project::update_status(&state.db, &id, ProjectStatus::Running, Some(pid as i64)).await;
            info!("⏪ Rolled back project '{}' to deploy {}", project.name, deploy_id);
            (StatusCode::OK, Json(serde_json::json!({"rolled_back": true, "pid": pid}))).into_response()
        }
        Err(e) => {
            let _ = Project::update_status(&state.db, &id, ProjectStatus::Error, None).await;
            api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Rollback failed to restart: {}", e))
        }
    }
}

// ─────────────────────────────────────────────
// Metrics
// ─────────────────────────────────────────────

/// GET /api/projects/:id/metrics
pub async fn get_metrics(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let mgr = state.process_manager.lock().await;
    match mgr.metrics.get(&id) {
        Some(m) => Json(m).into_response(),
        None => Json(runner::ProcessMetrics::default()).into_response(),
    }
}

// ─────────────────────────────────────────────
// Custom domain
// ─────────────────────────────────────────────

/// POST /api/projects/:id/custom_domain
pub async fn set_custom_domain(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
    Json(payload): Json<CustomDomainPayload>,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    // Normalize empty string to None
    let domain = payload.domain.as_deref().filter(|d| !d.trim().is_empty());

    match Project::update_custom_domain(&state.db, &id, domain).await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({"custom_domain": domain}))).into_response(),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ─────────────────────────────────────────────
// Resource limits
// ─────────────────────────────────────────────

/// PUT /api/projects/:id/limits
pub async fn set_limits(
    Path(id): Path<String>,
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
    Json(payload): Json<ResourceLimitsPayload>,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    if let Err(e) = Project::update_ram_limit(&state.db, &id, payload.ram_limit_mb).await {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }

    // Restart process so the new limit takes effect
    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return api_err(StatusCode::NOT_FOUND, "Project not found"),
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let _ = runner::kill_process(&id, &state.process_manager).await;
    match runner::spawn_process(&state.config, &project, &state.process_manager).await {
        Ok(pid) => {
            let _ = Project::update_status(&state.db, &id, ProjectStatus::Running, Some(pid as i64)).await;
            (StatusCode::OK, Json(serde_json::json!({"updated": true, "restarted": true}))).into_response()
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Limit saved but restart failed: {}", e)),
    }
}

// ─────────────────────────────────────────────
// Caddy config generator (HTTPS setup helper)
// ─────────────────────────────────────────────

/// GET /api/caddy-config — Returns a ready-to-use Caddyfile for this PaaS instance
pub async fn caddy_config(
    State(state): State<AppState>,
    jar: axum_extra::extract::cookie::CookieJar,
    headers: axum::http::HeaderMap,
) -> Response {
    if authenticate(&jar, &headers, &state.db).await.is_none() {
        return api_err(StatusCode::UNAUTHORIZED, "Authentication required");
    }

    let projects = match Project::find_all(&state.db).await {
        Ok(p) => p,
        Err(e) => return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };

    let domain = &state.config.domain;
    let paas_port = state.config.port;

    let mut cfg = format!(
        "# RustPaaS — generated Caddyfile\n\
         # Place this at /etc/caddy/Caddyfile and run: systemctl reload caddy\n\n\
         # Wildcard cert covers all app subdomains\n\
         *.{domain} {{\n\
         \treverse_proxy localhost:{paas_port}\n\
         }}\n\n\
         # Main dashboard\n\
         {domain} {{\n\
         \treverse_proxy localhost:{paas_port}\n\
         }}\n"
    );

    // Custom domains
    for project in &projects {
        if let Some(ref custom) = project.custom_domain {
            cfg.push_str(&format!(
                "\n# Custom domain for project '{}'\n{} {{\n\treverse_proxy localhost:{}\n}}\n",
                project.name, custom, paas_port
            ));
        }
    }

    (StatusCode::OK, [("Content-Type", "text/plain")], cfg).into_response()
}

// ─────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────

pub async fn ensure_admin_user(db: &SqlitePool) -> anyhow::Result<String> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM users WHERE username = 'admin' LIMIT 1",
    )
    .fetch_optional(db)
    .await?;

    if let Some((id,)) = existing {
        return Ok(id);
    }

    let user = crate::db::models::User::new("admin", "admin123");
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, api_key, created_at) \
         VALUES (?, ?, ?, ?, ?)",
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
