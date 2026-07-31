use std::collections::{HashMap, VecDeque};
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
// Metrics
// ─────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessMetrics {
    pub cpu_percent: f32,
    pub ram_mb: u64,
    pub uptime_secs: u64,
    pub history: VecDeque<MetricsSnapshot>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub cpu_percent: f32,
    pub ram_mb: u64,
    pub ts: u64, // unix timestamp secs
}

impl Default for ProcessMetrics {
    fn default() -> Self {
        Self {
            cpu_percent: 0.0,
            ram_mb: 0,
            uptime_secs: 0,
            history: VecDeque::with_capacity(30),
        }
    }
}

impl ProcessMetrics {
    pub fn cpu_pct(&self) -> u32 {
        self.cpu_percent.round() as u32
    }

    pub fn uptime_display(&self) -> String {
        let s = self.uptime_secs;
        if s < 60 { format!("{}s", s) }
        else if s < 3600 { format!("{}m {}s", s / 60, s % 60) }
        else { format!("{}h {}m", s / 3600, (s % 3600) / 60) }
    }
}

// ─────────────────────────────────────────────
// ProcessManager
// ─────────────────────────────────────────────

#[derive(Debug)]
pub struct ProcessManager {
    processes: HashMap<String, Child>,
    s3_processes: HashMap<String, crate::s3::S3Handle>,
    last_activity: HashMap<String, std::time::Instant>,
    /// PID for each tracked project (for metrics without locking Child)
    pids: HashMap<String, u32>,
    /// Start time for uptime calculation
    start_times: HashMap<String, std::time::Instant>,
    pub metrics: HashMap<String, ProcessMetrics>,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self {
            processes: HashMap::new(),
            s3_processes: HashMap::new(),
            last_activity: HashMap::new(),
            pids: HashMap::new(),
            start_times: HashMap::new(),
            metrics: HashMap::new(),
        }
    }
}

impl ProcessManager {
    pub fn new() -> Arc<Mutex<ProcessManager>> {
        Arc::new(Mutex::new(ProcessManager::default()))
    }

    pub fn is_running(&mut self, project_id: &str) -> bool {
        if let Some(child) = self.processes.get_mut(project_id) {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.processes.remove(project_id);
                    self.s3_processes.remove(project_id);
                    self.pids.remove(project_id);
                    self.start_times.remove(project_id);
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    pub fn record_activity(&mut self, project_id: &str) {
        self.last_activity.insert(project_id.to_string(), std::time::Instant::now());
    }

    pub fn idle_seconds(&self, project_id: &str) -> Option<u64> {
        self.last_activity.get(project_id).map(|t| t.elapsed().as_secs())
    }

    pub fn active_projects(&self) -> Vec<String> {
        self.processes.keys().cloned().collect()
    }

    pub fn get_pid(&self, project_id: &str) -> Option<u32> {
        self.pids.get(project_id).copied()
    }
}

// ─────────────────────────────────────────────
// Metrics collection (called from background task)
// ─────────────────────────────────────────────

pub async fn refresh_metrics(manager: &Arc<Mutex<ProcessManager>>) {
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let project_pids: Vec<(String, u32)> = {
        let mgr = manager.lock().await;
        mgr.pids.iter().map(|(k, v)| (k.clone(), *v)).collect()
    };

    if project_pids.is_empty() {
        return;
    }

    let mut sys = System::new();
    let pids_to_refresh: Vec<Pid> = project_pids.iter()
        .map(|(_, pid)| Pid::from(*pid as usize))
        .collect();

    sys.refresh_processes(ProcessesToUpdate::Some(&pids_to_refresh));

    // Brief pause so CPU usage calculation is meaningful
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    sys.refresh_processes(ProcessesToUpdate::Some(&pids_to_refresh));

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut mgr = manager.lock().await;
    for (project_id, pid) in project_pids {
        let sysinfo_pid = Pid::from(pid as usize);
        let (cpu, ram) = if let Some(proc) = sys.process(sysinfo_pid) {
            (proc.cpu_usage(), proc.memory() / (1024 * 1024))
        } else {
            (0.0, 0)
        };

        let uptime = mgr.start_times
            .get(&project_id)
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);

        let entry = mgr.metrics.entry(project_id).or_default();
        entry.cpu_percent = cpu;
        entry.ram_mb = ram;
        entry.uptime_secs = uptime;

        if entry.history.len() >= 30 {
            entry.history.pop_front();
        }
        entry.history.push_back(MetricsSnapshot { cpu_percent: cpu, ram_mb: ram, ts });
    }
}

// ─────────────────────────────────────────────
// Log rotation
// ─────────────────────────────────────────────

const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024; // 5 MB

fn rotate_log_if_needed(log_path: &std::path::Path) {
    let size = log_path.metadata().map(|m| m.len()).unwrap_or(0);
    if size < LOG_MAX_BYTES {
        return;
    }

    let base = log_path.to_string_lossy().to_string();
    let log_2 = format!("{}.2", base);
    let log_1 = format!("{}.1", base);

    let _ = std::fs::remove_file(&log_2);
    let _ = std::fs::rename(&log_1, &log_2);
    let _ = std::fs::rename(log_path, &log_1);
}

/// Read the last `n` lines across all log rotation files.
pub fn read_log_tail_multi(log_path: &std::path::Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};

