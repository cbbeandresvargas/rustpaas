use askama::Template;

use crate::db::models::{Bucket, Project, User};
use crate::s3::S3Object;
use super::i18n::Dict;

// ─────────────────────────────────────────────
// Index — List of all projects
// ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub projects: Vec<Project>,
    pub user: Option<User>,
    pub app_name: String,
    pub t: &'static Dict,
    pub paas_version: &'static str,
}

// ─────────────────────────────────────────────
// Project Detail
// ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "project_detail.html")]
pub struct ProjectDetailTemplate {
    pub project: Project,
    pub buckets: Vec<Bucket>,
    pub objects: Vec<S3Object>,
    pub log_lines: Vec<String>,
    pub user: Option<User>,
    pub app_name: String,
    pub t: &'static Dict,
    pub paas_version: &'static str,
}

// ─────────────────────────────────────────────
// Error page
// ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorTemplate {
    pub code: u16,
    pub message: String,
    pub user: Option<User>,
    pub app_name: String,
    pub t: &'static Dict,
    pub paas_version: &'static str,
}

// ─────────────────────────────────────────────
// Landing & Docs pages
// ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "landing.html")]
pub struct LandingTemplate {
    pub user: Option<User>,
    pub app_name: String,
    pub t: &'static Dict,
    pub paas_version: &'static str,
}

#[derive(Template)]
#[template(path = "docs.html")]
pub struct DocsTemplate {
    pub user: Option<User>,
    pub app_name: String,
    pub t: &'static Dict,
    pub paas_version: &'static str,
}

// ─────────────────────────────────────────────
// Login page
// ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub error: Option<String>,
    pub app_name: String,
    pub t: &'static Dict,
    pub paas_version: &'static str,
}
