pub mod dashboard;

use axum::{Router, routing::get};

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard::index))
        .with_state(state)
}
