# 20 — Known Quirks, Bugs and Dead Code

Behaviour the rebuild should **not** reproduce, and traps to be aware of when using the
original as a reference. Each item was verified against the source in this tree.

---

## Correctness bugs

### `/proc/meminfo` fields read in the wrong order
`MemoryInfo::updateMemoryInfo()` filters eight fields and then indexes them positionally,
assuming `SReclaimable` (index 6) precedes `Shmem` (index 7). On current kernels the order in
`/proc/meminfo` is `… Shmem … SReclaimable …`, so the two are **swapped** and the derived
`cached = Cached + SReclaimable − Shmem` is actually computed as `Cached + Shmem − SReclaimable`.
Used memory is therefore off by `2 × (Shmem − SReclaimable)`.
*Fix in the rebuild:* parse `/proc/meminfo` into a key→value map, never by position.
`stacer-core/Info/memory_info.cpp:22-32`

### DNF/YUM package cache points at the pacman directory
```cpp
case PackageTool::PackageTools::YUM:
case PackageTool::PackageTools::DNF:
    return PackageTool::getPacmanPackageCaches();   // ← /var/cache/pacman/pkg/
```
So on Fedora the System Cleaner's "Package cache" category scans a directory that does not
exist and always reports 0 bytes. There is no `getDnfPackageCaches()` at all.
`stacer/Managers/tool_manager.cpp:82`

### `HistoryChart::setSeriesList()` is a no-op
```cpp
for (int i = 0; i < seriesList.count(); ++i)
    mChart->series().replace(0, seriesList.at(i));   // series() returns a QList *by value*
mChartView->repaint();
```
It mutates a temporary copy, and always at index 0. The charts still update only because the
callers mutate the same `QSplineSeries` objects the chart already owns; the call is an
expensive repaint trigger.
`stacer/Pages/Resources/history_chart.cpp:108`

### `find -invert` is not a real predicate
`SearchPage` appends the literal `-invert` when the "invert" checkbox is ticked. `find(1)` has
no such option — it errors out, `CommandUtil` discards stderr, and the user sees an empty
result set. The intended semantics are `!` / `-not` applied to the name test.
`stacer/Pages/Search/search_page.cpp:211`

### Autostart entries with neither key are treated as disabled
```cpp
if (!hidden.isEmpty()) enabled = (hidden != "true");
else                   enabled = (gnomeEnabled == "true");
```
Per the XDG autostart spec, an entry with neither `Hidden` nor `X-GNOME-Autostart-enabled` **is**
enabled. Stacer shows most distro-shipped entries as off.
`stacer/Pages/StartupApps/startup_apps_page.cpp:98`

### `StartPage` is stored as a translated string
`SettingManager::getStartPage()` defaults to `QObject::tr("Dashboard")` and the value is
matched against `windowTitle()`. Change the language and the stored preference no longer
matches any page, so `clickSidebarButton` silently falls back to the first page.
`stacer/Managers/setting_manager.cpp:61`

### APT source lines are located by substring
`AptSourceTool::changeSource()` finds the line to rewrite with
`sourceFileContent[i].indexOf(aptSource->source) != -1`. Two entries in one file that share a
prefix (same repo, different components; or an active line plus its commented twin) can make it
rewrite the wrong line. `APTSource` carries `filePath` but no line index.
`stacer-core/Tools/apt_source_tool.cpp:39`

### Snap removal runs even with nothing selected
```cpp
ToolManager::ins()->uninstallPackages(selectedPackages);
ToolManager::ins()->uninstallSnapPackages(selectedSnapPackages);   // unconditional
```
An empty snap selection still spawns `pkexec snap remove` with no arguments — a second,
pointless polkit prompt on every uninstall.
`stacer/Pages/Uninstaller/uninstaller_page.cpp:148`

### Host deletion blanks the line instead of removing it
`mHostFileContent.replace(lineNumber, "")` leaves an empty line behind; repeated edits
accumulate blank lines in `/etc/hosts`.
`stacer/Pages/Helpers/host_manage.cpp:218`

### Package names round-trip through the display string
Items are created as `QString("  %1").arg(package)` (two leading spaces for icon padding) and
read back with `item->text().trimmed()`. Any change to the display format silently corrupts the
argument passed to `apt-get remove`.
`stacer/Pages/Uninstaller/uninstaller_page.cpp:52, :116`

### `CircleBar` double-deletes its chart
```cpp
mChartView(new QChartView(mChart))   // QChartView takes ownership of the chart
…
CircleBar::~CircleBar() { delete ui; delete mChart; }
```
`stacer/Pages/Dashboard/circlebar.cpp:7`

