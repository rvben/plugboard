pub mod dashboard;
pub mod events;

use axum::{Router, routing::get};

use crate::assets_route;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard::index))
        .route("/assets/:file", get(assets_route::serve))
        .route("/events", get(events::stream))
        .with_state(state)
}
