# 10 — Feature: Services

**Files:** `stacer/Pages/Services/{services_page,service_item}.{h,cpp,ui}`
**Window title:** `Services` · **Sidebar button:** `btnServices`
**Backend:** `stacer-core/Tools/service_tool.*`

## UI

- `listWidgetServices` — one `ServiceItem` widget per unit, hosted in `QListWidgetItem`s.
- `cmbRunningStatus` — `Running Status` (all) / `Running` / `Not Running`.
- `cmbStartupStatus` — `Startup Status` (all) / `Enabled` / `Disabled`.
- `lblServicesTitle` — `tr("System Services (%1)")`.
- `notFoundWidget` — shown when the filtered list is empty.

Each `ServiceItem` shows the unit name, `"- " + description`, and two checkboxes:
`checkServiceRunning` (active state) and `checkServiceStartup` (enabled state). Both the name
and description labels get tooltips carrying their full text.

## Loading

```cpp
connect(this, &ServicesPage::loadServicesS, this, &ServicesPage::loadServices);
QtConcurrent::run(this, &ServicesPage::getServices);
```

`getServices()` runs on a thread-pool thread, calls `ToolManager::getServices()` (which is the
expensive `systemctl` walk described below), stores the result in `mServices`, and emits
`loadServicesS`. Because the signal is connected to a slot on the same object with the default
`AutoConnection`, the emit is queued to the GUI thread — so `loadServices()` (which touches
widgets) runs safely on the main thread. This is the app's only correctly-threaded UI update.

`loadServices()` clears the list, then for each cached `Service` applies the two combo filters
and creates a `ServiceItem`. Filter semantics: index 0 means "no filter"; index 1 means
`true` (Running / Enabled); index 2 means `false`.

The list is loaded **once**, at page construction. Changing a filter re-renders from the cache;
there is no refresh button and no re-scan, so units that appear or change state elsewhere are
not reflected until restart.

## Cost of the scan

`ServiceTool::getServicesWithSystemctl()`:

```
systemctl list-unit-files -t service -a --state=enabled,disabled
```

then, for **each** returned unit:

```
systemctl cat <unit>        # → first ^Description line, split on '='
systemctl is-active <unit>  # → "active"?
```

So the cost is `1 + 2N` process spawns. On a typical desktop (N ≈ 200–400) this is several
hundred `systemctl` invocations and multiple seconds of wall time. The `.filter(QRegExp("[^@].service"))`
step drops template/instance units (`foo@.service`) before that loop.

`status` (enabled) comes free from the `list-unit-files` output's last column; only `active`
and `description` need the extra calls.

## Toggling

```cpp
// ServiceItem::on_checkServiceStartup_clicked(bool status)
tm->changeServiceStatus(name, status);                        // pkexec systemctl enable|disable
ui->checkServiceStartup->setChecked(tm->serviceIsEnabled(name));   // verify

// ServiceItem::on_checkServiceRunning_clicked(bool status)
tm->changeServiceActive(name, status);                        // pkexec systemctl start|stop
ui->checkServiceRunning->setChecked(tm->serviceIsActive(name));    // verify
```

The verification re-read is what makes a cancelled polkit prompt visibly revert the checkbox.
Note the unit name passed to `systemctl` is the **`.service`-stripped** name (the label text),
which systemd accepts.

Each toggle blocks the GUI thread for the duration of the `pkexec` prompt plus the verification
call — the window is unresponsive while the polkit dialog is open.

## Port notes

- Use the systemd D-Bus API (`org.freedesktop.systemd1.Manager.ListUnitFiles`,
  `ListUnits`, `GetUnitFileState`, plus the `Description` property) — one round trip instead of
  `2N` process spawns, and it gives descriptions and active state in the same call.
- `EnableUnitFiles` / `DisableUnitFiles` / `StartUnit` / `StopUnit` over D-Bus integrate with
  polkit natively, giving one prompt and a real error result.
- Subscribe to unit change signals for live state instead of a one-shot scan.
- Keep the two-axis filter (running × enabled) and the search-free list; add a refresh action.
