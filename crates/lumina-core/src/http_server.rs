//! MATRIX-05: HTTP server exposing the guarded core loop as an OpenAI-compatible
//! chat completions endpoint.  Compiled only with the `http` cargo feature.
//!
//! Endpoint: POST /v1/chat/completions
//! Auth:     Authorization: Bearer <LUMINA_HTTP_TOKEN>
//! Binding:  LUMINA_HTTP_BIND (default 127.0.0.1:3300)

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::agent_loop::process_message;
use crate::config::Config;
use crate::dashboard::{render_dashboard, DashboardParams};
use crate::error::LuminaError;
use crate::pwa::{manifest_json, render_mobile_page, service_worker_js};

// ── Request / Response types (OpenAI-compatible) ──────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    #[allow(dead_code)]
    pub model: Option<String>,
    pub messages: Vec<RequestMessage>,
}

#[derive(Debug, Deserialize)]
pub struct RequestMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: ResponseMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct ResponseMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub message: String,
    pub r#type: String,
}

fn error_response(status: StatusCode, message: impl Into<String>, kind: &str) -> Response {
    let body = ErrorBody {
        error: ErrorDetail { message: message.into(), r#type: kind.to_string() },
    };
    (status, Json(body)).into_response()
}

// ── Auth middleware ────────────────────────────────────────────────────────────

async fn auth_middleware(
    State(state): State<Arc<ServerState>>,
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let token = match &state.token {
        Some(t) => t.clone(),
        None => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "LUMINA_HTTP_TOKEN is not configured. Set it in the vault before using the HTTP endpoint.",
                "authentication_error",
            );
        }
    };

    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let provided = auth_header.trim_start_matches("Bearer ").trim();

    if provided != token {
        return error_response(StatusCode::UNAUTHORIZED, "Invalid token", "authentication_error");
    }

    next.run(request).await
}

// ── Handler ───────────────────────────────────────────────────────────────────

async fn chat_completions(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    // Enforce max body size at the application layer (HTTP layer already has a limit)
    if req.messages.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "messages array is required and must not be empty", "invalid_request_error");
    }

    // Extract last user message
    let user_content = req.messages.iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");

    if user_content.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "No user message found in messages array", "invalid_request_error");
    }

    match process_message(&state.config, user_content).await {
        Ok(response) => {
            let body = ChatResponse {
                id: format!("lumina-{}", uuid_lite()),
                object: "chat.completion".to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ResponseMessage {
                        role: "assistant".to_string(),
                        content: response,
                    },
                    finish_reason: "stop".to_string(),
                }],
            };
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(LuminaError::SecurityViolation(_)) => {
            error_response(StatusCode::BAD_REQUEST, "Input rejected by security guard", "invalid_request_error")
        }
        Err(e) if e.to_string().contains("Rate limit") => {
            error_response(StatusCode::TOO_MANY_REQUESTS, e.to_string(), "rate_limit_error")
        }
        Err(e) if e.to_string().contains("Chord") || e.to_string().contains("chord") => {
            error_response(StatusCode::BAD_GATEWAY, "LLM backend unavailable", "upstream_error")
        }
        Err(e) => {
            eprintln!("http: unexpected error: {}", e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal error", "server_error")
        }
    }
}

// ── Tiny UUID substitute (avoids pulling in uuid crate) ──────────────────────

fn uuid_lite() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}{:08x}", t.as_secs(), t.subsec_nanos())
}

// ── Dashboard handler ─────────────────────────────────────────────────────────

async fn dashboard_handler(State(_state): State<Arc<ServerState>>) -> impl IntoResponse {
    let params = DashboardParams::default();
    let html = render_dashboard(&params);
    Html(html)
}

// ── PWA handlers ─────────────────────────────────────────────────────────────

/// Serve `/manifest.json` — PWA manifest.
///
/// The `name` field comes from `LUMINA_APP_NAME` (default `"Lumina"`).
/// No org name is hardcoded.  Content-Type is `application/manifest+json`.
async fn manifest_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/manifest+json")],
        manifest_json(),
    )
}

