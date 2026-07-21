use std::io::Write;
use std::path::PathBuf;

use http_body_util::BodyExt;
use tower::ServiceExt;

/// Error presentation is content-negotiated: a browser navigation to a
/// missing page gets the styled HTML error page (never a blank window or
/// bare text), while an htmx request keeps its concise plain-text body for
/// the toast layer.
#[tokio::test]
async fn errors_render_html_pages_for_navigations_and_text_for_htmx() {
    let state = plugboard::state::AppState::new(
        plugboard::config::Config::default(),
        PathBuf::from("unused.toml"),
    );
    let app = plugboard::routes::router(state, false);

    let nav = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/no-such-page")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(nav.status(), axum::http::StatusCode::NOT_FOUND);
    let body = nav.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body.contains("<html") && body.contains("Not found"),
        "a navigation 404 must be a styled page, got: {body}"
    );

    let htmx = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/no-such-page")
                .header("hx-request", "true")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(htmx.status(), axum::http::StatusCode::NOT_FOUND);
    let body = htmx.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        !body.contains("<html"),
        "an htmx error must stay plain text for the toast layer, got: {body}"
    );
}

#[test]
fn config_default_has_loopback_bind() {
    let c = plugboard::config::Config::default();
    assert_eq!(c.poll_interval_secs, 5);
    assert!(c.bind.ip().is_loopback());
}

#[test]
fn config_load_round_trip() {
    let path = std::env::temp_dir().join(format!(
        "plugboard-smoke-{}-{}.toml",
        std::process::id(),
        line!()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    write!(
        f,
        r#"
bind = "127.0.0.1:9090"
poll_interval_secs = 10

[auth]
mode = "builtin"

[[devices]]
name = "plug1"
host = "10.0.0.5"
"#
    )
    .unwrap();
    drop(f);

    let loaded = plugboard::config::Config::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    assert_eq!(loaded.bind.port(), 9090);
    assert_eq!(loaded.poll_interval_secs, 10);
    assert_eq!(loaded.auth.mode, plugboard::config::AuthMode::Builtin);
    assert_eq!(loaded.devices.len(), 1);
    assert_eq!(loaded.devices[0].name, "plug1");
    assert_eq!(loaded.devices[0].host, "10.0.0.5");
}

#[test]
fn config_load_missing_file_returns_default() {
    let path = std::env::temp_dir().join(format!(
        "plugboard-smoke-missing-{}-{}.toml",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&path);

    let loaded = plugboard::config::Config::load(&path).unwrap();

    assert_eq!(loaded.poll_interval_secs, 5);
    assert!(loaded.devices.is_empty());
}
