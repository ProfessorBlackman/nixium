// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The command surface. Task 0.3 (`FND-2`).
//!
//! Conventions, applied without exception:
//!
//! - **Every command returns `Result<T, AppError>`**, so a failure crosses the boundary as typed
//!   data rather than a string.
//! - **Long operations return an [`OperationId`] immediately** and report through events. They
//!   never block the caller, and they honour cancellation.
//! - **Naming is `noun_verb`** (`settings_save`, `operation_cancel`), so related commands sort
//!   together.
//!
//! Events emitted:
//!
//! | Event | Payload | Meaning |
//! |---|---|---|
//! | `op://progress` | [`Progress`] | Incremental progress for one operation |
//! | `op://done` | [`Completion`] | Terminal outcome: done, cancelled, or failed |

use std::path::PathBuf;

use nix_core::cache::{Cache, CachedScan};
use nix_core::caps;
use nix_core::cow::{self, Snapshot};
use nix_core::error::{AppError, ErrorCode, Result};
use nix_core::helper::{self, Op, OpResult};
use nix_core::logging::{self, Diagnostics};
use nix_core::op::{Completion, OperationId, Progress};
use nix_core::protect::Refusal;
use nix_core::reclaim::{Preview, Report, Ticket};
use nix_core::settings::Settings;
use nix_core::{find, fs as nixfs, history, metrics, process, scan, timer};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;

/// Event name for incremental progress.
pub(crate) const EVENT_PROGRESS: &str = "op://progress";
/// Event name for the terminal outcome of an operation.
pub(crate) const EVENT_DONE: &str = "op://done";

/// Versions of both halves of the application, so a mismatched install is detectable.
#[derive(Debug, Serialize)]
pub(crate) struct Versions {
    pub(crate) app: String,
    pub(crate) core: String,
}

#[tauri::command]
pub(crate) fn versions() -> Versions {
    Versions {
        app: env!("CARGO_PKG_VERSION").to_string(),
        core: nix_core::VERSION.to_string(),
    }
}

/// The diagnostics bundle, for the About view and for bug reports.
#[tauri::command]
pub(crate) fn diagnostics() -> Result<Diagnostics> {
    logging::diagnostics()
}

/// What this host can do. Drives which features the UI offers (`FND-7`).
#[tauri::command]
pub(crate) fn capabilities() -> caps::Capabilities {
    caps::registry().snapshot()
}

/// Re-probe capabilities. Call after anything that could install or remove a tool.
#[tauri::command]
pub(crate) fn capabilities_refresh() -> caps::Capabilities {
    caps::registry().invalidate();
    caps::registry().snapshot()
}

#[tauri::command]
pub(crate) fn settings_get(state: State<'_, AppState>) -> Settings {
    state.settings()
}

#[tauri::command]
pub(crate) fn settings_save(state: State<'_, AppState>, settings: Settings) -> Result<Settings> {
    state.save_settings(settings)
}

/// A warning raised during startup, delivered once so the notification centre can show it.
#[tauri::command]
pub(crate) fn startup_warning(state: State<'_, AppState>) -> Option<AppError> {
    state.take_startup_warning()
}

/// Request cancellation. Returns whether an operation with that id was still running.
#[tauri::command]
pub(crate) fn operation_cancel(state: State<'_, AppState>, id: OperationId) -> bool {
    state.operations.cancel(id)
}

/// Number of operations currently in flight.
#[tauri::command]
pub(crate) fn operation_count(state: State<'_, AppState>) -> usize {
    state.operations.live_count()
}

/// Ask the privileged helper to identify itself.
///
/// The Phase 0 proof that the escalation loop works end to end: spawn under polkit, handshake,
/// verify the protocol version, report the effective uid. A refused authorisation comes back as
/// [`ErrorCode::AuthDenied`] with a remedy — never as success, which is what Stacer did.
#[tauri::command]
pub(crate) fn helper_probe() -> Result<HelperProbe> {
    let transport = helper::Transport::production()?;
    let mut client = helper::Client::connect(&transport)?;
    let uid = client.helper_uid();
    let elevated = client.is_elevated();

    let osrelease = match client.request(&Op::ReadTextFile {
        path: PathBuf::from("/proc/sys/kernel/osrelease"),
    })? {
        OpResult::Text { content } => content.trim().to_string(),
        other => {
            return Err(AppError::internal(format!(
                "Helper answered a file read with {other:?}"
            )));
        }
    };

    Ok(HelperProbe {
        uid,
        elevated,
        kernel: osrelease,
    })
}