### Shadowed member in the disk pie chart
`ResourcesPage::initDiskPieChart()` declares a **local** `QChartView *mChartViewDiskPie`,
shadowing the member of the same name — which is therefore never initialised (and is read
nowhere, so it is latent rather than fatal).
`stacer/Pages/Resources/resources_page.cpp:89`

### `Feedback` calls a slot as if it were a signal
`emit clearInputs();` — `clearInputs` is a **slot**, not a signal, and `emit` is a no-op macro,
so this is a direct call that clears three widgets from the worker thread. The correctly-named
`clearInputsS` signal exists two lines away.
`stacer/feedback.cpp:78`

---

## Crash and out-of-bounds risks

| Risk | Location |
| --- | --- |
| `lines.filter(QRegExp("^CPU MHz")).first()` on an empty list — `lscpu` on VMs/ARM often has no `CPU MHz` line | `cpu_info.cpp:71` (`getAvgClock`) |
| `im->getCpuPercents().at(0)` with no zero-length guard — empty if `/proc/stat` is unreadable | `dashboard_page.cpp:129` |
| `colors.at(i)` over a 20-entry palette with one series per logical core — asserts on >20-core machines | `history_chart.cpp:50` |
| `static QVector<double> l_idles(N)` sized on the **first** call; if `getCpuCoreCount()` returned 0 then (unreadable `/proc/cpuinfo`), later indexing is out of bounds | `cpu_info.cpp:137` |
| `static quint8 count` for the logical core count — wraps to 0 at 256 CPUs | `cpu_info.cpp:40` |
| Memory chart divides by `getMemTotal()` with no zero guard (the swap series *is* guarded) | `resources_page.cpp:358` |
| `FileUtil::getFileSize()` recurses into symlinked directories with no cycle detection | `file_util.cpp:76` |

---

## Silent-failure patterns

1. **Exit codes are never checked.** `CommandUtil::exec` reads stdout only and throws solely
   on `QProcess` errors. A command that fails cleanly looks like "succeeded, empty output".
2. **stderr is discarded everywhere.**
3. **A cancelled `pkexec` prompt is indistinguishable from success** — `sudoExec` catches the
   throw, logs it and returns `""`. Only the Services page compensates, by re-reading state
   after every write.
4. **`readListFromFile` on a missing file returns a list containing one empty string**, so
   `isEmpty()` checks pass and downstream `at(0)`/`split()` calls operate on `""`.
5. **The file logger is never installed.** `messageHandler()` in `main.cpp` is fully
   implemented but `qInstallMessageHandler()` is never called, so every `qCritical()` in the
   codebase goes to stderr and `~/.config/stacer/stacer.log` is never created. Effectively the
   app has **no error reporting at all**.

---

## Fragile parsing

| Parse | Fragility |
| --- | --- |
| `lscpu` under `LANG=nl_NL.UTF-8` | Pins to **Dutch**, which uses `,` as the decimal separator — `toDouble()` truncates at the comma. Intended to stabilise field names; `LC_ALL=C` was meant. |
| `ps` output split on `\s+` with a fixed field count | A username containing whitespace breaks the row (mitigated by `uname:50`, not solved). |
| `systemctl list-unit-files` output columns | Format is not stable across systemd versions; `--state=enabled,disabled` also omits `static`, `masked`, `generated` units entirely. |
| `rpm -qa` | Returns full NVRA strings, so the Fedora package list shows versions while APT shows names. |
| `dpkg --get-selections` filtered by `\s+install$` | Misses `hold` packages; deselected-but-configured packages are excluded (usually desirable). |
| `gsettings get` return values | Strings arrive quoted; every call site strips quotes with `.replace("'", "")`. |
| APT one-line source format only | deb822 `.sources` files (current Debian/Ubuntu) are invisible. |
| `Utilities::getDesktopValue` splits on `=` and takes `last()` | Values containing `=` are truncated. |
| Update check compares versions as strings | Any difference — including a *lower* upstream tag — shows the "update available" banner. |
| Email regex `[A-Z]{2,4}` TLD | Rejects `.museum`, `.online`, etc. |

---

## Threading violations

`SystemCleanerPage::systemScan/systemClean`, `SearchPage::searching`,
`UninstallerPage::loadPackages/loadSnapPackages` and two `setText` calls in `Feedback` all
mutate widgets from `QtConcurrent` worker threads. The System Cleaner constructor even registers
metatypes "to suppress qt warnings (signal/slot <> threads)". `ServicesPage` is the only page
that does the hand-off correctly. See
[19-data-flow-and-concurrency.md](19-data-flow-and-concurrency.md).

---

## Layering violations

Pages that bypass the manager layer and call `stacer-core` (or Qt) directly:

