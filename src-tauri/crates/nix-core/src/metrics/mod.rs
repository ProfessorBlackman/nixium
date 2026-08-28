// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The live metrics pipeline. `MON-1`.
//!
//! # The four principles this module exists to satisfy
//!
//! - **P3, one owner of state.** Every counter that needs a delta — CPU jiffies, disk sectors,
//!   network bytes — is owned by exactly one [`Sampler`], which is owned by exactly one
//!   [`Pipeline`]. There is no shared mutable delta state anywhere, because two consumers computing
//!   rates from one counter is how a monitor reports numbers that are each individually plausible and
//!   jointly impossible.
//! - **P4, no subprocess in the steady-state loop.** Everything here is a file read from `/proc` or
//!   `/sys`. Stacer shelled out to `ps`, `df` and `free` on a timer.
//! - **P8, parse into maps, never by line index.** `/proc/meminfo` is a map and is read as one.
//!   Stacer read it positionally, so a kernel that adds or reorders a field made it silently wrong.
//! - **P9, nothing samples until a view is mounted.** The pipeline is paused with no subscribers and
//!   does no work at all — not a cheap tick, none.
//!
//! # Why the history lives here and not in the frontend
//!
//! `MON-1`'s acceptance criterion is that *a late-mounting view immediately receives the full
//! 60-second history*. A frontend that accumulated its own history would start every chart from an
//! empty axis on each navigation, so the ring buffers are backend-side and a subscriber is handed the
//! whole window on its first read.

mod cpu;
mod disk;
mod memory;
mod net;

pub use cpu::{CpuReading, CpuTimes};
pub use disk::{DiskReading, DiskTotals};
pub use memory::{MemoryReading, sample as memory_sample};
pub use net::{InterfaceReading, NetReading};

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How many samples the ring holds. One a second, so a minute.
pub const HISTORY: usize = 60;

/// The interval between samples.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// A fixed-capacity history that drops its oldest entry rather than growing.
///
/// Bounded by construction, because a monitor that accumulates a sample a second and never forgets is
/// a memory leak with a chart on it.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    items: VecDeque<T>,
    capacity: usize,
}

impl<T: Clone> Ring<T> {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Add a sample, evicting the oldest if full.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    /// Everything held, oldest first.
    #[must_use]
    pub fn samples(&self) -> Vec<T> {
        self.items.iter().cloned().collect()
    }

    #[must_use]
    pub fn latest(&self) -> Option<&T> {
        self.items.back()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// Load averages, straight from `/proc/loadavg`.
///
/// The one metric that needs no delta: the kernel already averages it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LoadReading {
    pub one: f32,
    pub five: f32,
    pub fifteen: f32,
    /// Tasks currently runnable.
    #[ts(type = "number")]
    pub running: u32,
    /// Tasks in total.
    #[ts(type = "number")]
    pub total: u32,
}

/// Parse `/proc/loadavg`: `2.10 1.26 0.96 2/3144 1953697`.
#[must_use]
pub fn parse_loadavg(text: &str) -> Option<LoadReading> {
    let mut fields = text.split_whitespace();
    let one = fields.next()?.parse().ok()?;
    let five = fields.next()?.parse().ok()?;
    let fifteen = fields.next()?.parse().ok()?;

    // `running/total`, which is one field with a slash in it rather than two fields.
    let (running, total) = fields
        .next()
        .and_then(|entities| entities.split_once('/'))
        .map_or((0, 0), |(r, t)| {
            (r.parse().unwrap_or(0), t.parse().unwrap_or(0))
        });

    Some(LoadReading {
        one,
        five,
        fifteen,
        running,
        total,
    })
}

/// One tick: every metric family, sampled at the same moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Reading {
    /// Seconds since the Unix epoch.
    #[ts(type = "number")]
    pub at: i64,
    pub cpu: CpuReading,
    pub memory: MemoryReading,
    pub load: LoadReading,
    pub disk: DiskReading,
    pub network: NetReading,
}

/// Owns every sampler, ticks them together, and keeps the history.
///
/// # Paused means paused
///
/// §P9 requires that nothing samples until a view is mounted, and that idle CPU with the window
/// closed is about zero. The worker thread blocks on a condition variable while no one is
/// subscribed — not a cheap tick, not a short sleep in a loop. A blocked thread consumes nothing.
///
/// # Resuming clears the history
///
/// The ring means "the last sixty seconds". After a pause it does not, and handing a chart sixty
/// points spaced a second apart when four minutes elapsed between two of them would draw a
/// straight line through time that was never observed. The same reasoning as `STO-16`'s series: a
/// gap is a gap. So a resume starts the window again, and the samplers are reset too — their
/// previous counters are minutes stale, and subtracting them would report one enormous second.
pub struct Pipeline {
    shared: Arc<Shared>,
}

