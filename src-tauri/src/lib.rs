//! The Tauri application shell.
//!
//! This crate owns the window, the command surface and the event plumbing. All system access lives
//! in [`nix_core`]; anything here that starts reading `/proc` or spawning processes belongs in that
//! crate instead. See `docs/ARCHITECTURE.md`.

mod commands;
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
