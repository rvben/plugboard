pub mod dashboard;
pub mod events;

use axum::{
    Router, middleware,
    routing::{get, post},
};

use crate::assets_route;
use crate::state::AppState;

/// Builds the app router. `secure` sets the session cookie's `Secure`
/// attribute (see `AuthConfig::cookie_secure`); pass `false` only for a
/// trusted plain-http deployment (or in tests using a plain-http transport).
///
/// Three-tier routing: `/assets/:file` is public static content and is
/// merged in OUTSIDE the session + CSRF/same-origin layers (it needs
/// neither). Every other route (the dashboard, `/device/:id/toggle`,
/// `/devices/power` and every future write route) sits under
/// `session_layer` + `csrf_and_origin`, so every write route inherits CSRF
/// protection automatically. There is deliberately no separate
/// confirm-bypass route: `/device/:id/toggle` and `/devices/power` are the
/// only paths that ever execute a power command.
pub fn router(state: AppState, secure: bool) -> Router {
    Router::new()
        .route("/", get(dashboard::index))
        .route("/events", get(events::stream))
        .route("/device/:id/toggle", post(dashboard::toggle))
        .route("/devices/power", post(dashboard::bulk_power))
        .route("/modal/close", get(dashboard::modal_close))
        .layer(middleware::from_fn(crate::auth::csrf_and_origin))
        .layer(crate::auth::session_layer(secure))
        .with_state(state)
        .merge(Router::new().route("/assets/:file", get(assets_route::serve)))
}
