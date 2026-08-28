// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The Tauri application shell.
//!
//! This crate owns the window, the command surface and the event plumbing. All system access lives
//! in [`nix_core`]; anything here that starts reading `/proc` or spawning processes belongs in that
//! crate instead. See `docs/ARCHITECTURE.md`.

mod commands;
mod snapshot;
mod state;

use tauri::Manager;

use state::AppState;

/// Build and run the application.
///
/// Logging is installed **before** anything else can fail, so a startup problem is recorded rather
/// than lost. Stacer wrote a file logger and never installed it, and so had no error reporting at
/// all; [`nix_core::logging::is_initialised`] exists to assert we have not repeated that.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // `STO-16`: the growth-history timer's `ExecStart` is a subcommand of this same binary rather
    // than a second artefact — two executables is two things to version, package and sign, and an
    // `ExecStart` naming something that no longer exists is a job that fails silently every day.
    //
    // Handled before Tauri is touched, because this path must never open a window.
    if let Some(code) = snapshot::run_if_requested() {
        std::process::exit(code);
    }

    let app_state = AppState::load();
    let (_log_guard, log_problem) = nix_core::logging::init(app_state.settings().log_level);

    if let Some(problem) = &log_problem {
        // Cannot be logged to a file by definition, so stderr is the only honest channel.
        eprintln!("nix: {problem}");
    }

    tracing::info!(
        app = env!("CARGO_PKG_VERSION"),
        core = nix_core::VERSION,
        "starting"
    );

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::versions,
            commands::diagnostics,
            commands::capabilities,
            commands::capabilities_refresh,
            commands::filesystems,
            commands::scan_start,
            commands::scan_cached,
            commands::units_list,
            commands::unit_files,
            commands::units_timers,
            commands::unit_act,
            commands::units_watch,
            commands::unit_logs,
            commands::processes_list,
            commands::processes_forget,
            commands::process_signal,
            commands::process_renice,
            commands::process_detail,
            commands::process_tree,
            commands::alerts_evaluate,
            commands::reclaim_last_total,
            commands::metrics_subscribe,
            commands::metrics_unsubscribe,
            commands::metrics_history,
            commands::metrics_sampling,
            commands::largest_files,
            commands::history_samples,
            commands::history_series,
            commands::history_growth,
            commands::history_clear,
            commands::history_snapshot_now,
            commands::timer_state,
            commands::timer_install,
            commands::timer_uninstall,
            commands::duplicates_find,
            commands::scan_cache_clear,
            commands::scan_cache_size,
            commands::home_directory,
            commands::snapshots,
            commands::reclaim_preview,
            commands::reclaim_execute,
            commands::reclaim_clear,
            commands::protected_paths,
            commands::settings_get,
            commands::settings_save,
            commands::startup_warning,
            commands::operation_cancel,
            commands::operation_count,
            commands::helper_probe,
            commands::demo_operation,
            commands::demo_failure,
        ])
        .on_window_event(|window, event| {
            // Cancel in-flight work when the window closes, so no worker outlives the UI that
            // started it (P9: nothing keeps running once nobody is listening).
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                if let Some(state) = window.try_state::<AppState>() {
                    state.operations.cancel_all();
                }
            }
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "the application could not start");
        eprintln!("nix: could not start: {e}");
        std::process::exit(1);
    }
}