/// Called with each new reading, so the app can push it to the UI without polling.
type Observer = Box<dyn Fn(&Reading) + Send + Sync>;

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
    /// Invoked outside the state lock, so a slow consumer cannot stall sampling.
    observer: Mutex<Option<Observer>>,
}

struct State {
    cpu: cpu::CpuSampler,
    disk: disk::DiskSampler,
    net: net::NetSampler,
    history: Ring<Reading>,
    subscribers: usize,
    last_tick: Option<Instant>,
}

impl State {
    fn new() -> Self {
        Self {
            cpu: cpu::CpuSampler::new(),
            disk: disk::DiskSampler::new(),
            net: net::NetSampler::new(),
            history: Ring::new(HISTORY),
            subscribers: 0,
            last_tick: None,
        }
    }

    /// Forget everything measured. Used on resume, where the old state describes another time.
    fn reset(&mut self) {
        self.cpu = cpu::CpuSampler::new();
        self.disk = disk::DiskSampler::new();
        self.net = net::NetSampler::new();
        self.history.clear();
        self.last_tick = None;
    }
}

/// A subscription. Sampling runs while at least one is alive.
///
/// Dropping it is what pauses the pipeline, so a view that goes away cannot leave the machine
/// sampling forever — which is the failure §P9 exists to prevent, and it is not one a person
/// reliably remembers to avoid by calling an `unsubscribe` method.
pub struct Subscription {
    shared: Arc<Shared>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.subscribers = state.subscribers.saturating_sub(1);
        }
        self.shared.wake.notify_all();
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline {
    /// Start the worker. It sleeps until something subscribes.
    #[must_use]
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new()),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            observer: Mutex::new(None),
        });

        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("nix-metrics".into())
            .spawn(move || run(&worker))
            .ok();

        Self { shared }
    }

    /// Be told about each new reading, rather than polling for it.
    ///
    /// One observer, replaced by a later call. The app registers one that emits a Tauri event; a
    /// second consumer would be a second owner of the same stream, which §P3 is about avoiding.
    pub fn observe(&self, observer: impl Fn(&Reading) + Send + Sync + 'static) {
        if let Ok(mut slot) = self.shared.observer.lock() {
            *slot = Some(Box::new(observer));
        }
    }

    /// Begin sampling, or join sampling already in progress.
    #[must_use]
    pub fn subscribe(&self) -> Subscription {
        if let Ok(mut state) = self.shared.state.lock() {
            if state.subscribers == 0 {
                // Coming back from a pause: what was measured describes a different minute.
                state.reset();
            }
            state.subscribers += 1;
        }
        self.shared.wake.notify_all();
        Subscription {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Everything held, oldest first. **The whole window, so a late-mounting view has its history.**
    #[must_use]
    pub fn history(&self) -> Vec<Reading> {
        self.shared
            .state
            .lock()
            .map(|state| state.history.samples())
            .unwrap_or_default()
    }

    /// The most recent reading, if there is one.
    #[must_use]
    pub fn latest(&self) -> Option<Reading> {
        self.shared
            .state
            .lock()
            .ok()
            .and_then(|state| state.history.latest().cloned())
    }

    /// Whether anything is currently subscribed.
    #[must_use]
    pub fn is_sampling(&self) -> bool {
        self.shared
            .state
            .lock()
            .map(|state| state.subscribers > 0)
            .unwrap_or(false)
    }

    /// A pipeline with no worker thread, driven only by [`Pipeline::tick_once`].
    ///
    /// Tests that assert on the contents of the history need to know exactly how many readings are in
    /// it. With a live worker they are racing it — one such test failed with `left: 3, right: 2`,
    /// which is a flaw in the test rather than the code, and the kind that comes back intermittently
    /// on a loaded CI machine if it is papered over with a tolerance.
    #[cfg(test)]
    #[must_use]
    fn without_worker() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::new()),
                wake: Condvar::new(),
                stop: AtomicBool::new(false),
                observer: Mutex::new(None),
            }),
        }
    }

    /// Take one reading immediately, outside the tick. For tests.
    #[cfg(test)]
    fn tick_once(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            tick(&mut state);
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
    }
}

/// One sample of every family, appended to the history. Returns it, when there was one.
fn tick(state: &mut State) -> Option<Reading> {
    let elapsed = state
        .last_tick
        .map_or(TICK, |previous| previous.elapsed().max(TICK / 10));
    state.last_tick = Some(Instant::now());

    // Each sampler returns `None` until it has a delta, so a first tick produces nothing rather than
    // a reading built from lifetime averages.
    let (Some(cpu), Some(memory)) = (state.cpu.sample(), memory::sample()) else {
        return None;
    };
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| parse_loadavg(&text))
        .unwrap_or_default();

    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

    let reading = Reading {
        at,
        cpu,
        memory,
        load,
        disk: state.disk.sample(elapsed).unwrap_or_default(),
        network: state.net.sample(elapsed).unwrap_or_default(),
    };
    state.history.push(reading.clone());
    Some(reading)
}

