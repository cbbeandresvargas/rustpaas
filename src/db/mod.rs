use anyhow::{Context, Result};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use tracing::info;

use crate::config::Config;

pub mod models;

/// Initialize the SQLite connection pool and run all migrations.
pub async fn init_pool(config: &Config) -> Result<SqlitePool> {
    // SQLite URL — create the file if it doesn't exist
    let db_url = format!("sqlite://{}?mode=rwc", config.db_path().display());

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .with_context(|| format!("Failed to connect to SQLite at {}", config.db_path().display()))?;

    // Run migrations embedded in the binary
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("Failed to run database migrations")?;

    info!("✅ Database migrations applied successfully");
    Ok(pool)
}
