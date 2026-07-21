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
    let (poll_secs, settings) = {
        let config = state.inner.config.read().await;
        let has_credential = config
            .devices
            .iter()
            .find(|d| crate::fleet::device_id(&d.host) == id)
            .is_some_and(|d| d.password.is_some());
        let mut group_names: Vec<String> = config
            .devices
            .iter()
            .filter_map(|d| d.group.as_deref())
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect();
        group_names.sort_unstable();
        group_names.dedup();
        (
            config.poll_interval_secs,
            device::SettingsCtx {
                has_credential,
                group_names,
            },
        )
    };
    let series = crate::history::snapshot(&state.inner.history);
    let upds = crate::updates::snapshot(&state.inner.updates);
    let fleet = state.inner.fleet.read().await;
    let dev = fleet
        .get(&id)
        .ok_or_else(|| AppError::NotFound(format!("Device {id} is not configured.")))?;
    Ok(layout::page(
        dev.display_name(),
        &csrf.0,
        chrome,
        device::device_page(dev, poll_secs, &series, upds.get(&id), &settings),
    ))
}