/// The worker loop. Blocks entirely while nothing is subscribed.
fn run(shared: &Arc<Shared>) {
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }

        let Ok(mut state) = shared.state.lock() else {
            return;
        };

        // Nothing subscribed: block. Not a poll, not a short sleep — a blocked thread is the only
        // way "idle CPU is about zero" is true rather than merely small.
        while state.subscribers == 0 && !shared.stop.load(Ordering::Relaxed) {
            let Ok(next) = shared.wake.wait(state) else {
                return;
            };
            state = next;
        }
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }

        let fresh = tick(&mut state);
        drop(state);

        // Outside the state lock: an observer that blocks — an IPC channel with a slow reader — must
        // not hold up the next sample or a subscriber trying to leave.
        if let Some(reading) = fresh {
            if let Ok(observer) = shared.observer.lock() {
                if let Some(observer) = observer.as_ref() {
                    observer(&reading);
                }
            }
        }

        // Slept outside the lock, so a subscriber joining or leaving is not blocked for a second.
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// §P4: zero subprocess spawns in the steady-state monitoring loop.
    ///
    /// Checked by reading the module's own sources. Stacer ran `ps`, `df` and `free` on a timer, which
    /// is why this is a binding principle rather than a preference — a subprocess per second is both
    /// the CPU cost and a parsing surface that changes with the tool's version.
    ///
    /// The implementation is scanned, not the tests: this very function has to name the thing it
    /// forbids. A previous test of this shape in `reclaim::packages` scanned its own banned list and
    /// so could never fail.
    #[test]
    fn the_sampling_loop_spawns_no_subprocesses() {
        for (module, source) in [
            ("metrics/mod.rs", include_str!("mod.rs")),
            ("metrics/cpu.rs", include_str!("cpu.rs")),
            ("metrics/memory.rs", include_str!("memory.rs")),
            ("metrics/disk.rs", include_str!("disk.rs")),
            ("metrics/net.rs", include_str!("net.rs")),
        ] {
            let implementation = source.split("#[cfg(test)]").next().unwrap_or(source);
            for line in implementation.lines().filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
            }) {
                for forbidden in ["Command::new", "process::Command", "std::process::"] {
                    assert!(
                        !line.contains(forbidden),
                        "{module} spawns a subprocess: {line}"
                    );
                }
            }
        }
    }

    // ---- the pipeline ----

    /// §P9: nothing samples until a view is mounted.
    #[test]
    fn nothing_is_sampled_until_something_subscribes() {
        let pipeline = Pipeline::new();
        assert!(!pipeline.is_sampling());

        // Long enough that a tick would certainly have happened if one were going to.
        std::thread::sleep(TICK * 2);
        assert!(
            pipeline.history().is_empty(),
            "an unsubscribed pipeline must do no work at all"
        );
        assert_eq!(pipeline.latest(), None);
    }

    #[test]
    fn subscribing_starts_sampling_and_dropping_stops_it() {
        let pipeline = Pipeline::new();
        let subscription = pipeline.subscribe();
        assert!(pipeline.is_sampling());

        drop(subscription);
        assert!(
            !pipeline.is_sampling(),
            "a view going away must stop the machine sampling, without anyone remembering to say so"
        );
    }

    #[test]
    fn several_subscribers_share_one_pipeline() {
        let pipeline = Pipeline::new();
        let first = pipeline.subscribe();
        let second = pipeline.subscribe();
        assert!(pipeline.is_sampling());

        drop(first);
        assert!(
            pipeline.is_sampling(),
            "one view closing must not stop sampling for another"
        );
        drop(second);
        assert!(!pipeline.is_sampling());
    }

    /// The acceptance criterion: a view mounting late gets the window that already exists.
    #[test]
    fn a_late_subscriber_receives_the_history_already_collected() {
        let pipeline = Pipeline::without_worker();
        let _first = pipeline.subscribe();

        // Two readings, without waiting two seconds of wall clock.
        pipeline.tick_once();
        pipeline.tick_once();
        pipeline.tick_once();
        let before = pipeline.history().len();
        if before == 0 {
            return; // /proc unavailable in this environment
        }

        let _late = pipeline.subscribe();
        assert_eq!(
            pipeline.history().len(),
            before,
            "a second subscriber joins the existing window rather than resetting it"
        );
    }

    /// After a pause the ring no longer means "the last sixty seconds".
    #[test]
    fn resuming_after_a_pause_starts_the_window_again() {
        let pipeline = Pipeline::without_worker();
        let subscription = pipeline.subscribe();
        pipeline.tick_once();
        pipeline.tick_once();
        if pipeline.history().is_empty() {
            return;
        }

        drop(subscription);
        let _resumed = pipeline.subscribe();
        assert!(
            pipeline.history().is_empty(),
            "sixty points spaced a second apart would draw a line through time nobody observed"
        );
    }

    /// The first tick has no delta to work from, so it produces nothing rather than a lifetime average.
    #[test]
    fn the_first_tick_produces_no_reading() {
        let pipeline = Pipeline::without_worker();
        let _subscription = pipeline.subscribe();
        pipeline.tick_once();
        assert!(
            pipeline.history().is_empty(),
            "one reading of a cumulative counter is not a rate"
        );
    }

    #[test]
    fn readings_carry_every_family_and_a_timestamp() {
        let pipeline = Pipeline::without_worker();
        let _subscription = pipeline.subscribe();
        pipeline.tick_once();
        pipeline.tick_once();

        let Some(reading) = pipeline.latest() else {
            return;
        };
        assert!(reading.at > 0, "a reading is anchored in time");
        assert!((0.0..=1.0).contains(&reading.cpu.total));
        assert!(reading.memory.total > 0);
        assert!(reading.load.total > 0);
    }

    /// The history is bounded whatever happens.
    #[test]
    fn the_history_never_exceeds_its_window() {
        let pipeline = Pipeline::without_worker();
        let _subscription = pipeline.subscribe();
        for _ in 0..(HISTORY + 20) {
            pipeline.tick_once();
        }
        assert!(pipeline.history().len() <= HISTORY);
    }

    #[test]
    fn a_dropped_pipeline_stops_its_worker() {
        let pipeline = Pipeline::new();
        let _subscription = pipeline.subscribe();
        drop(pipeline);
        // Nothing to assert beyond not hanging: the worker observes `stop` and returns, and the test
        // process exiting cleanly is the evidence.
    }

    #[test]
    fn a_ring_forgets_its_oldest_and_never_grows() {
        let mut ring: Ring<u32> = Ring::new(3);
        for i in 0..10 {
            ring.push(i);
        }
        assert_eq!(ring.len(), 3, "the capacity is the whole point");
        assert_eq!(ring.samples(), vec![7, 8, 9], "oldest first, newest last");
        assert_eq!(ring.latest(), Some(&9));
    }

    #[test]
    fn a_ring_reports_a_partial_history_rather_than_padding_it() {
        let mut ring: Ring<u32> = Ring::new(60);
        ring.push(1);
        ring.push(2);
        assert_eq!(ring.len(), 2);
        assert_eq!(
            ring.samples(),
            vec![1, 2],
            "two seconds of history is two points, not sixty with fifty-eight zeros"
        );
    }

    #[test]
    fn an_empty_ring_has_no_latest() {
        let ring: Ring<u32> = Ring::new(60);
        assert!(ring.is_empty());
        assert_eq!(ring.latest(), None);
        assert!(ring.samples().is_empty());
    }

    #[test]
    fn a_zero_capacity_ring_still_holds_one() {
        // Rather than dividing by zero or holding nothing, which would make `latest` useless.
        let mut ring: Ring<u32> = Ring::new(0);
        ring.push(5);
        assert_eq!(ring.latest(), Some(&5));
    }

    /// Golden file, §P8. Captured from this machine.
    #[test]
    fn loadavg_is_parsed_from_real_output() {
        let load = parse_loadavg("2.10 1.26 0.96 2/3144 1953697\n").unwrap();
        assert!((load.one - 2.10).abs() < 0.001);
        assert!((load.five - 1.26).abs() < 0.001);
        assert!((load.fifteen - 0.96).abs() < 0.001);
        assert_eq!(load.running, 2);
        assert_eq!(load.total, 3144);
    }

    #[test]
    fn a_malformed_loadavg_yields_nothing_rather_than_zeros() {
        // Zeros would be indistinguishable from a genuinely idle machine.
        assert!(parse_loadavg("").is_none());
        assert!(parse_loadavg("not numbers here").is_none());
        assert!(parse_loadavg("1.0 2.0").is_none());
    }

    #[test]
    fn a_truncated_entity_count_does_not_lose_the_averages() {
        let load = parse_loadavg("0.50 0.40 0.30").unwrap();
        assert!((load.one - 0.5).abs() < 0.001);
        assert_eq!(load.running, 0, "unknown, and said so as zero");
    }

    #[test]
    fn this_machines_loadavg_parses() {
        let Ok(text) = std::fs::read_to_string("/proc/loadavg") else {
            return;
        };
        let load = parse_loadavg(&text).expect("/proc/loadavg must parse");
        assert!(load.one >= 0.0);
        assert!(load.total > 0, "a running machine has tasks");
        assert!(load.running <= load.total);
    }
}
