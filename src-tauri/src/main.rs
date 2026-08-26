// Prevents an additional console window on Windows in release. Kept from the scaffold; nix targets
// Linux only, but removing it costs nothing to leave correct.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    nix_app_lib::run()
}