/// Result of [`helper_probe`].
#[derive(Debug, Serialize)]
pub(crate) struct HelperProbe {
    pub(crate) uid: u32,
    pub(crate) elevated: bool,
    pub(crate) kernel: String,
}

/// Mounted filesystems. `STO-1`.
///
/// Pseudo-filesystems are excluded unless the user has asked for them: they are not storage, and
/// including them is why Stacer's disk chart needed two filter combo boxes to be readable.
#[tauri::command]
pub(crate) fn filesystems(state: State<'_, AppState>) -> Result<Vec<nixfs::Filesystem>> {
    nixfs::filesystems(state.settings().show_pseudo_filesystems)
        .map_err(|e| e.context("listing filesystems"))
}

/// Where a completed scan's result is delivered.
pub(crate) const EVENT_SCAN_DONE: &str = "scan://done";

/// The previous scan of a path, if one was kept.
///
/// The explorer calls this on mount so the view is never empty after the first scan, per D6. The
/// result is labelled with its age in the UI: a figure presented without its age invites a reader to
/// trust a stale number.
#[tauri::command]
pub(crate) fn scan_cached(path: PathBuf, max_depth: Option<usize>) -> Option<CachedScan> {
    let options = scan::Options::new(path).max_depth(max_depth.or(Some(12)));
    Cache::discover().ok()?.load_for(&options)
}

/// The largest files in the cached scan. `STO-15`.
///
/// A projection, not a search. Stacer made the user fill in a `find` dialogue — path, pattern, size,
/// unit — and then listed the results without their sizes. The scan already knows.
#[tauri::command]
pub(crate) fn largest_files(
    path: PathBuf,
    limit: Option<usize>,
) -> Vec<nix_core::space::SpaceEntry> {
    let Some(cached) = Cache::discover().ok().and_then(|c| c.load(&path)) else {
        return Vec::new();
    };
    find::largest_files(&cached.result.tree, limit.unwrap_or(100))
}

/// Event carrying a finished duplicate search.
pub(crate) const EVENT_DUPLICATES_DONE: &str = "duplicates://done";

/// Search the cached scan for duplicate content. `STO-15`.
///
/// Returns immediately; the search runs on its own thread, reports progress on `op://progress`, and
/// delivers its result on `duplicates://done`. Hashing is staged and cancellable between chunks, so a
/// stop does not wait for a gigabyte to finish.
#[tauri::command]
pub(crate) fn duplicates_find(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    minimum_bytes: Option<u64>,
) -> OperationId {
    let (id, token) = state.operations.start();
    let minimum = minimum_bytes.unwrap_or(find::MIN_DUPLICATE_BYTES);

    std::thread::spawn(move || {
        let cached = Cache::discover().ok().and_then(|c| c.load(&path));
        let Some(cached) = cached else {
            let completion = Completion::Failed {
                id,
                error: AppError::new(
                    ErrorCode::Unsupported,
                    "There is no scan to search for duplicates in.",
                )
                .with_remedy("Scan a directory first, then look for duplicates in the result."),
            };
            if let Err(e) = app.emit(EVENT_DONE, &completion) {
                tracing::warn!(error = %e, "could not emit duplicate completion");
            }
            return;
        };

        let progress_handle = app.clone();
        let outcome = find::duplicates(&cached.result.tree, minimum, &token, move |done, total| {
            let progress = Progress::new(id, done)
                .with_total(total)
                .with_message(format!("hashed {done} of {total} candidates"));
            if let Err(e) = progress_handle.emit(EVENT_PROGRESS, &progress) {
                tracing::warn!(error = %e, "could not emit duplicate progress");
            }
        });

        let completion = match outcome {
            Ok((groups, stats)) => {
                let report = find::DuplicateReport {
                    recoverable: groups.iter().map(|g| g.recoverable).sum(),
                    cancelled: token.is_cancelled(),
                    groups,
                    stats,
                };
                let cancelled = report.cancelled;
                if let Err(e) = app.emit(EVENT_DUPLICATES_DONE, &report) {
                    tracing::warn!(error = %e, "could not emit duplicate report");
                }
                if cancelled {
                    Completion::Cancelled { id }
                } else {
                    Completion::Done { id }
                }
            }
            Err(error) if !error.is_fault() => Completion::Cancelled { id },
            Err(error) => Completion::Failed { id, error },
        };
        if let Err(e) = app.emit(EVENT_DONE, &completion) {
            tracing::warn!(error = %e, "could not emit duplicate completion");
        }
    });

    id
}

