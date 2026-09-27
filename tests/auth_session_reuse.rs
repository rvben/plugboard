//! `tower_sessions::MemoryStore` has no background sweep: an abandoned
//! session (one whose cookie is never presented again) sits in the map until
//! the process restarts. That is only a leak if something regularly mints a
//! NEW session for what should be the SAME returning client. This sends
//! `GET /` through the real router repeatedly, presenting the exact
//! `Set-Cookie` value the previous response returned, and checks that the
//! session id embedded in the cookie stays constant (one client, one session
//! record) rather than changing on every request (growth proportional to
//! request volume, not client count).

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use std::path::PathBuf;
use tower::ServiceExt;

use plugboard::config::Config;
use plugboard::routes;
use plugboard::state::AppState;

#[tokio::test]
async fn returning_cookie_reuses_the_same_session_id() {
    let state = AppState::new(Config::default(), PathBuf::from("unused.toml"));
    let app = routes::router(state, false);

    let mut cookie: Option<String> = None;
    let mut session_ids = std::collections::HashSet::new();

    for _ in 0..25 {
        let mut req = Request::builder().method("GET").uri("/");
        if let Some(c) = &cookie {
            req = req.header(header::COOKIE, c.clone());
        }
        let response = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        if let Some(set_cookie) = response.headers().get(header::SET_COOKIE) {
            let value = set_cookie.to_str().unwrap().to_string();
            let session_id = value.split(';').next().unwrap().to_string();
            session_ids.insert(session_id.clone());
            cookie = Some(session_id);
        }
    }

    assert_eq!(
        session_ids.len(),
        1,
        "a client that presents its cookie on every request must reuse the SAME session id \
         throughout; got {} distinct ids over 25 requests, which means every request mints a \
         fresh (and then immediately abandoned) session",
        session_ids.len()
    );
}
