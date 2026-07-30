use std::path::PathBuf;
use anyhow::{Context, Result};

/// Global configuration loaded from environment variables
#[derive(Debug, Clone)]
pub struct Config {
    /// Name of the PaaS instance (configurable via APP_NAME)
    pub app_name: String,
    /// Root directory for all PaaS data (apps, DBs, S3 buckets)
    pub data_dir: PathBuf,
    /// Hostname/IP the PaaS server listens on
    pub host: String,
    /// Port for the main HTTP server (dashboard + API)
    pub port: u16,
    /// Port for the embedded S3 server
    pub s3_port: u16,
    /// Base domain for app subdomains (e.g., "mypaas.com" → "myapp.mypaas.com")
    pub domain: String,
    /// Range start for dynamic port assignment to apps
    pub app_port_start: u16,
    /// Range end for dynamic port assignment to apps
    pub app_port_end: u16,
    /// Minutes of inactivity before auto-suspending an app (Phase 4)
    pub suspend_timeout_mins: u64,
}

impl Config {
    /// Load configuration from environment variables with sensible defaults
    pub fn from_env() -> Result<Self> {
        // Use a Windows-friendly default path during development
        let default_data_dir = if cfg!(windows) {
            dirs_next_data_dir()
        } else {
            PathBuf::from("/var/lib/mypaas")
        };

        let data_dir = std::env::var("MYPAAS_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or(default_data_dir);

        Ok(Config {
            app_name: std::env::var("APP_NAME").unwrap_or_else(|_| "RustPaaS".to_string()),
            data_dir,
            host: std::env::var("MYPAAS_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: std::env::var("MYPAAS_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()
                .context("MYPAAS_PORT must be a valid port number")?,
            s3_port: std::env::var("MYPAAS_S3_PORT")
                .unwrap_or_else(|_| "9000".to_string())
                .parse()
                .context("MYPAAS_S3_PORT must be a valid port number")?,
            domain: std::env::var("MYPAAS_DOMAIN")
                .unwrap_or_else(|_| "localhost".to_string()),
            app_port_start: std::env::var("MYPAAS_APP_PORT_START")
                .unwrap_or_else(|_| "8100".to_string())
                .parse()
                .context("MYPAAS_APP_PORT_START must be a valid port number")?,
            app_port_end: std::env::var("MYPAAS_APP_PORT_END")
                .unwrap_or_else(|_| "8999".to_string())
                .parse()
                .context("MYPAAS_APP_PORT_END must be a valid port number")?,
            suspend_timeout_mins: std::env::var("MYPAAS_SUSPEND_TIMEOUT_MINS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .context("MYPAAS_SUSPEND_TIMEOUT_MINS must be a number")?,
        })
    }

    /// Path to the main PaaS SQLite database
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("paas.db")
    }

    /// Database URL in the format SQLx expects
    pub fn db_url(&self) -> String {
        format!("sqlite://{}", self.db_path().display())
    }

    /// Root directory for all deployed apps
    pub fn apps_dir(&self) -> PathBuf {
        self.data_dir.join("apps")
    }

    /// Root directory for S3 buckets storage
    pub fn storage_dir(&self) -> PathBuf {
        self.data_dir.join("storage").join("buckets")
    }

    /// Directory for a specific project's binary
    pub fn project_bin_dir(&self, project_name: &str) -> PathBuf {
        self.apps_dir().join(project_name).join("bin")
    }

    /// Directory for a specific project's SQLite database
    pub fn project_data_dir(&self, project_name: &str) -> PathBuf {
        self.apps_dir().join(project_name).join("data")
    }

    /// Path to a project's log file
    pub fn project_log_path(&self, project_name: &str) -> PathBuf {
        self.apps_dir().join(project_name).join("app.log")
    }

    /// Create all required directories if they don't exist
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .context("Failed to create data_dir")?;
        std::fs::create_dir_all(self.apps_dir())
            .context("Failed to create apps_dir")?;
        std::fs::create_dir_all(self.storage_dir())
            .context("Failed to create storage_dir")?;
        Ok(())
    }
}

/// Returns a platform-appropriate default data directory
fn dirs_next_data_dir() -> PathBuf {
    // On Windows during development, use %APPDATA%\mypaas or a local folder
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("mypaas")
    } else {
        PathBuf::from("./mypaas-data")
    }
}
