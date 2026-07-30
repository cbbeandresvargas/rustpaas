use axum::{
    extract::{FromRequestParts, State},
    http::{request::Parts, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Form,
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::Utc;
use serde::Deserialize;
use tracing::{error, info};

use crate::api::AppState;
use crate::db::models::{Session, User};
use crate::dashboard::templates::LoginTemplate;

pub const SESSION_COOKIE_NAME: &str = "rustpaas_session";

// ─────────────────────────────────────────────
// Middleware / Extractor
// ─────────────────────────────────────────────

/// Extractor that requires a valid session cookie.
/// If valid, it provides the User object to the handler.
/// If invalid, it returns an error response (or we can make it redirect).
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let session_id = jar.get(SESSION_COOKIE_NAME).map(|c| c.value().to_string());

        if let Some(sid) = session_id {
            // Check session in DB
            let query = "
                SELECT u.* FROM users u 
                JOIN sessions s ON u.id = s.user_id 
                WHERE s.id = ? AND s.expires_at > ?
            ";
            
            let user: Option<User> = sqlx::query_as(query)
                .bind(&sid)
                .bind(Utc::now())
                .fetch_optional(&state.db)
                .await
                .map_err(|e| {
                    error!("Session DB error: {}", e);
                    (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response()
                })?;

            if let Some(u) = user {
                return Ok(AuthUser(u));
            }
        }

        // Redirect to login if not authenticated
        Err(Redirect::to("/login").into_response())
    }
}

/// Optional auth extractor (doesn't reject, just returns Option<User>)
/// Useful for the landing page or navbar rendering
pub struct OptionalAuthUser(pub Option<User>);

impl FromRequestParts<AppState> for OptionalAuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let session_id = jar.get(SESSION_COOKIE_NAME).map(|c| c.value().to_string());

        if let Some(sid) = session_id {
            let query = "
                SELECT u.* FROM users u 
                JOIN sessions s ON u.id = s.user_id 
                WHERE s.id = ? AND s.expires_at > ?
            ";
            
            if let Ok(Some(u)) = sqlx::query_as::<_, User>(query)
                .bind(&sid)
                .bind(Utc::now())
                .fetch_optional(&state.db)
                .await 
            {
                return Ok(OptionalAuthUser(Some(u)));
            }
        }
        
        Ok(OptionalAuthUser(None))
    }
}

// ─────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginPayload {
    pub username: String,
    pub password: String,
}

pub async fn login_page() -> Response {
    let tmpl = LoginTemplate { error: None };
    Html(askama::Template::render(&tmpl).unwrap()).into_response()
}

pub async fn login_post(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(payload): Form<LoginPayload>,
) -> Response {
    // 1. Find user by username
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);

    let error_msg;

    if let Some(u) = user {
        // 2. Verify password
        if u.verify_password(&payload.password) {
            // 3. Create session
            let session = Session::new(&u.id);
            if let Err(e) = session.insert(&state.db).await {
                error!("Failed to create session: {}", e);
                error_msg = Some("Internal error".to_string());
            } else {
                // 4. Set cookie
                let cookie = Cookie::build((SESSION_COOKIE_NAME, session.id.clone()))
                    .path("/")
                    .http_only(true)
                    .secure(false) // Set true in production with HTTPS
                    .same_site(axum_extra::extract::cookie::SameSite::Lax)
                    .build();
                
                info!("User '{}' logged in successfully", u.username);
                return (jar.add(cookie), Redirect::to("/dashboard")).into_response();
            }
        } else {
            error_msg = Some("Contraseña incorrecta".to_string());
        }
    } else {
        error_msg = Some("Usuario no encontrado".to_string());
    }

    // Return to login page with error
    let tmpl = LoginTemplate { error: error_msg };
    Html(askama::Template::render(&tmpl).unwrap()).into_response()
}

pub async fn logout_post(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Response {
    if let Some(cookie) = jar.get(SESSION_COOKIE_NAME) {
        // Delete session from DB
        let _ = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(cookie.value())
            .execute(&state.db)
            .await;
    }
    
    // Remove cookie
    let mut remove_cookie = Cookie::new(SESSION_COOKIE_NAME, "");
    remove_cookie.set_path("/");
    remove_cookie.set_http_only(true);
    
    (jar.remove(remove_cookie), Redirect::to("/")).into_response()
}
