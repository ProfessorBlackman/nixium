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
mod tray;

use tauri::Manager;

use state::AppState;

/// Build and run the application.
///
/// Logging is installed **before** anything else can fail, so a startup problem is recorded rather
/// than lost. Stacer wrote a file logger and never installed it, and so had no error reporting at
/// all; [`nix_core::logging::is_initialised`] exists to assert we have not repeated that.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Whether a tray icon actually exists.
///
/// Managed state rather than re-reading the setting, because the two can disagree: a desktop with no
/// StatusNotifier host cannot show a tray however the setting reads, and hiding the window to a tray
/// that does not exist leaves a running process with no way back to it.
struct TrayPresent(bool);

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
            commands::packages_list,
            commands::package_measure,
            commands::packages_residual,
            commands::packages_removal_preview,
            commands::packages_remove,
            commands::search_start,
            commands::apt_sources_list,
            commands::apt_source_set_enabled,
            commands::apt_source_remove,
            commands::autostart_list,
            commands::autostart_set_enabled,
            commands::autostart_add,
            commands::autostart_remove,
            commands::hosts_load,
            commands::hosts_save,
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
        .setup(|app| {
            // The tray is built here rather than in the builder chain because it needs the managed
            // state to read the setting, and state is only available once the app exists.
            let tray_exists = tray::install(app.handle());
            app.manage(TrayPresent(tray_exists));

            // `--hide` starts without showing the window: for a session autostart entry, where popping
            // a window open at login is exactly what people turn autostart off to avoid. Only honoured
            // with a tray, or there would be no way to get the window back.
            if tray_exists && std::env::args().any(|a| a == "--hide") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let tray_exists = window
                    .try_state::<TrayPresent>()
                    .is_some_and(|present| present.0);

                if tray::hide_instead_of_closing(window, tray_exists) {
                    // Hidden, not closed — so in-flight work is left alone and the close is refused.
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                    }
                    return;
                }

                // Cancel in-flight work when the window closes, so no worker outlives the UI that
                // started it (P9: nothing keeps running once nobody is listening).
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
