use axum::{
    body::Body,
    extract::State,
    http::{Request, Response, StatusCode},
    response::IntoResponse,
};
use tracing::{debug, warn};

use crate::api::AppState;
use crate::db::models::{Project, ProjectStatus};
use crate::runner;

/// Axum handler that acts as a dynamic reverse proxy.
///
/// Reads the `Host` header from the request, extracts the subdomain,
/// looks up the project in the DB, and forwards the request to that project's local port.
///
/// If the project is suspended, it is reactivated first (scale-from-zero).
pub async fn proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> impl IntoResponse {
    // Extract host from headers
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Host without port for custom domain matching
    let host_clean = host.split(':').next().unwrap_or(&host).to_string();

    // 1. Try custom domain match first
    let project = match Project::find_by_custom_domain(&state.db, &host_clean).await {
        Ok(Some(p)) => {
            debug!("Proxy request matched custom domain: {}", host_clean);
            p
        }
        Ok(None) => {
            // 2. Fall back to subdomain matching
            let subdomain = match extract_subdomain(&host, &state.config.domain) {
                Some(s) => s.to_string(),
                None => {
                    return (StatusCode::NOT_FOUND, "No subdomain found").into_response();
                }
            };

            debug!("Proxy request for subdomain: {}", subdomain);

            match Project::find_by_subdomain(&state.db, &subdomain).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    warn!("No project found for subdomain: {}", subdomain);
                    return (StatusCode::NOT_FOUND, format!("App '{}' not found", subdomain)).into_response();
                }
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response();
                }
            }
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response();
        }
    };

    // If suspended or stopped, try to restart (scale-from-zero)
    let is_running = {
        let mut mgr = state.process_manager.lock().await;
        mgr.is_running(&project.id)
    };

    if !is_running && matches!(project.status(), ProjectStatus::Running | ProjectStatus::Suspended) {
        if let Err(e) = runner::spawn_process(&state.config, &project, &state.process_manager).await {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Failed to start app '{}': {}", project.name, e),
            )
                .into_response();
        }
        // Brief wait for the process to initialize
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // Record activity for the auto-suspend timer
    {
        let mut mgr = state.process_manager.lock().await;
        mgr.record_activity(&project.id);
    }

    // Forward the request to the app's local port
    let uri_str = req.uri().to_string();
    let target_url = format!("http://127.0.0.1:{}{}", project.port, uri_str);

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build_http::<Body>();

    // Build forwarded request
    let (parts, body) = req.into_parts();
    let mut builder = Request::builder()
        .method(&parts.method)
        .uri(&target_url);

    for (key, value) in &parts.headers {
        builder = builder.header(key, value);
    }

    let forwarded_req = match builder.body(body) {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Request build error: {}", e))
                .into_response();
        }
    };

    match client.request(forwarded_req).await {
        Ok(res) => {
            let (res_parts, res_body) = res.into_parts();
            let bytes = match http_body_util::BodyExt::collect(res_body).await {
                Ok(b) => b.to_bytes(),
                Err(e) => {
                    return (StatusCode::BAD_GATEWAY, format!("Failed to read response: {}", e))
                        .into_response();
                }
            };
            Response::from_parts(res_parts, Body::from(bytes)).into_response()
        }
        Err(e) => {
            warn!("Proxy error for '{}': {}", project.name, e);
            (
                StatusCode::BAD_GATEWAY,
                format!("App '{}' is not responding: {}", project.name, e),
            )
                .into_response()
        }
    }
}

/// Returns true if the request's Host header contains a subdomain of `domain`.
/// Used by the middleware to decide whether to route to the proxy.
pub fn has_app_subdomain(req: &Request<Body>, domain: &str) -> bool {
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    extract_subdomain(host, domain).is_some()
}

/// Extract subdomain from host string.
/// e.g., "myapp.mypaas.com" with domain "mypaas.com" → Some("myapp")
/// e.g., "myapp.localhost" with domain "localhost" → Some("myapp")
pub fn extract_subdomain<'a>(host: &'a str, domain: &str) -> Option<&'a str> {
    // Remove port if present
    let host = host.split(':').next().unwrap_or(host);

    let suffix = format!(".{}", domain);
    if host.ends_with(&suffix) {
        let subdomain = &host[..host.len() - suffix.len()];
        if subdomain.is_empty() { None } else { Some(subdomain) }
    } else {
        None
    }
}
