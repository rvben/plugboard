use axum::extract::State;
use maud::Markup;

use crate::state::AppState;
use crate::views::{dashboard, layout};

pub async fn index(State(state): State<AppState>) -> Markup {
    let fleet = state.inner.fleet.read().await;
    layout::page("Dashboard", dashboard::dashboard_page(&fleet))
}
