// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! systemd units, over D-Bus. `SVC-1` to `SVC-4`, decision D10.
//!
//! # One round trip, not `1 + 2N` subprocesses
//!
//! Stacer ran `systemctl list-units`, then two more `systemctl show` calls **per unit** to fill in what
//! the list did not carry. On this machine's 764 units that is 1,529 process spawns to draw one screen.
//! `ListUnits` returns everything the table needs in a single call.
//!
//! # What Stacer's filter dropped
//!
//! It listed with `--state=enabled,disabled`, which silently excludes:
//!
//! - **`static`** units — the majority of a modern system. They have no `[Install]` section, so they
//!   are neither enabled nor disabled; they are pulled in by dependency. Filtering them out hides most
//!   of what is actually running.
//! - **`masked`** units, which is the state a user most needs to *see*, because it is the one that
//!   makes something refuse to start no matter what asks for it.
//! - **`generated`** and **`transient`** units — anything from a generator or created at runtime, which
//!   is how mount units and most container scopes appear.
//!
//! And it discarded template units with a regex, which loses every instantiated `foo@bar.service`.
//!
//! # Why polkit needs nothing from nix here
//!
//! `StartUnit` and friends ask polkit themselves, so a user is prompted by systemd's own action with
//! systemd's own wording, and nix writes no privileged code for any of this (§P6). The prompt is
//! `auth_admin_keep`, exactly like nix's own helper policy, so one authorisation covers a session.
//!
//! A refused authorisation arrives as a D-Bus error, not as a silent success — which is `SVC-2`'s
//! acceptance criterion and the specific thing Stacer got wrong, since it read only stdout and never
//! checked an exit status.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, ErrorCode, Result};
// Only the D-Bus error mapping builds a `Cause`; importing it unconditionally warns in the feature-off
// build, which is the configuration the privileged helper links.
#[cfg(feature = "dbus")]
use crate::error::Cause;

/// Which systemd instance to talk to. `SVC-4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Scope {
    /// The system manager: services, mounts, the machine's own units.
    System,
    /// The user's own manager. Absent from Stacer entirely, and where a desktop session's units live.
    User,
}

/// One unit, as `ListUnits` reports it.
///
/// The field order is the D-Bus signature `(ssssssouso)` and is not rearranged, because the tuple is
/// positional and reordering it silently shifts every value by one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Unit {
    /// `nginx.service`, `home.mount`, `logrotate.timer`.
    pub name: String,
    pub description: String,
    /// Whether systemd could read its definition: `loaded`, `not-found`, `masked`, `bad-setting`.
    pub load_state: String,
    /// `active`, `inactive`, `failed`, `activating`, `deactivating`.
    pub active_state: String,
    /// The finer state: `running`, `exited`, `dead`, `waiting`.
    pub sub_state: String,
    /// The unit this one follows, when it is an alias for another's state.
    pub following: String,
    /// Which manager it came from, so a table can mix both without confusing them.
    pub scope: Scope,
}

impl Unit {
    /// The part after the last dot: `service`, `timer`, `mount`, `socket`.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(_, kind)| kind)
    }

    /// Whether this is an instance of a template — `getty@tty1.service` from `getty@.service`.
    ///
    /// Stacer discarded these with a regex, which loses every instantiated unit on the system: every
    /// virtual console, every per-device mount, most container scopes.
    #[must_use]
    pub fn is_instance(&self) -> bool {
        self.name
            .rsplit_once('.')
            .is_some_and(|(stem, _)| stem.contains('@') && !stem.ends_with('@'))
    }

    /// Whether it is masked — refusing to start whatever asks for it.
    ///
    /// The state a user most needs to see, and the one Stacer's filter hid.
    #[must_use]
    pub fn is_masked(&self) -> bool {
        self.load_state == "masked"
    }

    /// Whether it failed. What a service list is usually opened to find out.
    #[must_use]
    pub fn has_failed(&self) -> bool {
        self.active_state == "failed" || self.sub_state == "failed"
    }
}

/// A unit file on disk, from `ListUnitFiles`. `SVC-1`.
///
/// Separate from [`Unit`] because the two answer different questions: `ListUnits` reports what is
/// *loaded*, and a disabled unit that has never run appears in neither its output nor Stacer's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnitFile {
    pub name: String,
    /// `enabled`, `disabled`, `static`, `masked`, `generated`, `transient`, `indirect`, `alias`.
    pub state: String,
    pub scope: Scope,
}