| Page | Direct call |
| --- | --- |
| `ProcessesPage` | `CommandUtil::exec/sudoExec("kill", …)` |
| `SearchPage` | `CommandUtil::exec/sudoExec("find"/"rm"/"mv", …)`, `FileUtil::writeFile` |
| `SystemCleanerPage` | `CommandUtil::sudoExec("rm", …)`, `FileUtil::getFileSize` |
| `HostManage` | `FileUtil::readListFromFile/writeFile`, `CommandUtil::sudoExec("mv", …)` |
| `StartupApps*` | `FileUtil` directly |
| `SettingsPage` | `FileUtil`, `QFile::remove`, `InfoManager` and `QStorageInfo` mixed |
| `DashboardPage` | constructs its own `SystemInfo` (a second `lscpu` run) |
| `GnomeSettings*` | `GnomeSettingsTool::ins()` directly — no manager exists for it |
| `Feedback` | `CommandUtil::exec("curl", …)` |

`ToolManager` has no GNOME/gsettings surface at all, which is why that page had to go direct.

---

## Performance

- **Two subprocesses per second at idle:** `bash -c lscpu` (Dashboard CPU gauge) and
  `ps ax -weo …` (Processes page). Both pages poll forever because their timers are started in
  the constructor and never stopped.
- **Service scan costs `1 + 2N` `systemctl` spawns** (`cat` + `is-active` per unit) — several
  hundred processes and multiple seconds on a normal desktop.
- **GNOME Settings issues ~30 blocking `gsettings` subprocesses** when the page is constructed
  (which happens at app startup, not on first view).
- **The process table model is destroyed and rebuilt every tick**, forcing manual selection and
  sort restoration.
- **All 10–12 pages are constructed eagerly at startup**, which is why a splash screen exists.
- **`FileUtil::getFileSize` walks whole trees synchronously** (`~/.cache` scans).
- **Chart Y-axis maxima only ever grow** — one traffic burst permanently flattens the network
  and disk charts for the rest of the session.

---

## Dead code and unused API

| Symbol | Note |
| --- | --- |
| `messageHandler()` (`main.cpp`) | Never installed — no logging happens |
| `CpuInfo::getClocks()` | Never called |
| `NetworkInfo::getAllInterfaces()` | Never called |
| `FileUtil::directoryList()` | Never called |
| `GnomeSettingsTool::checkGSettings()` | Never called (the page is gated on the distro name instead) |
| `PROC_MOUNTS` macro (`disk_info.h`) | Defined, never used |
| `AppManager::loadThemeList()` / `getThemeList()` | Commented out |
| `SettingsPage::cmbThemesChanged()` and the themes combo | Commented out |
| `SettingManager::getThemeName()` | Hard-coded to `"default"`; the `QSettings` read is commented out — the whole `light` theme is unreachable |
| `themes.json`, `static/themes/light/**` | Shipped but unusable |
| `SettingManager::setThemeName()` | Writes a value nothing reads |
| `HelpersPage` second tool slot | `//ui->stackedWidget->addWidget();` — the page is a one-tool shell |
| `translations/stacer_gl.ts`, `stacer_ro.ts` | Present but absent from `languages.json`, so unselectable |
| `Process` setters for fields never displayed | All 13 fields are displayed, but 7 columns are hidden by default |

---

## Dead external dependencies

- **Feedback endpoint** `https://stacer-web-api.herokuapp.com/feedback` — free Heroku dynos were
  retired; the feature cannot work.
- **Update check** targets the abandoned upstream repository, which will never publish a new
  release.
- **Unity 7 / Compiz / `org.gnome.nautilus.desktop` schemas** — removed from current systems, so
  most of the GNOME Settings page silently does nothing.

---

## Style / build warnings (harmless, but noisy)

- `QString::sprintf` (deprecated in Qt 5.14) in `FormatUtil::formatBytes` and two chart labels.
- `QTextStream << endl` (deprecated in Qt 5.14) in `FileUtil::writeFile` and `messageHandler`.
- `QListWidget::setItemHidden` (deprecated) in the APT source search.
- `-Wreorder` in `HelpersPage` and `HostManage` constructors (initialiser list order ≠
  declaration order).
- `-Wsign-compare` in `main.cpp` (`size_t i < argc`).
- `QRegExp` throughout (removed in Qt 6 — a Qt 6 port would have needed `QRegularExpression`).
- Filename inconsistency: `Pages/Uninstaller/uninstallerpage.ui` (no underscore) while every
  other form uses `snake_case`.
- `stacer-core.pro` lists sources explicitly while `CMakeLists.txt` globs them, so the two build
  systems can drift.
