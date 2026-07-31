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
    /// Optional custom domain mapping (e.g. "myapp.com")
    pub custom_domain: Option<String>,
    /// RAM limit in MB (None = unlimited)
    pub ram_limit_mb: Option<i64>,
}

// ─────────────────────────────────────────────
// DeployHistory
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DeployHistory {
    pub id: String,
    pub project_id: String,
    pub binary_path: String,
    pub is_bundle: bool,
    pub deployed_at: DateTime<Utc>,
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
            custom_domain: None,
            ram_limit_mb: None,
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

const PROJECT_COLS: &str =
    "id, name, subdomain, port, status, pid, user_id, created_at, updated_at, custom_domain, ram_limit_mb";

impl Project {
    pub async fn find_all(pool: &SqlitePool) -> sqlx::Result<Vec<Project>> {
        sqlx::query_as::<_, Project>(
            &format!("SELECT {PROJECT_COLS} FROM projects ORDER BY created_at DESC"),
        )
        .fetch_all(pool)
        .await
    }

    pub async fn find_by_id(pool: &SqlitePool, id: &str) -> sqlx::Result<Option<Project>> {
        sqlx::query_as::<_, Project>(
            &format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?"),
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    pub async fn find_by_subdomain(pool: &SqlitePool, subdomain: &str) -> sqlx::Result<Option<Project>> {
        sqlx::query_as::<_, Project>(
            &format!("SELECT {PROJECT_COLS} FROM projects WHERE subdomain = ?"),
        )
        .bind(subdomain)
        .fetch_optional(pool)
        .await
    }

    pub async fn find_by_custom_domain(pool: &SqlitePool, domain: &str) -> sqlx::Result<Option<Project>> {
        sqlx::query_as::<_, Project>(
            &format!("SELECT {PROJECT_COLS} FROM projects WHERE custom_domain = ?"),
        )
        .bind(domain)
        .fetch_optional(pool)
        .await
    }

    pub async fn insert(&self, pool: &SqlitePool) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO projects \
             (id, name, subdomain, port, status, pid, user_id, created_at, updated_at) \
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
        sqlx::query(
            "UPDATE projects SET status = ?, pid = ?, updated_at = ? WHERE id = ?",
        )
        .bind(status.to_string())
        .bind(pid)
        .bind(Utc::now())
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn update_custom_domain(
        pool: &SqlitePool,
        id: &str,
        domain: Option<&str>,
    ) -> sqlx::Result<()> {
        sqlx::query("UPDATE projects SET custom_domain = ?, updated_at = ? WHERE id = ?")
            .bind(domain)
            .bind(Utc::now())
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn update_ram_limit(
        pool: &SqlitePool,
        id: &str,
        ram_limit_mb: Option<i64>,
    ) -> sqlx::Result<()> {
        sqlx::query("UPDATE projects SET ram_limit_mb = ?, updated_at = ? WHERE id = ?")
            .bind(ram_limit_mb)
            .bind(Utc::now())
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

impl DeployHistory {
    pub async fn insert(pool: &SqlitePool, project_id: &str, binary_path: &str, is_bundle: bool) -> sqlx::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO deploy_history (id, project_id, binary_path, is_bundle) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(project_id)
        .bind(binary_path)
        .bind(is_bundle)
        .execute(pool)
        .await?;
        Ok(id)
    }

    pub async fn find_by_project(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<DeployHistory>> {
        sqlx::query_as::<_, DeployHistory>(
            "SELECT id, project_id, binary_path, is_bundle, deployed_at \
             FROM deploy_history WHERE project_id = ? ORDER BY deployed_at DESC",
        )
        .bind(project_id)
        .fetch_all(pool)
        .await
    }

    pub async fn find_by_id(pool: &SqlitePool, id: &str) -> sqlx::Result<Option<DeployHistory>> {
        sqlx::query_as::<_, DeployHistory>(
            "SELECT id, project_id, binary_path, is_bundle, deployed_at \
             FROM deploy_history WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    /// Keep only the last `max` entries per project, deleting older ones.
    pub async fn prune(pool: &SqlitePool, project_id: &str, max: i64) -> sqlx::Result<Vec<String>> {
        let old: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, binary_path FROM deploy_history \
             WHERE project_id = ? ORDER BY deployed_at DESC LIMIT -1 OFFSET ?",
        )
        .bind(project_id)
        .bind(max)
        .fetch_all(pool)
        .await?;

        for (id, _) in &old {
            sqlx::query("DELETE FROM deploy_history WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await?;
        }

        Ok(old.into_iter().map(|(_, path)| path).collect())
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
