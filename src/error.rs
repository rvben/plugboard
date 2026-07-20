use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::redact::scrub_credentials;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A `switchkit::Error` rendered to text. Scrubbed of `user=`/`password=`
    /// query-string values at construction (see `From<switchkit::Error>`
    /// below): a vendor client's error variants may embed the device's
    /// request URL, and any transport-level error (timeout, connection
    /// refused, DNS) can carry it too, so an unscrubbed message could leak a
    /// device's plaintext password.
    #[error("{0}")]
    Core(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Internal(String),
}

impl From<switchkit::Error> for AppError {
    fn from(e: switchkit::Error) -> Self {
        AppError::Core(scrub_credentials(&e.to_string()))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            // Already scrubbed at construction (`From<switchkit::Error>` above);
            // scrub again here regardless, so the response body a client sees is
            // guaranteed clean even if a future `AppError::Core(...)` call site is
            // ever added that bypasses that `From` impl.
            AppError::Core(m) => (StatusCode::BAD_GATEWAY, scrub_credentials(m)),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        (status, msg).into_response()
    }
}
