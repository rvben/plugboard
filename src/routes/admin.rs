//! Per-device admin panel routes: console, config get/set, firmware
//! check/update, and config backup download (Task 8).
//!
//! Every destructive action reuses `tasmota_core::guardrail::classify`
//! (identical classification to the CLI) so a device can never be reached
//! for a destructive or unclassifiable operation without an explicit
//! `confirmed=true` re-post. `config get`/`config set` additionally run
//! `validate_setting`, mirroring the CLI's own validator byte-for-byte, so a
//! setting name that smuggles a Backlog (`;`), a space-separated argument, or
//! a bare destructive command word is rejected with 400 before any network
//! I/O. `restore` is intentionally not wired to a route (see the admin panel
//! view): its upload endpoint is unverified against a live device.

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde::Deserialize;
use serde_json::Value;
use tasmota_core::guardrail::{self, Hazard};

use crate::error::AppError;
use crate::ops;
use crate::state::AppState;
use crate::views::components::{close_modal, confirm_modal};
use crate::views::device::admin_result;

/// Look up a device's `(host, display_name)` by id without doing any network
/// I/O, so a gated (unconfirmed) request can 404 on an unknown device while
/// still never touching the network.
async fn device_host_and_name(state: &AppState, id: &str) -> Result<(String, String), AppError> {
    let fleet = state.inner.fleet.read().await;
    let dev = fleet
        .get(id)
        .ok_or_else(|| AppError::NotFound(id.to_string()))?;
    Ok((dev.host.clone(), dev.display_name().to_string()))
}

/// Renders a device operation's raw JSON response, auto-escaped by maud like
/// every other device-controlled value in this crate (the console/config
/// response body is attacker-influenced, so it is never rendered raw).
fn result_block(value: &Value) -> Markup {
    html! { pre.admin-output { (value.to_string()) } }
}

/// Rejects a setting that is not a single bare command word: mirrors the
/// CLI's `validate_setting` (see `tasmota-cli/cli/src/commands.rs`) exactly,
/// so `config get`/`config set` refuse the same inputs the CLI refuses. An
/// empty setting, one containing whitespace or `;` (a smuggled Backlog or
/// argument), or one that itself classifies as `Hazard::Destructive` (a bare
/// `Reset`/`Upgrade`/`Module`/...) is rejected before any network I/O.
fn validate_setting(setting: &str) -> Result<(), AppError> {
    if setting.is_empty() || setting.chars().any(|c| c.is_whitespace() || c == ';') {
        return Err(AppError::BadRequest(format!(
            "`{setting}` is not a single setting name; use console (guarded) for commands \
             with arguments or Backlog"
        )));
    }
    if let Hazard::Destructive(reason) = guardrail::classify(setting) {
        return Err(AppError::BadRequest(format!(
            "refusing config on a destructive command ({reason}); use console, which guards it"
        )));
    }
    Ok(())
}

/// Sanitizes a device-controlled name into a safe `.dmp` filename stem: only
/// `[A-Za-z0-9._-]` survives (every other BYTE becomes `_`, so a multi-byte
/// UTF-8 character or a raw control byte like `\r`/`\n` can never leak into
/// the header), leading/trailing `.`/`_` are trimmed, the result is capped to
/// 64 bytes, and an empty result falls back to `tasmota-backup` - a hostile
/// or empty device name can never produce an empty or header-breaking
/// filename.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .bytes()
        .map(|b| {
            let c = b as char;
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '_');
    let capped: String = trimmed.chars().take(64).collect();
    if capped.is_empty() {
        "tasmota-backup".to_string()
    } else {
        capped
    }
}

#[derive(Deserialize)]
pub struct ConsoleForm {
    command: String,
    confirmed: Option<String>,
}

/// `POST /device/:id/console` - send an arbitrary console command. Every
/// (sub)command is classified via `guardrail::classify` (a `Backlog` is
/// expanded and each subcommand checked, exactly like the CLI); a
/// `Destructive` or `RequiresConfirmation` result without `confirmed=true`
/// returns a confirm modal carrying the ORIGINAL command as a hidden field
/// and never reaches the device. `Safe` (or an already-confirmed request)
/// executes directly.
pub async fn console(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ConsoleForm>,
) -> Result<Response, AppError> {
    let (host, _name) = device_host_and_name(&state, &id).await?;
    let confirmed = form.confirmed.as_deref() == Some("true");
    let hazard = guardrail::classify(&form.command);
    let needs_confirm = matches!(
        hazard,
        Hazard::Destructive(_) | Hazard::RequiresConfirmation
    );
    if needs_confirm && !confirmed {
        // `needs_confirm` already narrowed `hazard` to Destructive/RequiresConfirmation;
        // `if let` (rather than a three-arm match) avoids a dead `Hazard::Safe` branch.
        let title = if let Hazard::Destructive(reason) = &hazard {
            format!("Run `{}`? {reason}.", form.command)
        } else {
            format!(
                "Run `{}`? Not a known-safe command; confirmation required.",
                form.command
            )
        };
        let modal = confirm_modal(
            &title,
            &format!("/device/{id}/console"),
            &[("command", &form.command)],
            "#admin-result",
        );
        return Ok(html! { (admin_result(html! {})) (modal) }.into_response());
    }
    let addr = state.addr_for(&host).await;
    let value = ops::console(&state.inner.transport, addr, form.command.clone()).await?;
    Ok(html! { (admin_result(result_block(&value))) (close_modal()) }.into_response())
}

#[derive(Deserialize)]
pub struct ConfigGetForm {
    setting: String,
}

