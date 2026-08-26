//! Application state shared by every command.

use std::sync::Mutex;

use nix_core::error::Result;
use nix_core::op;
use nix_core::settings::{Settings, Store};

/// Everything a command may need, owned by Tauri and reachable through `State<AppState>`.
pub(crate) struct AppState {
    /// Settings, kept in memory so reads do not touch the disk on every call.
    settings: Mutex<Settings>,
    /// Where settings persist.
    store: Store,
    /// In-flight long operations, so they can be cancelled by id.
    pub(crate) operations: op::Registry,
    /// A warning raised while loading settings, surfaced once the frontend is ready to show it.
    startup_warning: Mutex<Option<nix_core::error::AppError>>,
}

impl AppState {
    /// Load settings and build the state. Never fails: a bad settings file yields defaults plus a
    /// warning the frontend collects on start.
    pub(crate) fn load() -> Self {
        let (store, warning) = match Store::discover() {
            Ok(store) => {
                let loaded = store.load();
                (store, loaded.warning.or(None))
            }
            Err(e) => {
                // No config directory. Use a path that will fail on save, and say why.
                (Store::at("/nonexistent/nix/settings.json"), Some(e))
            }
        };

        let settings = store.load().settings;

        Self {
            settings: Mutex::new(settings),
            store,
            operations: op::Registry::new(),
            startup_warning: Mutex::new(warning),
        }
    }

    /// Current settings.
    pub(crate) fn settings(&self) -> Settings {
        self.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Replace and persist settings.
    pub(crate) fn save_settings(&self, next: Settings) -> Result<Settings> {
        self.store.save(&next)?;
        if let Ok(mut current) = self.settings.lock() {
            *current = next.clone();
        }
        Ok(next)
    }

    /// Take the startup warning, if any. Returns it once and then forgets it, so the frontend
    /// shows it on the first poll and never again.
    pub(crate) fn take_startup_warning(&self) -> Option<nix_core::error::AppError> {
        self.startup_warning.lock().ok().and_then(|mut w| w.take())
    }
}