    // Collect rotated files in order (oldest first): .2 → .1 → current
    let base = log_path.to_string_lossy().to_string();
    let files = vec![
        std::path::PathBuf::from(format!("{}.2", base)),
        std::path::PathBuf::from(format!("{}.1", base)),
        log_path.to_path_buf(),
    ];

    let mut all_lines: Vec<String> = Vec::new();

    for path in files {
        if !path.exists() {
            continue;
        }
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let file_size = file.seek(SeekFrom::End(0)).unwrap_or(0);
        let read_from = file_size.saturating_sub(65536);
        if file.seek(SeekFrom::Start(read_from)).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            continue;
        }
        let content = String::from_utf8_lossy(&buf);
        for line in content.lines() {
            all_lines.push(line.to_string());
        }
    }

    // Take the last n lines
    let total = all_lines.len();
    if total <= n {
        all_lines
    } else {
        all_lines[total - n..].to_vec()
    }
}

// ─────────────────────────────────────────────
// Bundle detection & extraction
// ─────────────────────────────────────────────

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Extract a tar.gz bundle into `workspace_dir`.
/// Returns the path to the executable within the workspace (named after `project_name`).
pub fn extract_bundle(bytes: &[u8], workspace_dir: &std::path::Path, project_name: &str) -> Result<PathBuf> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    std::fs::create_dir_all(workspace_dir)
        .context("Failed to create workspace directory")?;

    // Remove existing workspace contents on re-deploy
    for entry in std::fs::read_dir(workspace_dir).context("Failed to read workspace dir")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    let gz = GzDecoder::new(bytes);
    let mut archive = Archive::new(gz);
    archive.unpack(workspace_dir)
        .context("Failed to extract tar.gz bundle")?;

    // Find the binary: file named after the project with execute bit
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Search recursively for a file named after the project
        fn find_exe(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(found) = find_exe(&path, name) {
                            return Some(found);
                        }
                    } else if path.file_name().map(|n| n == name).unwrap_or(false) {
                        return Some(path);
                    }
                }
            }
            None
        }

        // First try: exact name match
        let bin_path = find_exe(workspace_dir, project_name)
            .unwrap_or_else(|| {
                // Fallback: any executable file at the root of workspace
                std::fs::read_dir(workspace_dir)
                    .ok()
                    .and_then(|mut rd| {
                        rd.find_map(|e| {
                            let e = e.ok()?;
                            let p = e.path();
                            let mode = p.metadata().ok()?.permissions().mode();
                            if p.is_file() && mode & 0o111 != 0 { Some(p) } else { None }
                        })
                    })
                    .unwrap_or_else(|| workspace_dir.join(project_name))
            });

        // Ensure the binary is executable
        if bin_path.exists() {
            let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
        }
        return Ok(bin_path);
    }

    #[cfg(not(unix))]
    Ok(workspace_dir.join(format!("{}.exe", project_name)))
}

// ─────────────────────────────────────────────
// Deploy archive saving (for rollback)
// ─────────────────────────────────────────────

/// Save the current binary/bundle snapshot to the deploys history directory.
/// Returns the path where it was saved.
pub fn save_deploy_snapshot(
    bin_dir: &std::path::Path,
    deploys_dir: &std::path::Path,
    project_name: &str,
    is_bundle: bool,
    bytes: &[u8],
) -> Result<String> {
    std::fs::create_dir_all(deploys_dir)
        .context("Failed to create deploys directory")?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let ext = if is_bundle { "tar.gz" } else { "bin" };
    let snapshot_path = deploys_dir.join(format!("deploy-{}.{}", ts, ext));

    if is_bundle {
        std::fs::write(&snapshot_path, bytes)
            .context("Failed to write bundle snapshot")?;
    } else {
        // Save the current binary
        let current_bin = bin_dir.join(project_name);
        if current_bin.exists() {
            std::fs::copy(&current_bin, &snapshot_path)
                .context("Failed to copy binary snapshot")?;
        } else {
            std::fs::write(&snapshot_path, bytes)
                .context("Failed to write binary snapshot")?;
        }
    }

    Ok(snapshot_path.to_string_lossy().to_string())
}