impl UnitFile {
    /// Whether this unit can be enabled or disabled at all.
    ///
    /// A `static` unit has no `[Install]` section, so offering an enable button for it would produce
    /// an error systemd was always going to return. `generated` and `transient` units do not exist on
    /// disk to be enabled.
    #[must_use]
    pub fn is_installable(&self) -> bool {
        matches!(
            self.state.as_str(),
            "enabled" | "disabled" | "enabled-runtime"
        )
    }
}

/// A timer's schedule. `SVC-4`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Timer {
    pub name: String,
    /// The unit it starts.
    pub unit: String,
    /// Microseconds since the epoch, or `None` when it will not fire again — a one-shot that has run,
    /// or a calendar timer whose condition can no longer be met. `None` rather than zero, because
    /// systemd reports both `0` and `u64::MAX` for "never" depending on which property is asked.
    #[ts(type = "number | null")]
    pub next_elapse_us: Option<u64>,
    /// Whether it is scheduled to fire again at all.
    ///
    /// Separate from [`Timer::next_elapse_us`] because a **monotonic** timer — one using `OnBootSec`
    /// or `OnUnitActiveSec` — is genuinely scheduled but has no wall-clock instant to show. Reporting
    /// it as "never" would be wrong, and converting its monotonic offset into a date would be an
    /// invention.
    pub scheduled: bool,
    /// Seconds from now until it next fires, for either kind of schedule.
    ///
    /// The figure a person actually wants, and computable for both without inventing anything: a
    /// calendar timer's instant less the wall clock, or a monotonic timer's offset less the current
    /// uptime. The monotonic arithmetic is exact because both sides are the same clock — which is why
    /// it is done this way rather than by adding the boot time to the offset, a conversion that drifts
    /// once a machine has suspended.
    ///
    /// Negative when a timer is overdue, which happens legitimately while its service is still running.
    #[ts(type = "number | null")]
    pub seconds_until: Option<i64>,
    #[ts(type = "number | null")]
    pub last_trigger_us: Option<u64>,
    pub scope: Scope,
}

/// systemd's sentinel for "no time". Reported as `0` by some properties and `u64::MAX` by others.
const NEVER: u64 = u64::MAX;

/// Normalise a systemd timestamp property.
///
/// Both `0` and `u64::MAX` mean "never" depending on which property was asked, and rendering either as
/// a date gives 1970 or a year past 500,000. Both become `None`.
#[must_use]
pub fn timestamp(raw: u64) -> Option<u64> {
    (raw != 0 && raw != NEVER).then_some(raw)
}

/// Seconds since the epoch, now.
#[must_use]
// Only the D-Bus timer reader needs this; no test calls it directly.
#[cfg(feature = "dbus")]
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Seconds since boot, from `/proc/uptime`.
#[must_use]
// The D-Bus reader, plus `this_machines_uptime_reads`, which is not gated on the feature.
#[cfg(any(feature = "dbus", test))]
pub(crate) fn uptime_seconds() -> Option<f64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// How long until a timer next fires, from whichever schedule it has.
///
/// Pure, so the arithmetic is tested without a bus. A monotonic timer is compared against uptime
/// because both are the same clock, which makes the subtraction exact — adding boot time to the offset
/// instead would drift as soon as the machine suspended.
#[must_use]
pub fn seconds_until(
    realtime_us: Option<u64>,
    monotonic_us: Option<u64>,
    now_unix: i64,
    uptime_seconds: Option<f64>,
) -> Option<i64> {
    if let Some(at) = realtime_us {
        let at = i64::try_from(at / 1_000_000).unwrap_or(i64::MAX);
        return Some(at - now_unix);
    }
    let (offset, uptime) = (monotonic_us?, uptime_seconds?);
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let remaining = (offset as f64 / 1_000_000.0) - uptime;
    #[allow(clippy::cast_possible_truncation)]
    Some(remaining as i64)
}

