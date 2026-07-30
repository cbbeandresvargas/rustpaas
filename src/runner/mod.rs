use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::Config;
use crate::db::models::Project;

pub mod proxy;

// ─────────────────────────────────────────────
// ProcessManager
// ─────────────────────────────────────────────

/// Tracks all running app processes.
/// Stored in shared Axum state (Arc<Mutex<...>>).
#[derive(Debug, Default)]
pub struct ProcessManager {
    /// Map from project_id → running child process
    processes: HashMap<String, Child>,
    /// Map from project_id → last request timestamp (for auto-suspend)
    last_activity: HashMap<String, std::time::Instant>,
}

impl ProcessManager {
    pub fn new() -> Arc<Mutex<ProcessManager>> {
        Arc::new(Mutex::new(ProcessManager::default()))
    }

    /// Returns true if a process is currently running for the given project.
    pub fn is_running(&mut self, project_id: &str) -> bool {
        if let Some(child) = self.processes.get_mut(project_id) {
            // Try to check if it's still alive by polling (non-blocking)
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Process has exited
                    self.processes.remove(project_id);
                    false
                }
                Ok(None) => true, // Still running
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Record activity for a project (used by auto-suspend timer)
    pub fn record_activity(&mut self, project_id: &str) {
        self.last_activity.insert(project_id.to_string(), std::time::Instant::now());
    }

    /// Get time since last activity for a project
    pub fn idle_seconds(&self, project_id: &str) -> Option<u64> {
        self.last_activity.get(project_id).map(|t| t.elapsed().as_secs())
    }
}

// ─────────────────────────────────────────────
// Process spawn / kill
// ─────────────────────────────────────────────

/// Spawn a project's binary as a child process with the correct environment variables.
///
/// The binary's stdout and stderr are redirected to `app.log`.
pub async fn spawn_process(
    config: &Config,
    project: &Project,
    manager: &Arc<Mutex<ProcessManager>>,
) -> Result<u32> {
    let bin_dir = config.project_bin_dir(&project.name);
    let binary_path = find_binary(&bin_dir)?;

    let data_dir = config.project_data_dir(&project.name);
    std::fs::create_dir_all(&data_dir)
        .context("Failed to create project data directory")?;

    let log_path = config.project_log_path(&project.name);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;

    let log_file_err = log_file.try_clone()
        .context("Failed to clone log file handle")?;

    // Build environment for the child process
    let db_url = format!(
        "sqlite://{}",
        data_dir.join("app.db").display()
    );
    let s3_endpoint = format!("http://localhost:{}", config.s3_port);
    let bucket_name = format!("bucket-{}", project.name);

    let child = Command::new(&binary_path)
        .env("DATABASE_URL", &db_url)
        .env("S3_ENDPOINT", &s3_endpoint)
        .env("S3_BUCKET", &bucket_name)
        .env("PORT", project.port.to_string())
        .env("APP_NAME", &project.name)
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_file_err))
        .spawn()
        .with_context(|| format!("Failed to spawn binary: {}", binary_path.display()))?;

    let pid = child.id().context("Could not get child PID")?;

    info!(
        "▶️  Spawned project '{}' (pid={}, port={})",
        project.name, pid, project.port
    );

    // Store the child in the manager
    {
        let mut mgr = manager.lock().await;
        mgr.processes.insert(project.id.clone(), child);
        mgr.record_activity(&project.id);
    }

    Ok(pid)
}

/// Kill a running process for a project.
pub async fn kill_process(
    project_id: &str,
    manager: &Arc<Mutex<ProcessManager>>,
) -> Result<()> {
    let mut mgr = manager.lock().await;
    if let Some(mut child) = mgr.processes.remove(project_id) {
        child.kill().await.context("Failed to kill process")?;
        info!("⏹️  Killed process for project: {}", project_id);
    } else {
        warn!("No running process found for project: {}", project_id);
    }
    Ok(())
}

/// Find the first executable in a directory (cross-platform)
fn find_binary(bin_dir: &PathBuf) -> Result<PathBuf> {
    if !bin_dir.exists() {
        anyhow::bail!("Binary directory does not exist: {}", bin_dir.display());
    }

    for entry in std::fs::read_dir(bin_dir).context("Failed to read bin directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            // On Linux, check execute bit; on Windows, accept .exe files
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if path.metadata()?.permissions().mode() & 0o111 != 0 {
                    return Ok(path);
                }
            }
            #[cfg(windows)]
            {
                if path.extension().map(|e| e == "exe").unwrap_or(false) {
                    return Ok(path);
                }
            }
            // Fallback: return any file
            return Ok(path);
        }
    }

    anyhow::bail!("No binary found in: {}", bin_dir.display())
}