/// The last reclaim preview's totals, without computing one. `MON-2`.
///
/// `None` until a preview has been run this session. The dashboard says "not measured yet" rather
/// than scanning on mount, which the acceptance criterion forbids.
#[tauri::command]
pub(crate) fn reclaim_last_total(state: State<'_, AppState>) -> Option<(u64, u64)> {
    state.reclaim.last_preview()
}

// ---- The process table. `PRC-1`, `PRC-2` ----

/// Every process, busiest first. `PRC-1`.
///
/// Poll this while the view is open; nothing samples otherwise (§P9). The first call reports zero CPU
/// for everything, because one reading of a cumulative counter is not a rate — the second call onward
/// carries real instantaneous figures.
///
/// The interval is measured here rather than assumed, so a slow frame or a paused laptop produces a
/// correct percentage rather than one scaled by a tick length that did not happen.
#[tauri::command]
pub(crate) fn processes_list(state: State<'_, AppState>) -> Vec<process::Process> {
    let mut held = match state.processes.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    let (sampler, last) = &mut *held;

    let elapsed = last.map_or(process::TABLE_INTERVAL, |at| {
        // A floor, so a caller polling twice in quick succession cannot divide a tiny interval into a
        // huge percentage.
        at.elapsed().max(std::time::Duration::from_millis(100))
    });
    *last = Some(std::time::Instant::now());
    sampler.sample(elapsed)
}

/// Forget the process table's delta state. `PRC-1`.
///
/// Called when the view unmounts. Without it, reopening the view after ten minutes would compute a
/// percentage from a ten-minute-old counter over an assumed interval — a figure that is neither
/// instantaneous nor an average, which is the worst of both.
#[tauri::command]
pub(crate) fn processes_forget(state: State<'_, AppState>) {
    let mut held = match state.processes.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    *held = (process::ProcessSampler::new(), None);
}

/// Send a signal to a process. `PRC-2`.
///
/// Tries as the current user first and only escalates on `EPERM`, so signalling your own processes
/// never prompts — and a prompt, when it appears, means the process really does belong to someone
/// else. `state` is the process's state as the table last saw it, used to refuse a zombie before
/// anything is sent.
#[tauri::command]
pub(crate) fn process_signal(
    pid: u32,
    signal: nix_core::signal::Signal,
    process_state: process::ProcessState,
) -> Result<()> {
    match nix_core::signal::send(pid, process_state, signal) {
        Ok(()) => Ok(()),
        // Only a permission failure is worth escalating. A missing process or a zombie is a fact, not
        // an authorisation problem, and asking for a password would not change it.
        Err(error) if error.code == ErrorCode::AuthDenied => {
            let transport = helper::Transport::production()?;
            let mut client = helper::Client::connect(&transport)?;
            match client.request(&Op::SignalProcess { pid, signal })? {
                OpResult::Reclaimed { .. } => Ok(()),
                other => Err(AppError::internal(format!(
                    "The helper answered a signal with {other:?}"
                ))),
            }
        }
        Err(error) => Err(error),
    }
}

/// Change a process's niceness. `PRC-2`.
///
/// Escalates on permission failure for the same reason, and that is more often here: lowering a
/// niceness is privileged even for your own process.
#[tauri::command]
pub(crate) fn process_renice(pid: u32, niceness: i32) -> Result<()> {
    match nix_core::signal::renice(pid, niceness) {
        Ok(()) => Ok(()),
        Err(error) if error.code == ErrorCode::AuthDenied => {
            let transport = helper::Transport::production()?;
            let mut client = helper::Client::connect(&transport)?;
            match client.request(&Op::ReniceProcess { pid, niceness })? {
                OpResult::Reclaimed { .. } => Ok(()),
                other => Err(AppError::internal(format!(
                    "The helper answered a renice with {other:?}"
                ))),
            }
        }
        Err(error) => Err(error),
    }
}

