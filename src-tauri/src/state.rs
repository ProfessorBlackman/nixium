// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Application state shared by every command.

use std::sync::Mutex;

use nix_core::error::Result;
use nix_core::metrics::{Alerts, Pipeline, Subscription};
use nix_core::op;
use nix_core::protect::Guard;
use nix_core::reclaim::{Registry, Session};
use nix_core::settings::{Settings, Store};

/// Everything a command may need, owned by Tauri and reachable through `State<AppState>`.
pub(crate) struct AppState {
    /// Settings, kept in memory so reads do not touch the disk on every call.
    settings: Mutex<Settings>,
    /// Where settings persist.
    store: Store,
    /// In-flight long operations, so they can be cancelled by id.
    pub(crate) operations: op::Registry,
    /// The outstanding reclaim preview. One at a time, so a stale ticket cannot be replayed.
    pub(crate) reclaim: Session,
    /// The categories nix knows how to reclaim.
    pub(crate) categories: Registry,
    /// A warning raised while loading settings, surfaced once the frontend is ready to show it.
    startup_warning: Mutex<Option<nix_core::error::AppError>>,
    /// The live metrics pipeline. `MON-1`.
    ///
    /// Built at startup but **idle**: its worker blocks until a view subscribes, so a cold start
    /// samples nothing (§P9).
    pub(crate) metrics: Pipeline,
    /// The subscription held while a monitoring view is mounted.
    ///
    /// Dropping it is what pauses sampling, so this is `None` whenever no view wants metrics — the
    /// state and the behaviour cannot drift apart, because the state *is* the behaviour.
    pub(crate) metrics_subscription: Mutex<Option<Subscription>>,
    /// Firing state for the threshold alerts (`MON-6`).
    ///
    /// Held here rather than in the frontend because hysteresis and cooldown are *memory*: a rule
    /// that forgets it already fired notifies every second, which is the behaviour the whole design
    /// exists to prevent.
    pub(crate) alerts: Mutex<Alerts>,
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
            reclaim: Session::new(),
            categories: Registry::with_defaults(),
            startup_warning: Mutex::new(warning),
            metrics: Pipeline::new(),
            metrics_subscription: Mutex::new(None),
            alerts: Mutex::new(Alerts::new()),
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

    /// The protection rules, built from the user's current settings.
    ///
    /// Rebuilt on each call rather than cached: the user can change their exclusions at any time,
    /// and the executor re-checks them immediately before acting.
    pub(crate) fn guard(&self) -> Guard {
        Guard::from_settings(&self.settings())
    }

    /// Take the startup warning, if any. Returns it once and then forgets it, so the frontend
    /// shows it on the first poll and never again.
    pub(crate) fn take_startup_warning(&self) -> Option<nix_core::error::AppError> {
        self.startup_warning.lock().ok().and_then(|mut w| w.take())
    }
}
