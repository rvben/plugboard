pub mod admin;
pub mod dashboard;
pub mod device;
pub mod discover;
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
/// `/devices/power`, the Task 8 admin routes, and every future write route)
/// sits under `session_layer` + `csrf_and_origin`, so every write route
/// inherits CSRF protection automatically. There is deliberately no separate
/// confirm-bypass route for any admin action: each `/device/:id/...` admin
/// route is the only path that ever executes its operation, gated by the
/// SAME handler's own `confirmed=true` check.
pub fn router(state: AppState, secure: bool) -> Router {
    Router::new()
        .route("/", get(dashboard::index))
        .route("/events", get(events::stream))
        .route("/device/:id", get(device::detail))
        .route("/device/:id/toggle", post(dashboard::toggle))
        .route("/devices/power", post(dashboard::bulk_power))
        .route("/device/:id/console", post(admin::console))
        .route("/device/:id/config/get", post(admin::config_get))
        .route("/device/:id/config/set", post(admin::config_set))
        .route("/device/:id/firmware/check", post(admin::firmware_check))
        .route("/device/:id/firmware/update", post(admin::firmware_update))
        .route("/device/:id/backup", get(admin::backup))
        .route("/discover", get(discover::index))
        .route("/discover/scan", post(discover::scan))
        .route("/discover/add", post(discover::add))
        .route("/modal/close", get(dashboard::modal_close))
        .layer(middleware::from_fn(crate::auth::csrf_and_origin))
        .layer(crate::auth::session_layer(secure))
        .with_state(state)
        .merge(Router::new().route("/assets/:file", get(assets_route::serve)))
}
