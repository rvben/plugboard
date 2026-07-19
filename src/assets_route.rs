//! Serves the embedded static assets (`app.css`, `htmx.min.js`, `sse.js`).
//!
//! Assets are compiled INTO the binary via `include_bytes!`, not read from disk
//! at runtime, so `tasmota-web` stays a single self-contained executable. These
//! routes are public static files: no session/CSRF/auth is applied to them.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

pub async fn serve(Path(file): Path<String>) -> Response {
    let (bytes, ctype): (&'static [u8], &str) = match file.as_str() {
        "app.css" => (include_bytes!("../assets/app.css"), "text/css"),
        "htmx.min.js" => (include_bytes!("../assets/htmx.min.js"), "text/javascript"),
        "sse.js" => (include_bytes!("../assets/sse.js"), "text/javascript"),
        "csrf.js" => (include_bytes!("../assets/csrf.js"), "text/javascript"),
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    ([(header::CONTENT_TYPE, ctype)], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use axum::extract::Path;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;

    use super::serve;

    async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn app_css_returns_ok_with_css_content_type() {
        let resp = serve(Path("app.css".to_string())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/css"
        );
        let bytes = body_bytes(resp).await;
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(".card"), "app.css should define .card");
    }

    #[tokio::test]
    async fn htmx_min_js_returns_ok_with_js_content_type() {
        let resp = serve(Path("htmx.min.js".to_string())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/javascript"
        );
        let bytes = body_bytes(resp).await;
        assert!(!bytes.is_empty(), "htmx.min.js must not be empty");
    }

    #[tokio::test]
    async fn sse_js_returns_ok_with_js_content_type() {
        let resp = serve(Path("sse.js".to_string())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/javascript"
        );
    }

    #[tokio::test]
    async fn csrf_js_returns_ok_with_js_content_type() {
        let resp = serve(Path("csrf.js".to_string())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/javascript"
        );
        let bytes = body_bytes(resp).await;
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.contains("X-CSRF-Token"),
            "csrf.js should set the X-CSRF-Token header on htmx requests"
        );
    }

    #[tokio::test]
    async fn unknown_file_returns_404() {
        let resp = serve(Path("does-not-exist.js".to_string())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
