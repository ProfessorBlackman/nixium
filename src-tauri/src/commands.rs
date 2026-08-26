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
use nix_core::error::{AppError, ErrorCode, Result};
use nix_core::helper::{self, Op, OpResult};
use nix_core::logging::{self, Diagnostics};
use nix_core::op::{Completion, OperationId, Progress};
use nix_core::settings::Settings;
use nix_core::{fs as nixfs, scan};
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
pub(crate) fn capabilities() -> caps::Snapshot {
    caps::registry().snapshot()
}

/// Re-probe capabilities. Call after anything that could install or remove a tool.
#[tauri::command]
pub(crate) fn capabilities_refresh() -> caps::Snapshot {
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
