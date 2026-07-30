//! hello-app — Test binary for RustPaaS deploy validation
//!
//! Reads PORT, APP_NAME, DATABASE_URL, S3_ENDPOINT, S3_BUCKET from environment
//! (injected by the PaaS runner) and serves a simple HTTP response.

use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let port     = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let app_name = std::env::var("APP_NAME").unwrap_or_else(|_| "hello-app".to_string());
    let db_url   = std::env::var("DATABASE_URL").unwrap_or_else(|_| "(none)".to_string());
    let s3_ep    = std::env::var("S3_ENDPOINT").unwrap_or_else(|_| "(none)".to_string());
    let s3_bkt   = std::env::var("S3_BUCKET").unwrap_or_else(|_| "(none)".to_string());

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr)
        .unwrap_or_else(|e| panic!("Failed to bind on {}: {}", addr, e));

    eprintln!("🟢 {} listening on http://{}", app_name, addr);
    eprintln!("   DATABASE_URL = {}", db_url);
    eprintln!("   S3_ENDPOINT  = {}", s3_ep);
    eprintln!("   S3_BUCKET    = {}", s3_bkt);

    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                // Read the HTTP request (minimal parse — just enough to respond)
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);

                let body = format!(
                    r#"{{"app":"{app}","port":{port},"status":"running","env":{{"DATABASE_URL":"{db}","S3_ENDPOINT":"{s3}","S3_BUCKET":"{bkt}"}}}}"#,
                    app  = app_name,
                    port = port,
                    db   = db_url,
                    s3   = s3_ep,
                    bkt  = s3_bkt,
                );

                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    body.len(),
                    body
                );

                let _ = s.write_all(response.as_bytes());
            }
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }
}
