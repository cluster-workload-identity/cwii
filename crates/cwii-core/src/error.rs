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
        tracing::warn!(error = %self, "webhook request failed");
        let status = match &self {
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}
