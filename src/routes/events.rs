//! The `/events` SSE stream: pushes each device's card HTML as a named
//! `device-{id}` event so htmx's `sse-swap="device-{id}"` on that card swaps
//! it in place. Renders an initial burst on connect (so a newly-connected
//! browser sees current state immediately) and one burst per poller tick.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;
use crate::views::dashboard::device_card;

/// Snapshot the fleet under a short-lived read lock and render every device's
/// card. The lock is dropped before returning, so it is never held across an
/// `.await` in the stream below.
async fn render_all_cards(state: &AppState) -> Vec<(String, String)> {
    let fleet = state.inner.fleet.read().await;
    fleet
        .devices
        .iter()
        .map(|d| (d.id.clone(), device_card(d).into_string()))
        .collect()
}

pub async fn stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.subscribe();
    let s = async_stream::stream! {
        // Initial burst: a newly-connected browser must see current state
        // without waiting for the next poller tick.
        for (id, html) in render_all_cards(&state).await {
            yield Ok(Event::default().event(format!("device-{id}")).data(html));
        }

        loop {
            match rx.recv().await {
                Ok(()) => {
                    // Render one SSE event per device card; htmx `sse-swap` matches by name.
                    for (id, html) in render_all_cards(&state).await {
                        yield Ok(Event::default().event(format!("device-{id}")).data(html));
                    }
                }
                // A slow client that missed ticks: skip, do not disconnect.
                Err(RecvError::Lagged(_)) => continue,
                // Sender dropped (shutdown): end the stream.
                Err(RecvError::Closed) => break,
            }
        }
    };
    Sse::new(s).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::http::StatusCode;
    use tower::ServiceExt;

    use crate::config::{Config, DeviceConfig};
    use crate::routes;
    use crate::state::AppState;

    /// `GET /events` returns 200 with the SSE content type, and its initial
    /// burst (sent immediately on connect, before any poller tick) contains a
    /// `device-{id}` event for a known configured device.
    #[tokio::test]
    async fn events_stream_returns_sse_headers_and_initial_burst() {
        let host = "192.0.2.40".to_string();
        let config = Config {
            devices: vec![DeviceConfig {
                name: "Test Plug".into(),
                host: host.clone(),
                password: None,
                protected: false,
            }],
            ..Config::default()
        };
        let state = AppState::new(config, PathBuf::from("unused.toml"));
        let expected_id = crate::fleet::device_id(&host);

        let app = routes::router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/event-stream"
        );

        // Read only the initial burst rather than the whole (never-ending)
        // stream: the first frame collected off the body is enough to prove
        // the connect-time render fired with the right event name.
        let mut body = response.into_body().into_data_stream();
        let mut collected = Vec::new();
        let expected_event = format!("event: device-{expected_id}");
        for _ in 0..20 {
            let Some(chunk) = futures::StreamExt::next(&mut body).await else {
                break;
            };
            collected.extend_from_slice(&chunk.unwrap());
            if String::from_utf8_lossy(&collected).contains(&expected_event) {
                break;
            }
        }
        let text = String::from_utf8_lossy(&collected);
        assert!(
            text.contains(&expected_event),
            "initial burst should contain a device-{{id}} event for the configured device, got: {text}"
        );
    }
}