/// Turn a D-Bus failure into something a user can act on.
///
/// The distinction that matters is **denied versus failed**: `SVC-2`'s criterion is that a refused
/// authorisation is reported as refused. polkit denials arrive as
/// `org.freedesktop.DBus.Error.InteractiveAuthorizationRequired` or `AccessDenied`, and treating those
/// as a generic failure is how Stacer reported a cancelled prompt as success.
#[cfg(feature = "dbus")]
fn from_dbus(error: zbus::Error, doing: &str) -> AppError {
    let name = match &error {
        zbus::Error::MethodError(name, _, _) => name.as_str().to_string(),
        other => other.to_string(),
    };

    let detail = match &error {
        zbus::Error::MethodError(_, message, _) => message.clone().unwrap_or_default(),
        other => other.to_string(),
    };

    if name.contains("AccessDenied") || name.contains("InteractiveAuthorizationRequired") {
        return AppError::new(
            ErrorCode::AuthDenied,
            format!("Administrator rights were not granted, so nothing was changed ({doing})."),
        )
        .with_remedy("The action was cancelled. systemd made no change.")
        .with_cause(Cause::Other { detail });
    }

    if name.contains("NoSuchUnit") {
        return AppError::new(ErrorCode::NotFound, format!("No such unit ({doing})."))
            .with_cause(Cause::Other { detail });
    }

    AppError::new(
        ErrorCode::CommandFailed,
        format!("systemd refused: {doing}"),
    )
    .with_cause(Cause::Other { detail })
}

/// What can be done to a unit. `SVC-2`.
///
/// A closed set, and typed. Each maps to one D-Bus method, so nothing here is assembled from text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Action {
    Start,
    Stop,
    Restart,
    /// Ask it to re-read its configuration, where it supports that.
    Reload,
    /// Start at boot.
    Enable,
    Disable,
    /// Refuse to start, whatever asks.
    Mask,
    Unmask,
}

impl Action {
    /// Whether this changes what happens at boot rather than what is running now.
    ///
    /// Worth distinguishing in the UI: stopping a service and disabling it are different intentions,
    /// and doing one when you meant the other is a surprise on the next reboot.
    #[must_use]
    pub const fn is_persistent(self) -> bool {
        matches!(
            self,
            Self::Enable | Self::Disable | Self::Mask | Self::Unmask
        )
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reload => "reload",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Mask => "mask",
            Self::Unmask => "unmask",
        }
    }
}

#[cfg(feature = "dbus")]
mod dbus {
    use super::{
        Action, AppError, ErrorCode, Result, Scope, Timer, Unit, UnitFile, from_dbus, timestamp,
    };

