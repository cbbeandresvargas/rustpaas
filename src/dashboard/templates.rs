use askama::Template;

use crate::db::models::{Bucket, Project};
use crate::s3::S3Object;

// ─────────────────────────────────────────────
// Index — List of all projects
// ─────────────────────────────────────────────

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub projects: Vec<Project>,
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
    pub paas_version: &'static str,
}
