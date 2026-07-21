use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};

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

/// Largest error body this middleware will buffer to re-render. Real error
/// reasons are one line; anything bigger passes through untouched.
const MAX_ERROR_BODY: usize = 16 * 1024;

/// Content-negotiating error presentation: htmx requests keep their concise
/// plain-text error bodies (the toast layer shows them verbatim), while a
/// full-page browser navigation that errors - a mistyped URL, a stale link
/// to a removed device - gets the styled error page instead of bare text or
/// a blank window. Responses that already carry HTML (e.g. the login page's
/// own 429) pass through untouched.
pub async fn html_error_pages(req: Request, next: Next) -> Response {
    let is_htmx = req.headers().contains_key("hx-request");
    let response = next.run(req).await;
    let status = response.status();
    if is_htmx || !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if is_html {
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_ERROR_BODY).await else {
        // Body larger than any real error reason (or unreadable): give the
        // page shell with no detail rather than the original response, whose
        // body has been consumed.
        return (
            parts.status,
            Html(crate::views::layout::error_page(parts.status, "").into_string()),
        )
            .into_response();
    };
    let detail = String::from_utf8_lossy(&bytes);
    (
        parts.status,
        Html(crate::views::layout::error_page(parts.status, detail.trim()).into_string()),
    )
        .into_response()
}