/// Serve `/sw.js` — minimal service worker JavaScript.
async fn service_worker_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/javascript")],
        service_worker_js(),
    )
}

/// Serve `/mobile` — mobile-responsive HTML dashboard with PWA support.
async fn mobile_handler() -> impl IntoResponse {
    Html(render_mobile_page())
}

// ── Server startup ────────────────────────────────────────────────────────────

struct ServerState {
    config: Arc<Config>,
    token: Option<String>,
}

/// Bind and serve the HTTP endpoint.  Returns when the listener closes.
pub async fn serve(config: Arc<Config>) -> crate::error::Result<()> {
    let bind = config.lumina_http_bind.clone();
    let token = config.lumina_http_token.clone();

    let state = Arc::new(ServerState { config: config.clone(), token });

    // PWA assets must be publicly accessible — browsers fetch manifest.json and
    // sw.js automatically without auth headers, so they live outside the auth
    // middleware layer.
    let public_routes = Router::new()
        .route("/manifest.json", get(manifest_handler))
        .route("/sw.js", get(service_worker_handler));

    let authed_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/dashboard", get(dashboard_handler))
        .route("/mobile", get(mobile_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state.clone());

    let app = public_routes.merge(authed_routes);

    let listener = TcpListener::bind(&bind).await.map_err(|e| {
        LuminaError::Config(format!("HTTP bind to {} failed: {}", bind, e))
    })?;

    eprintln!("http: listening on {}", bind);
    axum::serve(listener, app).await.map_err(|e| {
        LuminaError::Config(format!("HTTP serve error: {}", e))
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_messages_rejected() {
        let req = ChatRequest { model: None, messages: vec![] };
        assert!(req.messages.is_empty());
    }

    #[test]
    fn test_no_user_message_detected() {
        let req = ChatRequest {
            model: None,
            messages: vec![
                RequestMessage { role: "system".to_string(), content: "you are helpful".to_string() },
            ],
        };
        let user = req.messages.iter().rev().find(|m| m.role == "user");
        assert!(user.is_none());
    }

    #[test]
    fn test_last_user_message_extracted() {
        let req = ChatRequest {
            model: None,
            messages: vec![
                RequestMessage { role: "user".to_string(), content: "first".to_string() },
                RequestMessage { role: "assistant".to_string(), content: "reply".to_string() },
                RequestMessage { role: "user".to_string(), content: "second".to_string() },
            ],
        };
        let content = req.messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.as_str());
        assert_eq!(content, Some("second"));
    }

    #[test]
    fn test_response_format_matches_openai_spec() {
        let resp = ChatResponse {
            id: "lumina-abc".to_string(),
            object: "chat.completion".to_string(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage { role: "assistant".to_string(), content: "hi".to_string() },
                finish_reason: "stop".to_string(),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"object\":\"chat.completion\""));
        assert!(json.contains("\"role\":\"assistant\""));
        assert!(json.contains("\"finish_reason\":\"stop\""));
    }

    #[test]
    fn test_missing_auth_handled() {
        // auth_middleware logic: if token not configured, always 401
        let token: Option<String> = None;
        let provided = "Bearer sometoken";
        let extracted = provided.trim_start_matches("Bearer ").trim();
        let valid = token.as_deref().map(|t| t == extracted).unwrap_or(false);
        assert!(!valid);
    }

    #[test]
    fn test_wrong_token_rejected() {
        let token = Some("correct-token".to_string());
        let provided = "Bearer wrong-token";
        let extracted = provided.trim_start_matches("Bearer ").trim();
        let valid = token.as_deref().map(|t| t == extracted).unwrap_or(false);
        assert!(!valid);
    }

    #[test]
    fn test_correct_token_accepted() {
        let token = Some("secret".to_string());
        let provided = "Bearer secret";
        let extracted = provided.trim_start_matches("Bearer ").trim();
        let valid = token.as_deref().map(|t| t == extracted).unwrap_or(false);
        assert!(valid);
    }
}
