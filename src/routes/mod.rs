pub mod admin;
pub mod auth;
pub mod dashboard;
pub mod device;
pub mod discover;
pub mod events;
pub mod settings;

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
/// Three tiers (Task 11):
/// - **assets** (`/assets/:file`): no session, no CSRF, no auth. Merged in
///   OUTSIDE every layer below - it needs none of them.
/// - **public auth** (`GET`/`POST /login`): session + CSRF (so `Csrf` works
///   and the login POST is CSRF-checked), but NOT `require_auth` - a
///   logged-out visitor must be able to reach the login form at all.
/// - **app routes** (everything else, including `POST /logout`): session +
///   CSRF + `require_auth`. Every write route inherits CSRF protection
///   automatically, and in `AuthMode::Builtin` every route here requires an
///   authenticated session.
///
/// Build order matters: `require_auth` is layered onto the app router BEFORE
/// it is merged with the public auth router (so `/login` is not gated by
/// it), then the combined router gets the shared session + CSRF layers, then
/// the asset router is merged in last, outside all of that. There is
/// deliberately no separate confirm-bypass route for any admin action: each
/// `/device/:id/...` admin route is the only path that ever executes its
/// operation, gated by the SAME handler's own `confirmed=true` check.
pub fn router(state: AppState, secure: bool) -> Router {
    let app_router = Router::new()
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
        .route("/settings", get(settings::index))
        .route("/settings/device/rename", post(settings::rename))
        .route("/settings/device/remove", post(settings::remove))
        .route("/settings/device/credentials", post(settings::credentials))
        .route("/settings/device/protected", post(settings::protected))
        .route("/settings/poll-interval", post(settings::poll_interval))
        .route("/modal/close", get(dashboard::modal_close))
        .route("/logout", post(auth::logout))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_auth,
        ));

    let public_auth_router =
        Router::new().route("/login", get(auth::login_get).post(auth::login_post));

    app_router
        .merge(public_auth_router)
        .layer(middleware::from_fn(crate::auth::csrf_and_origin))
        .layer(crate::auth::session_layer(secure))
        .with_state(state)
        .merge(Router::new().route("/assets/:file", get(assets_route::serve)))
}
