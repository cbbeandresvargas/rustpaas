use askama::Template;
use aws_config::BehaviorVersion;
use aws_sdk_s3::{Client, primitives::ByteStream};
use axum::{
    extract::{Multipart, State},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
    Router,
};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use std::{env, sync::Arc};
use tracing::{info, error};

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    s3: Client,
    bucket: String,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    messages: Vec<Message>,
    files: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct Message {
    id: i64,
    content: String,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();
    
    info!("Starting test app...");

    // 1. Setup SQLite
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://app.db?mode=rwc".to_string());
    let db = SqlitePoolOptions::new()
        .connect(&db_url)
        .await
        .expect("Failed to connect to SQLite");
        
    sqlx::query("CREATE TABLE IF NOT EXISTS messages (id INTEGER PRIMARY KEY, content TEXT)")
        .execute(&db)
        .await
        .expect("Failed to create table");

    // 2. Setup S3
    let endpoint = env::var("S3_ENDPOINT").expect("S3_ENDPOINT not set");
    let bucket = env::var("S3_BUCKET").expect("S3_BUCKET not set");
    
    // Create S3 client configuration overrides (force path style for local S3)
    let sdk_config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
        .endpoint_url(endpoint)
        .force_path_style(true)
        .build();
    let s3 = Client::from_conf(s3_config);
    
    // Create bucket if it doesn't exist
    if let Err(e) = s3.create_bucket().bucket(&bucket).send().await {
        info!("Bucket creation skipped/failed: {}", e);
    }

    let state = AppState { db, s3, bucket };

    let app = Router::new()
        .route("/", get(index))
        .route("/message", post(add_message))
        .route("/upload", post(upload_file))
        .with_state(state);

    let port = env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{}", port);
    
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    info!("Listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let messages = sqlx::query_as::<_, Message>("SELECT id, content FROM messages")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        
    let mut files = vec![];
    if let Ok(res) = state.s3.list_objects_v2().bucket(&state.bucket).send().await {
        for obj in res.contents() {
            if let Some(key) = obj.key() {
                files.push(key.to_string());
            }
        }
    }

    let tmpl = IndexTemplate { messages, files };
    Html(tmpl.render().unwrap())
}

async fn add_message(State(state): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    while let Some(field) = multipart.next_field().await.unwrap_or(None) {
        if field.name() == Some("content") {
            if let Ok(content) = field.text().await {
                let _ = sqlx::query("INSERT INTO messages (content) VALUES (?)")
                    .bind(content)
                    .execute(&state.db)
                    .await;
            }
        }
    }
    Redirect::to("/")
}

async fn upload_file(State(state): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    while let Some(field) = multipart.next_field().await.unwrap_or(None) {
        if field.name() == Some("file") {
            let filename = field.file_name().unwrap_or("unnamed").to_string();
            if let Ok(bytes) = field.bytes().await {
                let stream = ByteStream::from(bytes);
                let _ = state.s3.put_object()
                    .bucket(&state.bucket)
                    .key(filename)
                    .body(stream)
                    .send()
                    .await;
            }
        }
    }
    Redirect::to("/")
}
