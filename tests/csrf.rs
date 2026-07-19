//! CSRF + same-origin enforcement (Task 6a). The toggle/write routes do not
//! exist yet, so this test builds a SELF-CONTAINED router: a GET `/` that
//! returns the session's CSRF token in the body, and a dummy `POST /_probe`
//! (200 OK) guarded the same way every future write route will be, under
//! `session_layer(false)` + `csrf_and_origin`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Router, middleware};
use http_body_util::BodyExt;
use tower::ServiceExt;

use tasmota_web::auth::{Csrf, csrf_and_origin, session_layer};

async fn probe_get(csrf: Csrf) -> String {
    csrf.0
}

async fn probe_post() -> StatusCode {
    StatusCode::OK
}

fn app() -> Router {
    Router::new()
        .route("/", get(probe_get))
        .route("/_probe", post(probe_post))
        .layer(middleware::from_fn(csrf_and_origin))
        .layer(session_layer(false))
}

/// A POST with neither an `X-CSRF-Token` header nor any origin headers must
/// be rejected: there is no session token to match against, so the
/// same-origin fallback (no `Origin` header -> "rely on CSRF token") cannot
/// save it.
#[tokio::test]
async fn post_without_token_or_origin_is_rejected() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_probe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// A POST carrying a cross-origin `Origin` header must be rejected outright,
/// before the CSRF token is even checked.
#[tokio::test]
async fn post_with_cross_origin_header_is_rejected() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_probe")
                .header("origin", "https://evil.example.com")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// The happy path: GET `/` establishes a session and returns its CSRF token,
/// then a same-origin POST carrying that token in `X-CSRF-Token` (plus the
/// session cookie) is allowed through.
#[tokio::test]
async fn post_with_valid_token_and_same_origin_is_allowed() {
    let app = app();

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);

    let cookie = get_response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("GET / should set a session cookie")
        .to_str()
        .unwrap()
        .to_string();
    // Keep only the cookie's name=value pair (drop Path=/, HttpOnly, etc.)
    // so it round-trips as a request `Cookie` header.
    let cookie = cookie.split(';').next().unwrap().to_string();

    let token = get_response.into_body().collect().await.unwrap().to_bytes();
    let token = String::from_utf8(token.to_vec()).unwrap();
    assert!(!token.is_empty(), "GET / should return a non-empty token");

    let post_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/_probe")
                .header("cookie", cookie)
                .header("x-csrf-token", token)
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(post_response.status(), StatusCode::OK);
}