// ---- Live metrics. `MON-1` ----

/// Event carrying one metrics reading.
pub(crate) const EVENT_METRICS_TICK: &str = "metrics://tick";

/// Start sampling and return the history that already exists.
///
/// The return value is the acceptance criterion made concrete: a view mounting late is handed the
/// whole window rather than starting its charts from an empty axis. Sampling continues until
/// [`metrics_unsubscribe`], and nothing samples before this is called (§P9).
#[tauri::command]
pub(crate) fn metrics_subscribe(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Vec<metrics::Reading> {
    let mut held = match state.metrics_subscription.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };

    if held.is_none() {
        // Registered before subscribing, so the very first reading is delivered rather than missed.
        let emitter = app.clone();
        state.metrics.observe(move |reading| {
            if let Err(e) = emitter.emit(EVENT_METRICS_TICK, reading) {
                tracing::warn!(error = %e, "could not emit a metrics reading");
            }
        });
        *held = Some(state.metrics.subscribe());
    }

    state.metrics.history()
}

/// Stop sampling.
///
/// Idempotent, and the only way sampling stops — which is deliberate: the subscription's `Drop` is
/// what pauses the pipeline, so there is no path where the state says "not sampling" while the worker
/// carries on.
#[tauri::command]
pub(crate) fn metrics_unsubscribe(state: State<'_, AppState>) {
    let mut held = match state.metrics_subscription.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    *held = None;
}

/// The window as it stands, without subscribing.
#[tauri::command]
pub(crate) fn metrics_history(state: State<'_, AppState>) -> Vec<metrics::Reading> {
    state.metrics.history()
}

/// Whether the pipeline is currently sampling. Shown in About, so §P9 is visible rather than claimed.
#[tauri::command]
pub(crate) fn metrics_sampling(state: State<'_, AppState>) -> bool {
    state.metrics.is_sampling()
}

