//! In-memory power history: a small ring buffer of recent measured-power
//! samples per device, plus the fleet total, recorded once per poll tick.
//! Session-lifetime only (no persistence, no database) - it exists to give
//! the dashboard and detail pages an honest short-term shape of consumption.
//!
//! Honesty invariants (mirrors the rest of the app):
//! - A sample is `Some(watts)` ONLY when the device was reachable AND
//!   reported a power reading on that tick. An offline, unpolled, or
//!   non-metering tick is recorded as `None` and renders as a GAP - never
//!   interpolated, never coerced to 0 (a measured `0.0` IS a real sample).
//! - The fleet sample is `Some(sum of reporting devices)` only when at least
//!   one device reported; an all-silent tick is a `None` gap.
//! - Buffers are keyed by device id and pruned every tick, so a removed
//!   device's history never lingers or reattaches to a later device.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// Samples kept per series. At the default 5s poll interval this is five
/// minutes of shape - enough to see a kettle spike or a heater cycling,
/// small enough to never matter in memory.
pub const CAPACITY: usize = 60;

#[derive(Default)]
pub struct History {
    fleet: VecDeque<Option<f64>>,
    devices: HashMap<String, VecDeque<Option<f64>>>,
}

pub type HistoryState = Mutex<History>;

/// A cloned, render-ready snapshot of every series. Views take this by
/// reference so rendering never holds the live lock.
#[derive(Debug, Clone, Default)]
pub struct Series {
    pub fleet: Vec<Option<f64>>,
    pub devices: HashMap<String, Vec<Option<f64>>>,
}

impl Series {
    pub fn device(&self, id: &str) -> &[Option<f64>] {
        self.devices.get(id).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn push(buf: &mut VecDeque<Option<f64>>, sample: Option<f64>) {
    if buf.len() == CAPACITY {
        buf.pop_front();
    }
    buf.push_back(sample);
}

/// Record one poll tick: `(device_id, measured_power)` covering the WHOLE
/// fleet (the poller reconciles every device every tick, so absence from
/// `samples` means the device left the fleet, and its buffer is pruned).
pub fn record_tick(state: &HistoryState, samples: &[(String, Option<f64>)]) {
    let mut h = state.lock().expect("history lock");
    let ids: std::collections::HashSet<&str> = samples.iter().map(|(id, _)| id.as_str()).collect();
    h.devices.retain(|id, _| ids.contains(id.as_str()));
    let mut total: Option<f64> = None;
    for (id, sample) in samples {
        if let Some(w) = sample {
            *total.get_or_insert(0.0) += w;
        }
        push(h.devices.entry(id.clone()).or_default(), *sample);
    }
    push(&mut h.fleet, total);
}

/// Snapshot every series for rendering.
pub fn snapshot(state: &HistoryState) -> Series {
    let h = state.lock().expect("history lock");
    Series {
        fleet: h.fleet.iter().copied().collect(),
        devices: h
            .devices
            .iter()
            .map(|(id, buf)| (id.clone(), buf.iter().copied().collect()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CAPACITY, HistoryState, record_tick, snapshot};

    fn state() -> HistoryState {
        HistoryState::default()
    }

    /// Offline ticks are recorded as gaps, and the fleet total sums only the
    /// devices that actually reported - never a coerced 0 for silence.
    #[test]
    fn gaps_stay_gaps_and_totals_sum_only_reporters() {
        let s = state();
        record_tick(
            &s,
            &[
                ("a".into(), Some(100.0)),
                ("b".into(), None), // offline: a gap, not zero
            ],
        );
        record_tick(&s, &[("a".into(), None), ("b".into(), None)]);

        let snap = snapshot(&s);
        assert_eq!(snap.device("a"), &[Some(100.0), None]);
        assert_eq!(snap.device("b"), &[None, None]);
        // Tick 1: only `a` reported -> total is a's reading. Tick 2: nobody
        // reported -> the fleet sample is a gap, NEVER Some(0.0).
        assert_eq!(snap.fleet, vec![Some(100.0), None]);
    }

    /// A real measured zero IS a sample: a switched-off metering plug reads
    /// 0.0 W and must be distinguishable from an offline gap.
    #[test]
    fn measured_zero_is_a_real_sample() {
        let s = state();
        record_tick(&s, &[("a".into(), Some(0.0))]);
        let snap = snapshot(&s);
        assert_eq!(snap.device("a"), &[Some(0.0)]);
        assert_eq!(snap.fleet, vec![Some(0.0)]);
    }

    /// The ring is bounded and drops the oldest sample first.
    #[test]
    fn ring_is_bounded_and_drops_oldest() {
        let s = state();
        for i in 0..(CAPACITY + 5) {
            record_tick(&s, &[("a".into(), Some(i as f64))]);
        }
        let snap = snapshot(&s);
        assert_eq!(snap.device("a").len(), CAPACITY);
        assert_eq!(snap.device("a")[0], Some(5.0));
        assert_eq!(snap.fleet.len(), CAPACITY);
    }

    /// A device removed from the fleet loses its buffer immediately: history
    /// must never linger for (or reattach to) a device that is gone.
    #[test]
    fn removed_device_history_is_pruned() {
        let s = state();
        record_tick(&s, &[("a".into(), Some(1.0)), ("b".into(), Some(2.0))]);
        record_tick(&s, &[("a".into(), Some(1.0))]);
        let snap = snapshot(&s);
        assert!(snap.device("b").is_empty(), "b left the fleet");
        assert_eq!(snap.device("a").len(), 2);
    }

    /// An unknown device renders an empty series, not a fabricated one.
    #[test]
    fn unknown_device_series_is_empty() {
        let snap = snapshot(&state());
        assert!(snap.device("nope").is_empty());
        assert!(snap.fleet.is_empty());
    }
}
