use axum::extract::State;
use maud::Markup;

use crate::auth::Csrf;
use crate::state::AppState;
use crate::views::{dashboard, layout};

pub async fn index(State(state): State<AppState>, csrf: Csrf) -> Markup {
    let fleet = state.inner.fleet.read().await;
    layout::page("Dashboard", &csrf.0, dashboard::dashboard_page(&fleet))
}
