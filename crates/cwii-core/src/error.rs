//! Webhook error type and its HTTP rendering.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kubernetes: {0}")]
    Kube(#[from] kube::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        // The detailed error is logged but never returned: 5xx responses expose only a generic
        // message so internal details (cluster errors, patch internals) don't leak to clients.
        tracing::warn!(error = %self, "webhook request failed");
        match self {
            Error::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response(),
        }
    }
}
