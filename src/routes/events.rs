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
    use std::time::Duration;

    use axum::body::BodyDataStream;
    use axum::http::StatusCode;
    use switchkit::Vendor;
    use tower::ServiceExt;

    use crate::config::{Config, DeviceConfig};
    use crate::routes;
    use crate::state::AppState;

    /// How long a bounded read waits for the next chunk before concluding the
    /// SSE body is idle (e.g. parked on the next broadcast tick) rather than
    /// truly finished. Every read in these tests goes through this timeout, so
    /// a broken stream that never yields fails the test instead of hanging it.
    const READ_WAIT: Duration = Duration::from_secs(2);

    /// Bound on chunks read per burst. These tests configure exactly one
    /// device, so a burst is a single SSE event / chunk; the higher cap just
    /// guards against a runaway loop if that ever changes.
    const MAX_CHUNKS_PER_BURST: usize = 8;

    /// Outcome of one bounded read attempt against the SSE body.
    enum Chunk {
        Data(Vec<u8>),
        /// No data arrived within `READ_WAIT`: the stream is idle (e.g.
        /// suspended in `rx.recv().await` waiting on the next tick), not
        /// necessarily finished.
        Idle,
        /// The underlying stream returned `None`: the SSE body has ended.
        Ended,
    }

    async fn next_chunk(body: &mut BodyDataStream, wait: Duration) -> Chunk {
        match tokio::time::timeout(wait, futures::StreamExt::next(body)).await {
            Ok(Some(Ok(bytes))) => Chunk::Data(bytes.to_vec()),
            Ok(Some(Err(e))) => panic!("SSE body stream error: {e}"),
            Ok(None) => Chunk::Ended,
            Err(_) => Chunk::Idle,
        }
    }

    /// Read chunks until the body goes idle or ends, bounded by
    /// `MAX_CHUNKS_PER_BURST` so a misbehaving stream can never hang the test.
    /// Returns the concatenated bytes as text, and whether the underlying
    /// stream had truly ended (vs. merely gone idle between ticks).
    async fn read_burst(body: &mut BodyDataStream) -> (String, bool) {
        let mut collected = Vec::new();
        let mut ended = false;
        for _ in 0..MAX_CHUNKS_PER_BURST {
            match next_chunk(body, READ_WAIT).await {
                Chunk::Data(bytes) => collected.extend_from_slice(&bytes),
                Chunk::Idle => break,
                Chunk::Ended => {
                    ended = true;
                    break;
                }
            }
        }
        (String::from_utf8_lossy(&collected).into_owned(), ended)
    }

    /// One configured (but not yet polled) device, an `AppState` for it, and
    /// its expected SSE event name (`device-{id}`).
    fn state_with_one_device(host: &str) -> (AppState, String) {
        let host = host.to_string();
        let config = Config {
            devices: vec![DeviceConfig {
                name: "Test Plug".into(),
                host: host.clone(),
                password: None,
                protected: false,
                vendor: Vendor::Tasmota,
            }],
            ..Config::default()
        };
        let state = AppState::new(config, PathBuf::from("unused.toml"));
        let expected_id = crate::fleet::device_id(&host);
        (state, expected_id)
    }

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
                vendor: Vendor::Tasmota,
            }],
            ..Config::default()
        };
        let state = AppState::new(config, PathBuf::from("unused.toml"));
        let expected_id = crate::fleet::device_id(&host);

        let app = routes::router(state, false);
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

    /// A poller tick (`state.notify()`) must trigger a SECOND burst of
    /// `device-{id}` events on top of the initial connect-time burst. This is
    /// the only coverage of the tick-driven render loop (the `Ok(()) => { for
    /// ... }` arm in `stream`): a change that removed the per-tick loop, or
    /// stopped re-rendering on tick, would silently break live dashboard
    /// updates while every other test kept passing.
    #[tokio::test]
    async fn events_stream_pushes_a_new_burst_on_notify() {
        let (state, expected_id) = state_with_one_device("192.0.2.41");
        let expected_event = format!("event: device-{expected_id}");

        let app = routes::router(state.clone(), false);
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

        let mut body = response.into_body().into_data_stream();

        let (initial, ended) = read_burst(&mut body).await;
        assert!(!ended, "stream ended during the initial burst");
        assert!(
            initial.contains(&expected_event),
            "initial burst should contain a device-{{id}} event, got: {initial}"
        );

        // Drive a poller tick. `stream` re-renders every device card on each
        // `state.subscribe()` message it receives.
        state.notify();

        let (second, ended) = read_burst(&mut body).await;
        assert!(
            !ended,
            "stream ended instead of delivering a tick-driven burst"
        );
        assert!(
            second.contains(&expected_event),
            "a poller tick (state.notify()) should push a new device-{{id}} burst, got: {second}"
        );
    }

    /// A slow reader that falls behind the broadcast channel's capacity must
    /// see the stream SKIP the missed ticks (`RecvError::Lagged` -> `continue`)
    /// and keep streaming, not terminate. This is the only coverage of the
    /// `Lagged` arm: a change that turned it into a `break` (ending the stream
    /// the way `Closed` does) would silently stop live updates for any client
    /// that ever falls behind, while every other test kept passing.
    #[tokio::test]
    async fn events_stream_continues_past_a_lagged_receiver() {
        let (state, expected_id) = state_with_one_device("192.0.2.42");
        let expected_event = format!("event: device-{expected_id}");

        let app = routes::router(state.clone(), false);
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

        let mut body = response.into_body().into_data_stream();

        let (initial, ended) = read_burst(&mut body).await;
        assert!(!ended, "stream ended during the initial burst");
        assert!(initial.contains(&expected_event));

        // `AppState::new` builds the broadcast channel with capacity 16
        // (`broadcast::channel(16)` in state.rs). Deliberately do NOT read the
        // body between these sends: the stream's receiver only advances when
        // the body is polled, so notifying well past capacity while the body
        // sits idle guarantees its next `recv()` observes `RecvError::Lagged`
        // rather than draining the sends one by one.
        for _ in 0..20 {
            state.notify();
        }
        state.notify();

        let (after_lag, ended) = read_burst(&mut body).await;
        assert!(
            !ended,
            "stream ended on a lagged receiver instead of continuing \
             (Lagged must `continue`, not `break`)"
        );
        assert!(
            !after_lag.is_empty(),
            "a burst should still arrive after the receiver lags behind the broadcast channel"
        );
        assert!(
            after_lag.contains(&expected_event),
            "the post-lag burst should still contain a device-{{id}} event, got: {after_lag}"
        );
    }
}
