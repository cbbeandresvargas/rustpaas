use axum::{
    extract::{Path, State, Query},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use serde::Deserialize;
use askama::Template;
use tracing::error;

use crate::api::AppState;
use crate::db::models::{Bucket, Project, User};
use crate::s3;
use super::auth::{AuthUser, OptionalAuthUser};
use super::i18n::{Lang, Dict};
use super::templates::{
    IndexTemplate, ProjectDetailTemplate, ErrorTemplate, LandingTemplate, DocsTemplate
};

const PAAS_VERSION: &str = env!("CARGO_PKG_VERSION");

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

fn render<T: Template>(tmpl: T) -> Response {
    match tmpl.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Template render error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Template rendering failed").into_response()
        }
    }
}

fn error_page(code: u16, message: impl Into<String>, user: Option<User>, app_name: String, t: &'static Dict) -> Response {
    let tmpl = ErrorTemplate {
        code,
        message: message.into(),
        user,
        app_name,
        t,
        paas_version: PAAS_VERSION,
    };
    match tmpl.render() {
        Ok(html) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Html(html),
        )
            .into_response(),
        Err(_) => (code.to_string()).into_response(),
    }
}

// ─────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────

/// GET / — Landing page
pub async fn landing(Lang(t): Lang, OptionalAuthUser(user): OptionalAuthUser, State(state): State<AppState>) -> Response {
    if user.is_some() {
        return axum::response::Redirect::to("/dashboard").into_response();
    }
    render(LandingTemplate {
        user,
        app_name: state.config.app_name.clone(),
        t,
        paas_version: PAAS_VERSION,
    })
}

/// GET /docs — Documentation page
pub async fn docs(Lang(t): Lang, OptionalAuthUser(user): OptionalAuthUser, State(state): State<AppState>) -> Response {
    render(DocsTemplate {
        user,
        app_name: state.config.app_name.clone(),
        t,
        paas_version: PAAS_VERSION,
    })
}

/// GET /dashboard — Dashboard home: list all projects
pub async fn index(Lang(t): Lang, AuthUser(user): AuthUser, State(state): State<AppState>) -> Response {
    let app_name = state.config.app_name.clone();
    let projects = match Project::find_all(&state.db).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to load projects: {}", e);
            return error_page(500, format!("Failed to load projects: {}", e), Some(user), app_name, t);
        }
    };

    render(IndexTemplate {
        projects,
        user: Some(user),
        app_name,
        t,
        paas_version: PAAS_VERSION,
    })
}

/// GET /dashboard/projects/:id — Project detail page
pub async fn project_detail(
    Lang(t): Lang,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let app_name = state.config.app_name.clone();
    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return error_page(404, format!("Project '{}' not found", id), Some(user), app_name, t),
        Err(e) => return error_page(500, format!("DB error: {}", e), Some(user), app_name, t),
    };

    let buckets = Bucket::find_by_project(&state.db, &project.id)
        .await
        .unwrap_or_default();

    // List objects in the first bucket (if any)
    let objects = if let Some(bucket) = buckets.first() {
        s3::list_bucket_objects(&state.config, &project.id, &bucket.name).unwrap_or_default()
    } else {
        vec![]
    };

    // Read last 100 lines of the log file
    let log_lines = read_log_tail(&state.config.project_log_path(&project.name), 100);

    // Read S3 secret or generate if not exists
    let s3_port = (project.port as u16) + 10000;
    let data_dir = state.config.project_data_dir(&project.name);
    let s3_secret_path = data_dir.join(".s3_secret");
    let s3_secret_key = if s3_secret_path.exists() {
        std::fs::read_to_string(&s3_secret_path).unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    } else {
        let new_secret = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::write(&s3_secret_path, &new_secret);
        new_secret
    };

    render(ProjectDetailTemplate {
        project: project.clone(),
        buckets,
        objects,
        s3_port,
        s3_access_key: project.id.clone(),
        s3_secret_key,
        log_lines,
        user: Some(user),
        app_name,
        t,
        paas_version: PAAS_VERSION,
    })
}

/// GET /dashboard/projects/:id/logs — Returns the last N log lines as plain text (for polling)
pub async fn project_logs(
    AuthUser(_user): AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let project = match Project::find_by_id(&state.db, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, "Project not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let log_path = state.config.project_log_path(&project.name);
    let lines = read_log_tail(&log_path, 100);
    (StatusCode::OK, lines.join("\n")).into_response()
}

#[derive(Deserialize)]
pub struct LangQuery {
    pub lang: String,
    pub next: Option<String>,
}

/// GET /set_lang?lang=es&next=/
pub async fn set_lang(jar: CookieJar, Query(q): Query<LangQuery>) -> (CookieJar, axum::response::Redirect) {
    let cookie = Cookie::build(("lang", q.lang.clone()))
        .path("/")
        .same_site(axum_extra::extract::cookie::SameSite::Lax)
        .build();
    let redirect_url = q.next.unwrap_or_else(|| "/".to_string());
    (jar.add(cookie), axum::response::Redirect::to(&redirect_url))
}

// ─────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────

/// Read the last `n` lines of a file without loading the entire file into memory.
/// Reads at most 64 KB from the end of the file to find the requested lines.
fn read_log_tail(path: &std::path::Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };

    let file_size = match file.seek(SeekFrom::End(0)) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let read_from = file_size.saturating_sub(65536);
    if file.seek(SeekFrom::Start(read_from)).is_err() {
        return vec![];
    }

    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return vec![];
    }

    let content = String::from_utf8_lossy(&buf);
    content
        .lines()
        .rev()
        .take(n)
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}
