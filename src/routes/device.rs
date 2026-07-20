use axum::extract::{Path, State};
use maud::Markup;

use crate::auth::Csrf;
use crate::error::AppError;
use crate::state::AppState;
use crate::views::{device, layout};

/// `GET /device/:id` - the full-status detail page for one device. An
/// unknown id (never configured, or removed from config since the page was
/// linked) is a 404, never a silently empty or default-valued page.
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    csrf: Csrf,
) -> Result<Markup, AppError> {
    let chrome = layout::Chrome {
        active: layout::Nav::Devices,
        show_logout: state.builtin_auth().await,
    };
    let poll_secs = state.inner.config.read().await.poll_interval_secs;
    let fleet = state.inner.fleet.read().await;
    let dev = fleet
        .get(&id)
        .ok_or_else(|| AppError::NotFound(id.clone()))?;
    Ok(layout::page(
        dev.display_name(),
        &csrf.0,
        chrome,
        device::device_page(dev, poll_secs),
    ))
}