    /// `(name, description, load, active, sub, following, path, job id, job type, job path)` — the
    /// signature `a(ssssssouso)` that `ListUnits` returns, one entry per unit.
    type UnitRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        zbus::zvariant::OwnedObjectPath,
        u32,
        String,
        zbus::zvariant::OwnedObjectPath,
    );

    /// What an enable, disable, mask or unmask reports back: one `(type, file, destination)` triple per
    /// symlink systemd created or removed.
    type Changes = Vec<(String, String, String)>;

    /// The subset of `org.freedesktop.systemd1.Manager` this needs.
    ///
    /// Declared rather than introspected, so the signatures are checked at compile time against what
    /// systemd documents — and a mismatch is a build failure rather than a runtime surprise.
    #[zbus::proxy(
        interface = "org.freedesktop.systemd1.Manager",
        default_service = "org.freedesktop.systemd1",
        default_path = "/org/freedesktop/systemd1",
        gen_blocking = true,
        gen_async = false
    )]
    trait Manager {
        /// One row of `ListUnits`, in the D-Bus signature's own order.
        ///
        /// Named rather than written inline because the tuple is positional and ten fields wide:
        /// reordering it silently shifts every value by one, and a name makes that visible at the
        /// call site.
        fn list_units(&self) -> zbus::Result<Vec<UnitRow>>;

        /// `(path, state)` for every unit file on disk, including ones never loaded.
        fn list_unit_files(&self) -> zbus::Result<Vec<(String, String)>>;

        fn start_unit(
            &self,
            name: &str,
            mode: &str,
        ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        fn stop_unit(
            &self,
            name: &str,
            mode: &str,
        ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        fn restart_unit(
            &self,
            name: &str,
            mode: &str,
        ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
        fn reload_unit(
            &self,
            name: &str,
            mode: &str,
        ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

        fn enable_unit_files(
            &self,
            files: &[&str],
            runtime: bool,
            force: bool,
        ) -> zbus::Result<(bool, Changes)>;
        fn disable_unit_files(&self, files: &[&str], runtime: bool) -> zbus::Result<Changes>;
        fn mask_unit_files(
            &self,
            files: &[&str],
            runtime: bool,
            force: bool,
        ) -> zbus::Result<Changes>;
        fn unmask_unit_files(&self, files: &[&str], runtime: bool) -> zbus::Result<Changes>;

        fn reload(&self) -> zbus::Result<()>;

        /// Ask systemd to emit change signals. `SVC-3` — without this it stays quiet.
        fn subscribe(&self) -> zbus::Result<()>;

        #[zbus(signal)]
        fn unit_new(&self, name: String, unit: zbus::zvariant::OwnedObjectPath)
        -> zbus::Result<()>;

        #[zbus(signal)]
        fn unit_removed(
            &self,
            name: String,
            unit: zbus::zvariant::OwnedObjectPath,
        ) -> zbus::Result<()>;

        #[zbus(signal)]
        fn job_removed(
            &self,
            id: u32,
            job: zbus::zvariant::OwnedObjectPath,
            unit: String,
            result: String,
        ) -> zbus::Result<()>;
    }

    /// A timer unit's schedule properties.
    #[zbus::proxy(
        interface = "org.freedesktop.systemd1.Timer",
        default_service = "org.freedesktop.systemd1",
        gen_blocking = true,
        gen_async = false
    )]
    trait TimerUnit {
        // Named explicitly. zbus derives a property name from the method name by capitalising each
        // word, which gives `NextElapseUsecRealtime` — and systemd's property is
        // `NextElapseUSecRealtime`, with `USec` capitalised as an abbreviation.
        //
        // The mismatch does not fail loudly: the read returns an error, `.ok()` turns it into `None`,
        // and `None` renders as "never". Every timer on this machine reported that it would never fire
        // again, which is a perfectly plausible-looking answer and completely wrong.
        #[zbus(property, name = "NextElapseUSecRealtime")]
        fn next_elapse_usec_realtime(&self) -> zbus::Result<u64>;

        /// Monotonic scheduling, for timers using `OnBootSec` or `OnUnitActiveSec` rather than a
        /// calendar. Not comparable with a wall clock, so it is only used to tell "scheduled" from
        /// "never".
        #[zbus(property, name = "NextElapseUSecMonotonic")]
        fn next_elapse_usec_monotonic(&self) -> zbus::Result<u64>;

        #[zbus(property, name = "LastTriggerUSec")]
        fn last_trigger_usec(&self) -> zbus::Result<u64>;

        #[zbus(property)]
        fn unit(&self) -> zbus::Result<String>;
    }

    /// Connect to one manager.
    ///
    /// A user manager may simply not exist — in a Flatpak sandbox, or over `ssh` with no session bus —
    /// and that is reported rather than treated as a fault, because `SVC-4` is additive: a machine with
    /// no user units still has system units worth showing.
    fn connect(scope: Scope) -> Result<zbus::blocking::Connection> {
        let result = match scope {
            Scope::System => zbus::blocking::Connection::system(),
            Scope::User => zbus::blocking::Connection::session(),
        };
        result.map_err(|e| {
            AppError::new(
                ErrorCode::Unsupported,
                match scope {
                    Scope::System => "Could not reach the system service manager.".to_string(),
                    Scope::User => "This session has no user service manager.".to_string(),
                },
            )
            .with_remedy(match scope {
                Scope::System => "systemd may not be this system's init.",
                Scope::User => {
                    "User units are unavailable here — inside a sandbox, or with no login session."
                }
            })
            .with_cause(crate::error::Cause::Other {
                detail: e.to_string(),
            })
        })
    }

    fn manager(scope: Scope) -> Result<ManagerProxy<'static>> {
        let connection = connect(scope)?;
        ManagerProxy::new(&connection).map_err(|e| from_dbus(e, "reach the service manager"))
    }

    /// Every loaded unit. `SVC-1`.
    pub fn list(scope: Scope) -> Result<Vec<Unit>> {
        let proxy = manager(scope)?;
        let raw = proxy.list_units().map_err(|e| from_dbus(e, "list units"))?;

        Ok(raw
            .into_iter()
            .map(
                |(name, description, load_state, active_state, sub_state, following, ..)| Unit {
                    name,
                    description,
                    load_state,
                    active_state,
                    sub_state,
                    following,
                    scope,
                },
            )
            .collect())
    }

    /// Every unit *file*, loaded or not. `SVC-1`.
    ///
    /// `ListUnits` omits a disabled unit that has never run, so a list built from it alone cannot offer
    /// to enable anything that is not already going.
    pub fn list_files(scope: Scope) -> Result<Vec<UnitFile>> {
        let proxy = manager(scope)?;
        let raw = proxy
            .list_unit_files()
            .map_err(|e| from_dbus(e, "list unit files"))?;

        Ok(raw
            .into_iter()
            .map(|(path, state)| UnitFile {
                // The property is a path; the name is its last component.
                name: path
                    .rsplit_once('/')
                    .map_or(path.clone(), |(_, name)| name.to_string()),
                state,
                scope,
            })
            .collect())
    }

    /// Every timer, with its schedule. `SVC-4`.
    pub fn timers(scope: Scope) -> Result<Vec<Timer>> {
        let connection = connect(scope)?;
        let proxy = ManagerProxy::new(&connection)
            .map_err(|e| from_dbus(e, "reach the service manager"))?;
        let raw = proxy.list_units().map_err(|e| from_dbus(e, "list units"))?;

        // Read once for the batch rather than per timer: sixteen timers would otherwise mean sixteen
        // reads of the same two files, and a clock that moved between them.
        let now_unix = super::now_unix();
        let uptime = super::uptime_seconds();

        let mut timers = Vec::new();
        for (name, .., path, _, _, _) in raw {
            if !name.ends_with(".timer") {
                continue;
            }
            // A timer that cannot be queried is skipped rather than shown with invented times.
            let Ok(timer) = TimerUnitProxy::builder(&connection)
                .path(path.clone())
                .and_then(zbus::blocking::proxy::Builder::build)
            else {
                continue;
            };
            // A calendar timer carries a realtime elapse; a monotonic one carries only the monotonic
            // figure, which is not a wall-clock instant. Reporting the realtime value where there is
            // one and marking the rest as scheduled-but-not-datable is honest; converting a monotonic
            // offset into a date would invent one.
            let realtime = timer.next_elapse_usec_realtime().ok().and_then(timestamp);
            let monotonic = timer.next_elapse_usec_monotonic().ok().and_then(timestamp);
            timers.push(Timer {
                unit: timer.unit().unwrap_or_default(),
                next_elapse_us: realtime,
                scheduled: realtime.is_some() || monotonic.is_some(),
                seconds_until: super::seconds_until(realtime, monotonic, now_unix, uptime),
                last_trigger_us: timer.last_trigger_usec().ok().and_then(timestamp),
                name,
                scope,
            });
        }
        timers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(timers)
    }

    /// Carry out one action. `SVC-2`.
    ///
    /// systemd asks polkit itself, so a denial arrives as a D-Bus error and is reported as a denial —
    /// never as a success, which is the criterion.
    ///
    /// `mode` is `"replace"`: queue the job and displace any conflicting one. `"fail"` would refuse
    /// when something else is already starting the unit, which is a worse answer for a person pressing
    /// a button.
    pub fn act(scope: Scope, unit: &str, action: Action) -> Result<()> {
        let proxy = manager(scope)?;
        let doing = format!("{} {unit}", action.name());

        match action {
            Action::Start => proxy.start_unit(unit, "replace").map(|_| ()),
            Action::Stop => proxy.stop_unit(unit, "replace").map(|_| ()),
            Action::Restart => proxy.restart_unit(unit, "replace").map(|_| ()),
            Action::Reload => proxy.reload_unit(unit, "replace").map(|_| ()),
            Action::Enable => proxy.enable_unit_files(&[unit], false, false).map(|_| ()),
            Action::Disable => proxy.disable_unit_files(&[unit], false).map(|_| ()),
            Action::Mask => proxy.mask_unit_files(&[unit], false, false).map(|_| ()),
            Action::Unmask => proxy.unmask_unit_files(&[unit], false).map(|_| ()),
        }
        .map_err(|e| from_dbus(e, &doing))?;

        // Enabling and masking change files on disk; systemd does not re-read them on its own, so
        // without this the change is real but invisible until something else triggers a reload.
        if action.is_persistent() {
            proxy.reload().map_err(|e| from_dbus(e, "reload systemd"))?;
        }
        Ok(())
    }

    /// Watch for unit changes and call back on each one. `SVC-3`.
    ///
    /// systemd is **silent until something calls `Subscribe`**, which is why Stacer never saw a change
    /// and loaded its list once per run. Once subscribed, `JobRemoved` fires whenever a unit is
    /// started, stopped, enabled or disabled — including from a terminal, which is the acceptance
    /// criterion.
    ///
    /// Runs on its own thread, blocked on the bus socket. A blocked thread costs nothing — the same
    /// argument as the metrics pipeline's condition variable — and it does not poll, so there is no
    /// interval to tune and no change that arrives late.
    ///
    /// The thread lives for the process. An earlier version tried to give it a deadline and stop, and
    /// was wrong: the deadline was checked *before* a blocking read, so it could overrun by however
    /// long the bus stayed quiet — which on an idle machine is indefinitely.
    pub fn watch(scope: Scope, on_change: impl Fn(&str) + Send + 'static) -> Result<()> {
        let connection = connect(scope)?;
        let proxy = ManagerProxy::new(&connection)
            .map_err(|e| from_dbus(e, "reach the service manager"))?;
        proxy
            .subscribe()
            .map_err(|e| from_dbus(e, "subscribe to unit changes"))?;

        let jobs = proxy
            .receive_job_removed()
            .map_err(|e| from_dbus(e, "receive unit change signals"))?;

        std::thread::Builder::new()
            .name("nix-units-watch".into())
            .spawn(move || {
                // Held so the connection outlives the iterator borrowing it.
                let _keep_alive = (connection, proxy);
                for signal in jobs {
                    if let Ok(args) = signal.args() {
                        on_change(args.unit());
                    }
                }
            })
            .map_err(|e| AppError::from_io(&e, "start the unit watcher"))?;

        Ok(())
    }
}

