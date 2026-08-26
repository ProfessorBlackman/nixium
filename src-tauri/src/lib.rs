//! The Tauri application shell.
//!
//! This crate owns the window, the command surface and the event plumbing. All system access lives
//! in [`nix_core`]; anything in here that starts reading `/proc` or spawning processes is in the
//! wrong crate.

/// Versions of the app and core crates, so the frontend can prove the two halves were built
/// together. Placeholder command until the real IPC contract lands (task 0.3, `FND-2`).
#[tauri::command]
fn versions() -> serde_json::Value {
    serde_json::json!({
        "app": env!("CARGO_PKG_VERSION"),
        "core": nix_core::VERSION,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![versions])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
