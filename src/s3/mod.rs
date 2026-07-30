use anyhow::{Context, Result};
use tracing::info;

use crate::config::Config;

/// Starts the embedded S3 server using s3s-fs.
///
/// Returns a handle that keeps the server alive. Drop it to stop the server.
pub async fn start_server(config: &Config) -> Result<S3Handle> {
    use s3s_fs::FileSystem;
    use s3s::service::S3ServiceBuilder;

    let storage_dir = config.storage_dir();
    std::fs::create_dir_all(&storage_dir)
        .context("Failed to create S3 storage directory")?;

    info!("🗂️  S3 storage root: {}", storage_dir.display());

    // FileSystem-backed S3 service
    let fs = FileSystem::new(&storage_dir)
        .map_err(|e| anyhow::anyhow!("Failed to initialize S3 FileSystem: {:?}", e))?;

    // Build the S3 service and convert to SharedS3Service (which is Clone)
    let service = S3ServiceBuilder::new(fs).build().into_shared();

    // Bind to configured port
    let addr = format!("0.0.0.0:{}", config.s3_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind S3 server on {}", addr))?;

    info!("🪣  S3 server listening on http://{}", addr);

    // Spawn the S3 server in the background using hyper
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let svc = service.clone();
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
                Err(e) => {
                    tracing::error!("S3 accept error: {}", e);
                    break;
                }
            }
        }
    });

    Ok(S3Handle { _task: handle })
}

/// Handle to the S3 background task. Drop to stop (best-effort).
pub struct S3Handle {
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for S3Handle {
    fn drop(&mut self) {
        self._task.abort();
    }
}

/// Creates a new S3 bucket (directory) for a project.
pub fn create_bucket(config: &Config, bucket_name: &str) -> Result<()> {
    let bucket_path = config.storage_dir().join(bucket_name);
    std::fs::create_dir_all(&bucket_path)
        .with_context(|| format!("Failed to create bucket directory: {}", bucket_path.display()))?;
    info!("🪣  Created bucket: {}", bucket_name);
    Ok(())
}

/// Lists all objects (files) in a bucket directory.
pub fn list_bucket_objects(config: &Config, bucket_name: &str) -> Result<Vec<S3Object>> {
    let bucket_path = config.storage_dir().join(bucket_name);
    let mut objects = Vec::new();

    if !bucket_path.exists() {
        return Ok(objects);
    }

    collect_objects(&bucket_path, &bucket_path, &mut objects)?;
    Ok(objects)
}

fn collect_objects(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<S3Object>) -> Result<()> {
    for entry in std::fs::read_dir(dir).context("Failed to read bucket directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_objects(base, &path, out)?;
        } else {
            let key = path.strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"); // Normalize Windows paths
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(S3Object { key, size });
        }
    }
    Ok(())
}

/// Deletes a bucket and all its contents.
pub fn delete_bucket(config: &Config, bucket_name: &str) -> Result<()> {
    let bucket_path = config.storage_dir().join(bucket_name);
    if bucket_path.exists() {
        std::fs::remove_dir_all(&bucket_path)
            .with_context(|| format!("Failed to delete bucket: {}", bucket_name))?;
        info!("🗑️  Deleted bucket: {}", bucket_name);
    }
    Ok(())
}

/// Represents a single object in an S3 bucket
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct S3Object {
    pub key: String,
    pub size: u64,
}