#[cfg(feature = "dbus")]
pub use dbus::{act, list, list_files, timers, watch};

/// Without the `dbus` feature nothing here can talk to systemd, and says so rather than returning an
/// empty list that would read as "this machine has no services".
#[cfg(not(feature = "dbus"))]
pub fn list(_scope: Scope) -> Result<Vec<Unit>> {
    Err(unavailable())
}

#[cfg(not(feature = "dbus"))]
fn unavailable() -> AppError {
    AppError::new(
        ErrorCode::Unsupported,
        "This build has no D-Bus support, so service management is unavailable.",
    )
    .with_remedy("An empty list would read as a machine with no services, which is not the case.")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn unit(name: &str, load: &str, active: &str, sub: &str) -> Unit {
        Unit {
            name: name.to_string(),
            description: String::new(),
            load_state: load.to_string(),
            active_state: active.to_string(),
            sub_state: sub.to_string(),
            following: String::new(),
            scope: Scope::System,
        }
    }

    #[test]
    fn a_units_kind_is_its_suffix() {
        assert_eq!(
            unit("nginx.service", "loaded", "active", "running").kind(),
            "service"
        );
        assert_eq!(
            unit("logrotate.timer", "loaded", "inactive", "dead").kind(),
            "timer"
        );
        assert_eq!(
            unit("home.mount", "loaded", "active", "mounted").kind(),
            "mount"
        );
        assert_eq!(
            unit("noextension", "loaded", "active", "running").kind(),
            ""
        );
    }

    /// Stacer discarded these with a regex, losing every instantiated unit on the system.
    #[test]
    fn template_instances_are_recognised_and_kept() {
        assert!(unit("getty@tty1.service", "loaded", "active", "running").is_instance());
        assert!(unit("user@1000.service", "loaded", "active", "running").is_instance());
        assert!(
            !unit("getty@.service", "loaded", "inactive", "dead").is_instance(),
            "the template itself is not an instance — it has nothing after the @"
        );
        assert!(!unit("nginx.service", "loaded", "active", "running").is_instance());
    }

    /// The state a user most needs to see, and the one the old filter hid.
    #[test]
    fn masked_units_are_identified() {
        assert!(unit("bad.service", "masked", "inactive", "dead").is_masked());
        assert!(!unit("ok.service", "loaded", "active", "running").is_masked());
    }

    #[test]
    fn a_failure_is_detected_in_either_state_field() {
        assert!(unit("a.service", "loaded", "failed", "failed").has_failed());
        assert!(
            unit("b.service", "loaded", "active", "failed").has_failed(),
            "a unit can be active with a failed sub-state, and that is still a failure"
        );
        assert!(!unit("c.service", "loaded", "active", "running").has_failed());
    }

    /// Offering an enable button for a static unit produces an error systemd was always going to give.
    #[test]
    fn only_installable_unit_files_can_be_enabled() {
        let file = |state: &str| UnitFile {
            name: "x.service".to_string(),
            state: state.to_string(),
            scope: Scope::System,
        };
        assert!(file("enabled").is_installable());
        assert!(file("disabled").is_installable());
        assert!(
            !file("static").is_installable(),
            "a static unit has no [Install] section — it is pulled in by dependency"
        );
        assert!(!file("masked").is_installable());
        assert!(
            !file("generated").is_installable(),
            "it does not exist on disk"
        );
        assert!(!file("transient").is_installable());
    }

    /// systemd reports "never" as both `0` and `u64::MAX`, depending on the property.
    #[test]
    fn both_of_systemds_never_sentinels_become_none() {
        assert_eq!(timestamp(0), None, "rendering this gives 1970");
        assert_eq!(
            timestamp(u64::MAX),
            None,
            "rendering this gives a year past 500,000"
        );
        assert_eq!(
            timestamp(1_787_000_000_000_000),
            Some(1_787_000_000_000_000)
        );
    }

    /// Stopping a service and disabling it are different intentions.
    #[test]
    fn persistent_actions_are_distinguished_from_transient_ones() {
        for persistent in [
            Action::Enable,
            Action::Disable,
            Action::Mask,
            Action::Unmask,
        ] {
            assert!(
                persistent.is_persistent(),
                "{persistent:?} survives a reboot"
            );
        }
        for transient in [Action::Start, Action::Stop, Action::Restart, Action::Reload] {
            assert!(
                !transient.is_persistent(),
                "{transient:?} changes what is running now, not what happens at boot"
            );
        }
    }

    #[test]
    fn action_names_are_stable_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for action in [
            Action::Start,
            Action::Stop,
            Action::Restart,
            Action::Reload,
            Action::Enable,
            Action::Disable,
            Action::Mask,
            Action::Unmask,
        ] {
            assert!(seen.insert(action.name()), "{action:?} shares a name");
        }
        assert_eq!(seen.len(), 8);
    }

    /// A calendar timer: compared against the wall clock.
    #[test]
    fn a_calendar_timers_countdown_uses_the_wall_clock() {
        // 15 minutes from a notional now.
        let now = 1_787_000_000;
        let at_us = (now as u64 + 900) * 1_000_000;
        assert_eq!(seconds_until(Some(at_us), None, now, Some(50.0)), Some(900));
    }

    /// A monotonic timer: compared against uptime, because both are the same clock.
    ///
    /// Adding boot time to the offset instead would drift as soon as the machine suspended, since
    /// `CLOCK_MONOTONIC` does not advance while suspended but the wall clock does.
    #[test]
    fn a_monotonic_timers_countdown_uses_uptime() {
        // Fires at 4 h 7 s after boot; the machine has been up 3 h.
        let next_us = (4 * 3600 + 7) * 1_000_000;
        let until = seconds_until(None, Some(next_us), 1_787_000_000, Some(3.0 * 3600.0)).unwrap();
        assert!(
            (until - 3607).abs() <= 1,
            "an hour and seven seconds away, got {until}"
        );
    }

    #[test]
    fn a_realtime_schedule_wins_when_both_are_present() {
        let now = 1_787_000_000;
        let realtime = (now as u64 + 60) * 1_000_000;
        assert_eq!(
            seconds_until(Some(realtime), Some(9_999_000_000), now, Some(10.0)),
            Some(60),
            "the wall-clock instant is the one a person can act on"
        );
    }

    #[test]
    fn an_overdue_timer_reports_a_negative_countdown() {
        let now = 1_787_000_000;
        let past = (now as u64 - 30) * 1_000_000;
        assert_eq!(
            seconds_until(Some(past), None, now, Some(10.0)),
            Some(-30),
            "overdue is legitimate while its service is still running, and is not the same as never"
        );
    }

    #[test]
    fn a_timer_with_no_schedule_has_no_countdown() {
        assert_eq!(seconds_until(None, None, 1_787_000_000, Some(10.0)), None);
        assert_eq!(
            seconds_until(None, Some(5_000_000), 1_787_000_000, None),
            None,
            "without uptime a monotonic offset cannot be turned into a countdown"
        );
    }

    #[test]
    fn this_machines_uptime_reads() {
        if let Some(uptime) = uptime_seconds() {
            assert!(uptime > 0.0, "a running machine has been up for some time");
        }
    }

    // ---- against this machine ----

    #[cfg(feature = "dbus")]
    #[test]
    fn this_machines_units_are_listed_in_one_call() {
        let Ok(units) = list(Scope::System) else {
            return; // no systemd here
        };
        assert!(
            units.len() > 50,
            "a running machine has units: {}",
            units.len()
        );

        // The states Stacer's `--state=enabled,disabled` filter dropped must be present.
        assert!(
            units.iter().any(|u| u.kind() == "service"),
            "no services in the inventory"
        );
        assert!(
            units.iter().any(|u| u.kind() != "service"),
            "an inventory of only services has filtered something out"
        );

        for unit in &units {
            assert!(!unit.name.is_empty());
            assert!(!unit.load_state.is_empty());
            assert!(!unit.active_state.is_empty());
        }
    }

    /// `SVC-1`'s acceptance criterion: 400 units in under 500 ms.
    #[cfg(feature = "dbus")]
    #[test]
    fn the_inventory_meets_its_budget() {
        let start = std::time::Instant::now();
        let Ok(units) = list(Scope::System) else {
            return;
        };
        let elapsed = start.elapsed();
        if units.len() < 50 {
            return;
        }

        // Scaled to the criterion's 400, so a machine with more units is judged on the same rate.
        let per_unit = elapsed.as_secs_f64() / units.len() as f64;
        let for_four_hundred = per_unit * 400.0;
        assert!(
            for_four_hundred < 0.5,
            "{} units took {elapsed:?}, which is {for_four_hundred:.3}s for 400 — the budget is 0.5s",
            units.len()
        );
    }

    #[cfg(feature = "dbus")]
    #[test]
    fn unit_files_include_ones_that_have_never_run() {
        let Ok(files) = list_files(Scope::System) else {
            return;
        };
        let Ok(loaded) = list(Scope::System) else {
            return;
        };
        if files.is_empty() {
            return;
        }

        let loaded_names: std::collections::HashSet<&str> =
            loaded.iter().map(|u| u.name.as_str()).collect();
        assert!(
            files
                .iter()
                .any(|f| !loaded_names.contains(f.name.as_str())),
            "ListUnits omits units that have never run, which is why ListUnitFiles is also read"
        );
        // And the states the old filter dropped are really there.
        assert!(
            files.iter().any(|f| f.state == "static"),
            "a modern system is mostly static units"
        );
    }

    #[cfg(feature = "dbus")]
    #[test]
    fn this_machines_timers_have_sane_schedules() {
        let Ok(timers) = timers(Scope::System) else {
            return;
        };
        for timer in &timers {
            assert!(timer.name.ends_with(".timer"));
            // Whatever the value, it is never one of systemd's sentinels.
            assert_ne!(timer.next_elapse_us, Some(0));
            assert_ne!(timer.next_elapse_us, Some(u64::MAX));
        }

        // # Regression
        //
        // The property names were derived by zbus rather than stated, giving `NextElapseUsecRealtime`
        // where systemd has `NextElapseUSecRealtime`. The read failed, `.ok()` made it `None`, and
        // every timer on this machine reported that it would never fire again — a plausible-looking
        // answer and completely wrong. A machine with active timers must have at least one with a
        // real schedule.
        if timers.iter().any(|t| t.last_trigger_us.is_some()) {
            assert!(
                timers.iter().any(|t| t.next_elapse_us.is_some()),
                "timers have fired before but none is scheduled to fire again — the property names \
                 are almost certainly not being read: {:?}",
                timers.iter().map(|t| &t.name).collect::<Vec<_>>()
            );
        }
    }
}
