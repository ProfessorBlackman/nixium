// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! The tray icon and what closing the window means. `PLT-3`.
//!
//! # Off by default
//!
//! A tray icon is a claim on the user's panel and a process that outlives the window they closed.
//! Neither is something to assume they wanted, and Stacer had no tray at all — so nobody is losing
//! behaviour they had by this starting off.
//!
//! # The acceptance criterion is about what stops, not what shows
//!
//! *Hidden in tray with no alerts armed, CPU is ~0.* That does not follow from hiding a window.
//! Hiding does not unmount anything — the webview keeps running, the monitoring view keeps its
//! subscription, and the sampler keeps sampling for a window nobody can see. Which is precisely the
//! Stacer behaviour §P9 exists to avoid, arrived at by a different route.
//!
//! So hiding **drops the metrics subscription**, and showing does not restore it — the view asks again
//! when it is next visible, which is the same path a fresh mount takes and therefore the one already
//! exercised. The exception is an armed alert rule: a threshold alert that stops watching while the
//! window is hidden is not an alert, it is a decoration. That is the one case where sampling continues,
//! and it is a case the user opted into by writing a rule.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// Build the tray icon, if the user has asked for one.
///
/// Returns whether one was built, so the caller can decide what closing the window means without
/// re-reading the setting — a tray that failed to build and a setting that says it should exist would
/// otherwise leave the window hiding to nothing.
pub(crate) fn install(app: &AppHandle) -> bool {
    let Some(state) = app.try_state::<AppState>() else {
        return false;
    };
    if !state.settings().tray_enabled {
        return false;
    }

    let show = match MenuItem::with_id(app, "show", "Show nix", true, None::<&str>) {
        Ok(item) => item,
        Err(e) => {
            tracing::warn!(error = %e, "could not build the tray menu");
            return false;
        }
    };
    let quit = match MenuItem::with_id(app, "quit", "Quit", true, None::<&str>) {
        Ok(item) => item,
        Err(e) => {
            tracing::warn!(error = %e, "could not build the tray menu");
            return false;
        }
    };
    let Ok(menu) = Menu::with_items(app, &[&show, &quit]) else {
        return false;
    };

    let built = TrayIconBuilder::with_id("nix")
        .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
            // Falls back to an empty image rather than failing: a tray with no icon is still
            // clickable, and refusing to start over an icon would be worse.
            tauri::image::Image::new_owned(vec![0; 4], 1, 1)
        }))
        .tooltip("nix")
        .menu(&menu)
        // Without this, the menu opens on left click too, which on most desktops is where "show the
        // window" belongs.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => reveal(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                reveal(tray.app_handle());
            }
        })
        .build(app);

    match built {
        Ok(_) => true,
        Err(e) => {
            // A desktop with no StatusNotifier host — some minimal window managers — cannot show one.
            // Reported rather than fatal, and the caller then treats close as quit.
            tracing::warn!(error = %e, "could not create a tray icon; closing will quit");
            false
        }
    }
}

/// Bring the window back and resume whatever it needs.
fn reveal(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if let Err(e) = window.show() {
        tracing::warn!(error = %e, "could not show the window");
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!(error = %e, "could not focus the window");
    }
    // Sampling is deliberately *not* restarted here. The view asks for it when it becomes visible,
    // which is the same path a fresh mount takes — restoring it from here would be a second way to
    // start sampling, and two ways to start one thing is how a subscription gets leaked.
}

/// What a close request means, given whether a tray exists.
///
/// Returns `true` when the window was hidden rather than closed, so the caller knows to prevent the
/// close.
pub(crate) fn hide_instead_of_closing(window: &tauri::Window, tray_exists: bool) -> bool {
    if !tray_exists {
        return false;
    }
    let Some(state) = window.try_state::<AppState>() else {
        return false;
    };
    if !state.settings().close_to_tray {
        return false;
    }

    if let Err(e) = window.hide() {
        tracing::warn!(error = %e, "could not hide the window; closing instead");
        return false;
    }

    pause_sampling_unless_alerting(&state);
    true
}

/// Stop sampling while hidden, unless an alert rule is armed.
///
/// This is the whole acceptance criterion. Hiding a window stops nothing on its own: the webview keeps
/// running and the monitoring view keeps its subscription, so the sampler would keep working for a
/// window nobody can see.
///
/// An armed alert rule is the exception, and the only one. A threshold alert that stops watching when
/// the window is hidden is not an alert — and it is a case the user opted into by writing the rule.
pub(crate) fn pause_sampling_unless_alerting(state: &AppState) {
    if !state.settings().alert_rules.is_empty() {
        tracing::debug!("hidden with alert rules armed, so sampling continues");
        return;
    }

    let mut held = match state.metrics_subscription.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    // Dropping the subscription is what pauses the pipeline. There is no separate "stop" to forget.
    *held = None;
    tracing::debug!("hidden with no alerts armed, so sampling is paused");
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix_core::metrics::{Metric, Rule};

    /// The acceptance criterion, tested on the rule rather than through a window.
    ///
    /// A Tauri window cannot be created in a test — there is no display, and `AppState` cannot be
    /// built into a `State<'_>` outside the app. What *can* be tested is the decision the window event
    /// delegates to, which is where the whole behaviour lives: hiding drops the subscription unless a
    /// rule is armed. That is the sentence the criterion is made of.
    fn state_with(rules: Vec<Rule>) -> AppState {
        let mut state = AppState::load();
        state.set_alert_rules_for_test(rules);
        state
    }

    #[test]
    fn hiding_with_no_alerts_stops_sampling() {
        let state = state_with(Vec::new());

        // Subscribe, as a mounted monitoring view does.
        {
            let mut held = state
                .metrics_subscription
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *held = Some(state.metrics.subscribe());
        }
        assert!(state.metrics.is_sampling(), "the view is watching");

        pause_sampling_unless_alerting(&state);

        assert!(
            !state.metrics.is_sampling(),
            "hidden with nothing armed, nothing should still be sampling — hiding a window does not \
             unmount the view, so without this the sampler runs for a window nobody can see"
        );
    }

    /// The one exception, and it is the user's own choice: they wrote a rule.
    #[test]
    fn hiding_with_an_alert_armed_keeps_sampling() {
        let state = state_with(vec![Rule::fraction(Metric::CpuUsage, 0.9)]);

        {
            let mut held = state
                .metrics_subscription
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *held = Some(state.metrics.subscribe());
        }

        pause_sampling_unless_alerting(&state);

        assert!(
            state.metrics.is_sampling(),
            "an alert that stops watching when the window is hidden is not an alert"
        );
    }

    /// Pausing twice is not an error, and the second does not resurrect anything.
    #[test]
    fn pausing_when_already_paused_is_harmless() {
        let state = state_with(Vec::new());
        pause_sampling_unless_alerting(&state);
        pause_sampling_unless_alerting(&state);
        assert!(!state.metrics.is_sampling());
    }
}
