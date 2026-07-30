use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// ─────────────────────────────────────────────
// Enums
// ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProjectStatus {
    Running,
    Stopped,
    Deploying,
    Suspended,
    Error,
}

impl std::fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectStatus::Running => write!(f, "Running"),
            ProjectStatus::Stopped => write!(f, "Stopped"),
            ProjectStatus::Deploying => write!(f, "Deploying"),
            ProjectStatus::Suspended => write!(f, "Suspended"),
            ProjectStatus::Error => write!(f, "Error"),
        }
    }
}

// ─────────────────────────────────────────────
// User
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub api_key: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl User {
    pub fn new(username: impl Into<String>, password: &str) -> Self {
        let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST).unwrap_or_default();
        // Generate a random prefix 'paas_sk_' + 32 hex chars
        let api_key = format!("paas_sk_{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
        User {
            id: Uuid::new_v4().to_string(),
            username: username.into(),
            password_hash: hash,
            api_key: Some(api_key),
            created_at: Utc::now(),
        }
    }

    pub fn verify_password(&self, password: &str) -> bool {
        bcrypt::verify(password, &self.password_hash).unwrap_or(false)
    }
}

// ─────────────────────────────────────────────
// Session
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
}

impl Session {
    pub fn new(user_id: impl Into<String>) -> Self {
        Session {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.into(),
            // Sessions expire in 7 days
            expires_at: Utc::now() + chrono::Duration::days(7),
        }
    }

    pub async fn insert(&self, db: &SqlitePool) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO sessions (id, user_id, expires_at) VALUES (?, ?, ?)"
        )
        .bind(&self.id)
        .bind(&self.user_id)
        .bind(self.expires_at)
        .execute(db)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, db: &SqlitePool) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(&self.id)
            .execute(db)
            .await?;
        Ok(())
    }
}

// ─────────────────────────────────────────────
// Project
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub subdomain: String,
    /// Port assigned on the host for this app's process
    pub port: i64,
    pub status: String, // ProjectStatus serialized as text
    /// OS PID of the running process, if any
    pub pid: Option<i64>,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Project {
    pub fn new(
        name: impl Into<String>,
        subdomain: impl Into<String>,
        port: u16,
        user_id: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Project {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            subdomain: subdomain.into(),
            port: port as i64,
            status: ProjectStatus::Stopped.to_string(),
            pid: None,
            user_id: user_id.into(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn status(&self) -> ProjectStatus {
        match self.status.as_str() {
            "Running" => ProjectStatus::Running,
            "Stopped" => ProjectStatus::Stopped,
            "Deploying" => ProjectStatus::Deploying,
            "Suspended" => ProjectStatus::Suspended,
            _ => ProjectStatus::Error,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status() == ProjectStatus::Running
    }
}

// ─────────────────────────────────────────────
// Bucket
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Bucket {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub created_at: DateTime<Utc>,
}

impl Bucket {
    pub fn new(name: impl Into<String>, project_id: impl Into<String>) -> Self {
        Bucket {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            project_id: project_id.into(),
            created_at: Utc::now(),
        }
    }
}

// ─────────────────────────────────────────────
// DB Query helpers — using runtime queries (no compile-time DATABASE_URL needed)
// ─────────────────────────────────────────────

use sqlx::SqlitePool;

impl Project {
    pub async fn find_all(pool: &SqlitePool) -> sqlx::Result<Vec<Project>> {
        sqlx::query_as::<_, Project>(
            "SELECT id, name, subdomain, port, status, pid, user_id, created_at, updated_at \
             FROM projects ORDER BY created_at DESC",
        )
        .fetch_all(pool)
        .await
    }

    pub async fn find_by_id(pool: &SqlitePool, id: &str) -> sqlx::Result<Option<Project>> {
        sqlx::query_as::<_, Project>(
            "SELECT id, name, subdomain, port, status, pid, user_id, created_at, updated_at \
             FROM projects WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    pub async fn find_by_subdomain(pool: &SqlitePool, subdomain: &str) -> sqlx::Result<Option<Project>> {
        sqlx::query_as::<_, Project>(
            "SELECT id, name, subdomain, port, status, pid, user_id, created_at, updated_at \
             FROM projects WHERE subdomain = ?",
        )
        .bind(subdomain)
        .fetch_optional(pool)
        .await
    }

    pub async fn insert(&self, pool: &SqlitePool) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO projects (id, name, subdomain, port, status, pid, user_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(&self.name)
        .bind(&self.subdomain)
        .bind(self.port)
        .bind(&self.status)
        .bind(self.pid)
        .bind(&self.user_id)
        .bind(self.created_at)
        .bind(self.updated_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn update_status(
        pool: &SqlitePool,
        id: &str,
        status: ProjectStatus,
        pid: Option<i64>,
    ) -> sqlx::Result<()> {
        let status_str = status.to_string();
        let updated_at = Utc::now();
        sqlx::query(
            "UPDATE projects SET status = ?, pid = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&status_str)
        .bind(pid)
        .bind(updated_at)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn delete(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
        sqlx::query("DELETE FROM projects WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }
}

impl Bucket {
    pub async fn find_by_project(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Bucket>> {
        sqlx::query_as::<_, Bucket>(
            "SELECT id, name, project_id, created_at FROM buckets WHERE project_id = ? ORDER BY created_at",
        )
        .bind(project_id)
        .fetch_all(pool)
        .await
    }

    pub async fn insert(&self, pool: &SqlitePool) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO buckets (id, name, project_id, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&self.id)
        .bind(&self.name)
        .bind(&self.project_id)
        .bind(self.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }
}