/// Evaluate the alert rules against the latest reading. `MON-6`.
///
/// Returns only rules that **just** crossed — a rule already firing, one inside its cooldown, and one
/// that merely cleared all return nothing, which is the acceptance criterion. Called by the frontend
/// on each tick, so the state machine lives in one place rather than being reimplemented there.
#[tauri::command]
pub(crate) fn alerts_evaluate(state: State<'_, AppState>) -> Vec<metrics::Metric> {
    let rules = state.settings().alert_rules;
    if rules.is_empty() {
        return Vec::new();
    }
    let Some(reading) = state.metrics.latest() else {
        return Vec::new();
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

    let mut alerts = match state.alerts.lock() {
        Ok(alerts) => alerts,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut fired = Vec::new();
    for rule in &rules {
        let Some(value) = value_for(&rule.metric, &reading) else {
            continue;
        };
        if alerts.evaluate(rule, value, now).notifies() {
            fired.push(rule.metric.clone());
        }
    }
    fired
}

/// The current value of a metric, or `None` when this machine cannot answer.
///
/// A rule watching a filesystem that has been unmounted, or a temperature on hardware that has none,
/// evaluates to nothing rather than to zero — which would fire a free-space alert on a disk that is
/// simply not there.
fn value_for(metric: &metrics::Metric, reading: &metrics::Reading) -> Option<f64> {
    match metric {
        metrics::Metric::CpuUsage => Some(f64::from(reading.cpu.total)),
        metrics::Metric::MemoryPressure => reading.memory.pressure().map(f64::from),
        metrics::Metric::SwapPressure => reading.memory.swap_pressure().map(f64::from),
        metrics::Metric::Temperature => reading
            .sensors
            .temperatures
            .iter()
            .map(|t| f64::from(t.celsius))
            .fold(None, |best: Option<f64>, c| {
                Some(best.map_or(c, |b| b.max(c)))
            }),
        metrics::Metric::DiskUsage { mount } => nixfs::filesystems(false)
            .ok()?
            .into_iter()
            .find(|fs| fs.mount_point.to_string_lossy() == *mount)
            .and_then(|fs| fs.used_fraction()),
        metrics::Metric::DiskSpaceRemaining { mount } => nixfs::filesystems(false)
            .ok()?
            .into_iter()
            .find(|fs| fs.mount_point.to_string_lossy() == *mount)
            .map(|fs| {
                #[allow(clippy::cast_precision_loss)]
                let available = fs.available as f64;
                available
            }),
    }
}

// ---- Growth history. `STO-16` ----

/// Every stored sample, oldest first.
#[tauri::command]
pub(crate) fn history_samples() -> Vec<history::Sample> {
    history::History::discover()
        .map(|h| h.samples())
        .unwrap_or_default()
}

/// Samples bucketed onto an interval, with gaps left as gaps.
///
/// `interval_seconds` defaults to a day. Nothing here interpolates: a missing interval means the
/// machine was off, on battery, or nix was not running, and inventing a point would turn "we do not
/// know" into a number someone might act on (§P8).
#[tauri::command]
pub(crate) fn history_series(interval_seconds: Option<i64>) -> history::Series {
    let samples = history_samples();
    history::series(&samples, interval_seconds.unwrap_or(86_400))
}

#[tauri::command]
pub(crate) fn history_growth(since_seconds: i64, limit: Option<usize>) -> history::GrowthReport {
    let samples = history_samples();
    history::GrowthReport {
        total: history::growth(&samples, since_seconds),
        directories: history::fastest_growing(&samples, since_seconds, limit.unwrap_or(10)),
    }
}

/// Delete all collected history. Offered because data a user cannot delete is data taken from them.
#[tauri::command]
pub(crate) fn history_clear() -> Result<()> {
    history::History::discover()?.clear()
}

/// Take a sample now, from the cached scan rather than by walking.
///
/// The timer's job does a full scan; this exists so a user can seed a series without waiting for
/// tomorrow, and so the feature can be seen working. It refuses rather than guessing when there is no
/// scan to sample.
#[tauri::command]
pub(crate) fn history_snapshot_now(path: PathBuf) -> Result<history::Sample> {
    let cached = Cache::discover()?.load(&path).ok_or_else(|| {
        AppError::new(
            ErrorCode::Unsupported,
            "There is no scan to take a sample from.",
        )
        .with_remedy("Scan a directory first, then record a sample of it.")
    })?;

    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let sample = history::Sample::from_scan(at, &cached.result, 40);
    history::History::discover()?.record(&sample)?;
    Ok(sample)
}

// ---- The collection timer. `STO-16`, decision D5 ----

/// The path of the running executable, which is what a unit's `ExecStart` has to name.
fn executable() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nix"))
}

/// What is installed, and whether it is current.
///
/// Called at startup so an orphaned unit from a previous version is *detected* rather than left
/// failing silently every day.
#[tauri::command]
pub(crate) fn timer_state() -> timer::State {
    timer::state(&executable())
}

/// Install the units and enable the timer. Installing over an orphan is how an orphan is repaired.
#[tauri::command]
pub(crate) fn timer_install() -> Result<timer::State> {
    timer::install(&executable())
}

/// Disable the timer, remove the units, and delete the collected data.
#[tauri::command]
pub(crate) fn timer_uninstall() -> Result<timer::State> {
    timer::uninstall(&executable())
}

/// Forget cached scans. Offered because a cache the user cannot clear is a cache they cannot trust.
#[tauri::command]
pub(crate) fn scan_cache_clear() -> Result<()> {
    Cache::discover()?.clear()
}

/// How much space nix's own cache occupies. A storage tool should be able to answer this about
/// itself.
#[tauri::command]
pub(crate) fn scan_cache_size() -> u64 {
    Cache::discover().map(|c| c.size_on_disk()).unwrap_or(0)
}

/// Start a scan. `STO-2`.
///
/// Returns an [`OperationId`] immediately; progress arrives on `op://progress`, the tree on
/// `scan://done`, and the terminal outcome on `op://done`. Nothing about this blocks the caller.
#[tauri::command]
pub(crate) fn scan_start(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    max_depth: Option<usize>,
    cross_filesystems: Option<bool>,
) -> OperationId {
    let (id, token) = state.operations.start();
    let exclude = state.settings().protected_paths;

    let options = scan::Options::new(path)
        .max_depth(max_depth.or(Some(12)))
        .cross_filesystems(cross_filesystems.unwrap_or(false))
        .exclude(exclude);

    let cache_options = options.clone();
    let emitter = app.clone();
    std::thread::spawn(move || {
        let progress_handle = emitter.clone();
        let outcome = scan::scan(options, token, move |files, bytes| {
            let progress = Progress::new(id, files)
                .with_message(format!("{files} items, {bytes} bytes so far"));
            if let Err(e) = progress_handle.emit(EVENT_PROGRESS, &progress) {
                tracing::warn!(error = %e, "could not emit scan progress");
            }
        });

        // A cancelled scan still carries a usable partial tree, so it is delivered either way.
        let completion = match outcome {
            Ok(result) => {
                let cancelled = result.cancelled;

                // Persist for the next open. A cache that cannot be written is a missed
                // optimisation, not a failed scan, so this only logs.
                match Cache::discover().and_then(|c| c.store(&cache_options, &result)) {
                    Ok(()) => tracing::debug!(root = %cache_options.root.display(), "scan cached"),
                    Err(e) => tracing::warn!(error = %e, "could not cache the scan"),
                }

                if let Err(e) = emitter.emit(EVENT_SCAN_DONE, &result) {
                    tracing::warn!(error = %e, "could not emit scan result");
                }
                if cancelled {
                    Completion::Cancelled { id }
                } else {
                    Completion::Done { id }
                }
            }
            Err(error) => Completion::Failed { id, error },
        };

        if let Err(e) = emitter.emit(EVENT_DONE, &completion) {
            tracing::warn!(error = %e, "could not emit scan completion");
        }
        if let Some(state) = emitter.try_state::<AppState>() {
            state.operations.finish(id);
        }
    });

    id
}

/// The user's home directory, as a sensible default scan root.
#[tauri::command]
pub(crate) fn home_directory() -> Result<PathBuf> {
    nix_core::paths::home_dir().ok_or_else(|| {
        AppError::new(
            ErrorCode::Unsupported,
            "Could not work out your home directory.",
        )
        .with_remedy("Set HOME and try again.")
    })
}

/// Snapshots holding space on copy-on-write filesystems. `STO-17`.
///
/// **Attribution only.** These are reported so their space lands in a named category rather than in
/// `Unknown` — a user should be able to see that 40 GiB is held by snapper. nix does not offer to
/// delete them: a snapper or Timeshift snapshot may be somebody's only route back from a bad
/// upgrade, and that decision is not one to make on their behalf. Deleting them is backlog, behind
/// explicit opt-in and its own design review.
#[tauri::command]
pub(crate) fn snapshots() -> Vec<Snapshot> {
    cow::snapshots()
}

/// What could be reclaimed, and at what cost. `STO-3`.
///
/// Computes but changes nothing. The returned [`Preview`] carries a ticket, and [`reclaim_execute`]
/// will not act without it — so there is no path from the UI to a deletion that skips this step.
#[tauri::command]
pub(crate) fn reclaim_preview(state: State<'_, AppState>) -> Result<Preview> {
    let token = nix_core::op::CancelToken::new();
    state
        .reclaim
        .preview(&state.categories, &state.guard(), &token)
        .map_err(|e| e.context("working out what can be reclaimed"))
}

/// Reclaim a subset of the outstanding preview. `STO-4`.
///
/// `ticket` must match the preview the user was shown, and `selection` must name items from it.
/// Both guards — the protection rules and the time-of-check comparison — run again per item, at the
/// moment of acting.
#[tauri::command]
pub(crate) fn reclaim_execute(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket: Ticket,
    selection: Vec<u64>,
) -> Result<Report> {
    let token = nix_core::op::CancelToken::new();
    let (id, _) = state.operations.start();
    let emitter = app.clone();

    let report = state.reclaim.execute(
        ticket,
        &selection,
        &state.guard(),
        &token,
        move |done, total| {
            let progress = Progress::new(id, done as u64)
                .with_total(total as u64)
                .with_message(format!("Reclaiming {} of {total}", done + 1));
            if let Err(e) = emitter.emit(EVENT_PROGRESS, &progress) {
                tracing::warn!(error = %e, "could not emit reclaim progress");
            }
        },
    );

    state.operations.finish(id);

    match &report {
        Ok(r) => tracing::info!(
            freed = r.freed,
            reclaimed = r.reclaimed_count,
            skipped = r.skipped_count,
            failed = r.failed_count,
            agrees = ?r.measurement_agrees,
            "reclaim finished"
        ),
        Err(e) => tracing::warn!(error = %e, "reclaim failed"),
    }

    report.map_err(|e| e.context("reclaiming space"))
}

/// Discard the outstanding preview, e.g. on navigating away, so its ticket cannot be used later.
#[tauri::command]
pub(crate) fn reclaim_clear(state: State<'_, AppState>) {
    state.reclaim.clear();
}

/// The paths nix refuses to touch. A user should be able to read what is protected on their behalf.
#[tauri::command]
pub(crate) fn protected_paths() -> Vec<Refusal> {
    nix_core::protect::Guard::built_in_rules()
}

/// A deliberately slow operation, so progress, cancellation and the terminal event can be verified
/// before any real work exists.
///
/// Phase 0 scaffolding. It exists to satisfy the M0 gate — "a typed command round-trips, and a
/// failure at any layer produces a specific message" — and is removed once the filesystem scanner
/// (task 1.3) provides a real long operation.
#[tauri::command]
pub(crate) fn demo_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    steps: u64,
    fail_at: Option<u64>,
) -> OperationId {
    let (id, token) = state.operations.start();
    let total = steps.max(1);

    std::thread::spawn(move || {
        let outcome = (|| -> Result<()> {
            for step in 1..=total {
                token.check()?;

                if Some(step) == fail_at {
                    return Err(AppError::new(
                        ErrorCode::Io,
                        format!("The demo operation failed at step {step} of {total}."),
                    )
                    .with_remedy("Nothing was changed. This command exists to test error handling.")
                    .context("running the demo operation"));
                }

                std::thread::sleep(std::time::Duration::from_millis(120));

                let progress = Progress::new(id, step)
                    .with_total(total)
                    .with_message(format!("Step {step} of {total}"));
                if let Err(e) = app.emit(EVENT_PROGRESS, &progress) {
                    tracing::warn!(error = %e, "could not emit progress");
                }
            }
            Ok(())
        })();

        let completion = Completion::from_result(id, outcome);
        if let Err(e) = app.emit(EVENT_DONE, &completion) {
            tracing::warn!(error = %e, "could not emit completion");
        }
        // Always release the token, however the worker exited.
        if let Some(state) = app.try_state::<AppState>() {
            state.operations.finish(id);
        }
    });

    id
}

/// Fail on purpose, with a chosen error class.
///
/// Phase 0 scaffolding for the M0 gate: it lets the error surface be exercised for every code
/// without waiting for a real failure. Stacer had no way to do this because it had no error
/// surface at all.
#[tauri::command]
pub(crate) fn demo_failure(code: String) -> Result<()> {
    let err = match code.as_str() {
        "cancelled" => AppError::cancelled(),
        "auth_denied" => AppError::new(
            ErrorCode::AuthDenied,
            "Administrator rights were not granted.",
        )
        .with_remedy("The action was cancelled. Nothing was changed."),
        "not_found" => AppError::new(ErrorCode::NotFound, "That file no longer exists.")
            .with_path("/tmp/does-not-exist")
            .with_remedy("Refresh and try again."),
        "unsupported" => AppError::unsupported("Snap"),
        "refused" => {
            AppError::refused("That path is protected and cannot be reclaimed.").with_path("/boot")
        }
        "internal" => AppError::internal("A deliberate internal error."),
        other => AppError::invalid_input(format!("Unknown demo error code: {other}")).with_remedy(
            "Pass one of: cancelled, auth_denied, not_found, unsupported, refused, internal.",
        ),
    };
    Err(err.context("demonstrating the error surface"))
}