/// `POST /device/:id/config/get` - read a single setting by issuing its bare
/// command word. Read-only, but still runs `validate_setting` so a smuggled
/// Backlog/argument/destructive word is rejected with 400 rather than sent.
pub async fn config_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ConfigGetForm>,
) -> Result<Markup, AppError> {
    validate_setting(&form.setting)?;
    let (host, _name) = device_host_and_name(&state, &id).await?;
    let addr = state.addr_for(&host).await;
    let value = ops::config_get(&state.inner.transport, addr, form.setting.clone()).await?;
    Ok(admin_result(result_block(&value)))
}

#[derive(Deserialize)]
pub struct ConfigSetForm {
    setting: String,
    value: String,
    confirmed: Option<String>,
}

/// `POST /device/:id/config/set` - write a single setting. `validate_setting`
/// runs FIRST, unconditionally (even before the confirm check), so an invalid
/// setting is rejected on every request, confirmed or not. A valid setting is
/// still destructive by nature (it writes device config) and always requires
/// `confirmed=true`, mirroring the CLI's unconditional `gate(..., true, ...)`
/// for `config set`.
pub async fn config_set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ConfigSetForm>,
) -> Result<Response, AppError> {
    validate_setting(&form.setting)?;
    let confirmed = form.confirmed.as_deref() == Some("true");
    if !confirmed {
        let modal = confirm_modal(
            &format!("Set `{}` to `{}`?", form.setting, form.value),
            &format!("/device/{id}/config/set"),
            &[("setting", &form.setting), ("value", &form.value)],
            "#admin-result",
        );
        return Ok(html! { (admin_result(html! {})) (modal) }.into_response());
    }
    let (host, _name) = device_host_and_name(&state, &id).await?;
    let addr = state.addr_for(&host).await;
    let value = ops::config_set(
        &state.inner.transport,
        addr,
        form.setting.clone(),
        form.value.clone(),
    )
    .await?;
    Ok(html! { (admin_result(result_block(&value))) (close_modal()) }.into_response())
}

/// `POST /device/:id/firmware/check` - read-only firmware version check, no
/// confirm modal.
pub async fn firmware_check(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Markup, AppError> {
    let (host, _name) = device_host_and_name(&state, &id).await?;
    let addr = state.addr_for(&host).await;
    let version = ops::firmware_version(&state.inner.transport, addr).await?;
    Ok(admin_result(html! { p { "Firmware version: " (version) } }))
}

#[derive(Deserialize)]
pub struct FirmwareUpdateForm {
    url: Option<String>,
    confirmed: Option<String>,
}

/// `POST /device/:id/firmware/update` - flash firmware (from the device's own
/// OTA URL, or the given `url`). Always destructive: without `confirmed=true`
/// this returns a confirm modal carrying the original `url` (when given) and
/// never touches the device; with `confirmed=true` it runs
/// `ops::firmware_update`, which sends `OtaUrl` (if a url was given) then
/// `Upgrade 1`.
pub async fn firmware_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<FirmwareUpdateForm>,
) -> Result<Response, AppError> {
    let confirmed = form.confirmed.as_deref() == Some("true");
    if !confirmed {
        let hidden: Vec<(&str, &str)> = match form.url.as_deref() {
            Some(u) if !u.is_empty() => vec![("url", u)],
            _ => vec![],
        };
        let modal = confirm_modal(
            "Flash firmware? This overwrites the device's running firmware.",
            &format!("/device/{id}/firmware/update"),
            &hidden,
            "#admin-result",
        );
        return Ok(html! { (admin_result(html! {})) (modal) }.into_response());
    }
    let (host, _name) = device_host_and_name(&state, &id).await?;
    let addr = state.addr_for(&host).await;
    let url = form.url.filter(|u| !u.is_empty());
    let value = ops::firmware_update(&state.inner.transport, addr, url).await?;
    Ok(html! { (admin_result(result_block(&value))) (close_modal()) }.into_response())
}

/// `GET /device/:id/backup` - streams the device's binary config backup
/// (`.dmp`) with a `Content-Disposition` filename SANITIZED from the
/// device-controlled name (`sanitize_filename`), never a raw/`.unwrap()`ed
/// value: a device name with header-hostile bytes (quotes, CR/LF, non-ASCII)
/// can only ever produce an allowlisted filename or, on the (now
/// unreachable) `HeaderValue` construction error, the static
/// `tasmota-backup.dmp` fallback.
pub async fn backup(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let (host, name) = device_host_and_name(&state, &id).await?;
    let addr = state.addr_for(&host).await;
    let bytes = ops::backup_config(&state.inner.transport, addr).await?;
    let safe = sanitize_filename(&name);
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{safe}.dmp\""))
        .unwrap_or_else(|_| {
            HeaderValue::from_static("attachment; filename=\"tasmota-backup.dmp\"")
        });
    let mut response = (StatusCode::OK, bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::sanitize_filename;

    #[test]
    fn sanitize_filename_allows_only_the_safe_charset() {
        let safe = sanitize_filename("Kit\"chen\r\nX");
        assert!(
            safe.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
            "sanitized filename must only contain [A-Za-z0-9._-], got {safe:?}"
        );
        assert!(!safe.is_empty());
    }

    #[test]
    fn sanitize_filename_falls_back_when_empty() {
        assert_eq!(sanitize_filename(""), "tasmota-backup");
        // Entirely hostile bytes that trim away to nothing.
        assert_eq!(sanitize_filename("...___..."), "tasmota-backup");
    }

    #[test]
    fn sanitize_filename_trims_leading_and_trailing_dots_and_underscores() {
        let safe = sanitize_filename(".._Kitchen_..");
        assert_eq!(safe, "Kitchen");
    }

    #[test]
    fn sanitize_filename_caps_length() {
        let long = "a".repeat(200);
        let safe = sanitize_filename(&long);
        assert!(safe.len() <= 64);
    }
}
