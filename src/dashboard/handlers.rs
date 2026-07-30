use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
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
        s3::list_bucket_objects(&state.config, &bucket.name).unwrap_or_default()
    } else {
        vec![]
    };

    // Read last 100 lines of the log file
    let log_lines = read_log_tail(&state.config.project_log_path(&project.name), 100);

    render(ProjectDetailTemplate {
        project,
        buckets,
        objects,
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

// ─────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────

/// Read the last `n` lines of a file
fn read_log_tail(path: &std::path::Path, n: usize) -> Vec<String> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
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