/// Delete old snapshot files from disk.
pub fn delete_snapshots(paths: &[String]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

// ─────────────────────────────────────────────
// Process spawn / kill
// ─────────────────────────────────────────────

/// Spawn a project's binary as a child process with the correct environment variables.
pub async fn spawn_process(
    config: &Config,
    project: &Project,
    manager: &Arc<Mutex<ProcessManager>>,
) -> Result<u32> {
    let bin_dir = config.project_bin_dir(&project.name);
    let workspace_dir = config.project_workspace_dir(&project.name);
    let data_dir = config.project_data_dir(&project.name);

    std::fs::create_dir_all(&data_dir)
        .context("Failed to create project data directory")?;

    // Find the binary: in bin/ directory
    let binary_path = find_binary(&bin_dir)?;

    // Rotate log if it's grown too large
    let log_path = config.project_log_path(&project.name);
    rotate_log_if_needed(&log_path);

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open log file: {}", log_path.display()))?;
    let log_file_err = log_file.try_clone().context("Failed to clone log file handle")?;

    // Build environment
    let db_url = format!("sqlite://{}", data_dir.join("app.db").display());
    let s3_port = (project.port as u16) + 10000;
    let s3_endpoint = format!("http://127.0.0.1:{}", s3_port);
    let bucket_name = format!("bucket-{}", project.name);

    let s3_secret_path = data_dir.join(".s3_secret");
    let s3_secret = if s3_secret_path.exists() {
        std::fs::read_to_string(&s3_secret_path)
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    } else {
        let new_secret = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::write(&s3_secret_path, &new_secret);
        new_secret
    };

    let s3_access_key = project.id.clone();

    let s3_handle = crate::s3::start_project_s3(
        config,
        &project.id,
        s3_port,
        &s3_access_key,
        &s3_secret,
    ).await.context("Failed to start isolated S3 server")?;

    let mut child_cmd = Command::new(&binary_path);

    // If a workspace directory exists (bundle deploy), run from there
    if workspace_dir.exists() {
        child_cmd.current_dir(&workspace_dir);
    }

    child_cmd
        .env("DATABASE_URL", &db_url)
        .env("S3_ENDPOINT", &s3_endpoint)
        .env("S3_BUCKET", &bucket_name)
        .env("AWS_ACCESS_KEY_ID", &s3_access_key)
        .env("AWS_SECRET_ACCESS_KEY", &s3_secret)
        .env("AWS_REGION", "us-east-1")
        .env("PORT", project.port.to_string())
        .env("APP_NAME", &project.name);

    let env_path = data_dir.join(".env");
    if let Ok(env_content) = std::fs::read_to_string(&env_path) {
        for line in env_content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim();
                if key != "PORT" {
                    child_cmd.env(key, v.trim());
                }
            }
        }
    }

    // Apply RAM limit on Linux via setrlimit (RLIMIT_AS = virtual address space)
    #[cfg(target_os = "linux")]
    if let Some(limit_mb) = project.ram_limit_mb {
        let limit_bytes = (limit_mb as u64) * 1024 * 1024;
        unsafe {
            child_cmd.pre_exec(move || {
                let rlim = libc::rlimit {
                    rlim_cur: limit_bytes,
                    rlim_max: limit_bytes,
                };
                libc::setrlimit(libc::RLIMIT_AS, &rlim);
                Ok(())
            });
        }
    }

    let child = child_cmd
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_file_err))
        .spawn()
        .with_context(|| format!("Failed to spawn binary: {}", binary_path.display()))?;

    let pid = child.id().context("Could not get child PID")?;

    info!(
        "▶️  Spawned project '{}' (pid={}, port={})",
        project.name, pid, project.port
    );

    {
        let mut mgr = manager.lock().await;
        mgr.pids.insert(project.id.clone(), pid);
        mgr.start_times.insert(project.id.clone(), std::time::Instant::now());
        mgr.processes.insert(project.id.clone(), child);
        mgr.s3_processes.insert(project.id.clone(), s3_handle);
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
        mgr.s3_processes.remove(project_id);
        mgr.pids.remove(project_id);
        mgr.start_times.remove(project_id);
        info!("⏹️  Killed process and S3 server for project: {}", project_id);
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
            return Ok(path);
        }
    }

    anyhow::bail!("No binary found in: {}", bin_dir.display())
}
