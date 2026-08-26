# 01 — Architecture

## 1. Two build targets

| Target | Type | Qt modules | Purpose |
| --- | --- | --- | --- |
| `stacer-core` | static library | `Core`, `Network` | All system interaction. No GUI symbols. |
| `stacer` | executable | `Core`, `Gui`, `Widgets`, `Charts`, `Svg`, `Concurrent` | The application; links `stacer-core`. |

`stacer-core/CMakeLists.txt` globs `**.cpp` recursively and builds a static lib; the qmake
file (`stacer-core.pro`) declares `TEMPLATE = lib` with `QT -= gui`. The separation is
deliberate: **nothing in `stacer-core` knows about widgets**, so it is the layer that maps
directly onto the Rust backend in the rebuild.

The library is *not* installed separately — it is statically linked into the single `stacer`
binary that ships.

## 2. Layering

```
                    ┌─────────────────────────────────────────┐
                    │  Pages/*  (QWidget per sidebar page)    │  presentation
                    │  + app.cpp shell, dialogs, custom bars  │
                    └───────────────┬─────────────────────────┘
                                    │  calls, only via managers
                    ┌───────────────▼─────────────────────────┐
                    │  Managers/  (4 singletons)              │  façade / state
                    │  InfoManager  ToolManager               │
                    │  AppManager   SettingManager            │
                    └───────────────┬─────────────────────────┘
                                    │
        ┌───────────────────────────▼───────────────────────────┐
        │  stacer-core                                          │
        │  Info/   read-only samplers   Tools/  mutators        │  system access
        │  Utils/  CommandUtil · FileUtil · FormatUtil          │
        └───────────────────────────┬───────────────────────────┘
                                    │
              /proc  /sys  /etc  ~/.config  ~/.cache  +  QProcess
```

The layering is respected inconsistently: several pages bypass the managers and call
`CommandUtil`, `FileUtil`, `SystemInfo` or `GnomeSettingsTool` directly (see
[20-known-quirks-and-bugs.md](20-known-quirks-and-bugs.md#layering-violations)). For the
rebuild, treat the *manager* surface as the intended API boundary.

### 2.1 `Info/` — read-only samplers

| Class | Reads |
| --- | --- |
| `CpuInfo` | `/proc/cpuinfo`, `/proc/stat`, `/proc/loadavg`, `lscpu` |
| `MemoryInfo` | `/proc/meminfo` |
| `DiskInfo` | `QStorageInfo` (mounted volumes), `/sys/block/*/stat` |
| `NetworkInfo` | `QNetworkInterface`, `/sys/class/net/<if>/statistics/{rx,tx}_bytes` |
| `SystemInfo` | `QSysInfo`, `lscpu`, `/etc/passwd`, `/etc/group`, `/var/crash`, `/var/log`, `~/.cache` |
| `ProcessInfo` + `Process` | `ps ax -weo …` |

`ProcessInfo` is the only `Info` class deriving from `QObject` (it exposes
`updateProcesses()` as a slot); the rest are plain classes.

### 2.2 `Tools/` — mutating operations

| Class | Backends |
| --- | --- |
| `ServiceTool` | `systemctl` (list-unit-files, cat, is-active, is-enabled, enable/disable, start/stop) |
| `PackageTool` | `dpkg`/`apt-get`, `rpm`/`dnf`/`yum`, `pacman`, `snap`; detects `zypper` but never uses it |
| `AptSourceTool` | `/etc/apt/sources.list`, `/etc/apt/sources.list.d/*.list`, `add-apt-repository`, `tee` |
| `GnomeSettingsTool` | `gsettings get/set`, plus `gnome_schema.h` schema/key/path constants |

`PackageTool::currentPackageTool` is a **static const** initialised once at program start by
probing `PATH` for `apt-get`, then `dnf`, `yum`, `pacman`, `zypper` in that order. That single
value drives all package operations; a system with both `apt-get` and `snap` still reports
`APT`.

`GnomeSettingsTool` is the only tool that is a real singleton (`ins()` returning a function-local
static); the others are all-static utility classes.

### 2.3 `Utils/`

- **`CommandUtil`** — the process gateway. Three functions:
  `exec(cmd, args, stdinData)`, `sudoExec(...)` (prepends `pkexec`), `isExecutable(cmd)`
  (`QStandardPaths::findExecutable`). Synchronous: it starts a `QProcess`, optionally writes
  stdin, `waitForFinished(600 s)`, reads **stdout only**, then `kill()` + `close()`.
  On `QProcess::error() != UnknownError` it **throws a `QString`** (raw error string).
  Callers catch `QString&` — an unusual convention that recurs everywhere.
- **`FileUtil`** — `readStringFromFile`, `readListFromFile` (split on `\n`), `writeFile`,
  `directoryList`, `getFileSize` (recursive for directories).
- **`FormatUtil`** — `formatBytes()` producing binary units (`bytes`, `KiB`, `MiB`, `GiB`,
  `TiB`) with one decimal; plus `KIBI/MEBI/GIBI/TEBI` constants.

### 2.4 `Managers/` — four singletons

All four use the same hand-rolled `static T* instance; static T* ins()` lazy pattern, never
freed, not thread-safe.

| Manager | Responsibility |
| --- | --- |
| `InfoManager` | Owns one instance each of the six `Info` classes; forwards ~25 getters. Pure delegation. |
| `ToolManager` | Forwards to `ServiceTool` / `PackageTool` / `AptSourceTool`; **contains the package-manager `switch`** that selects per-distro behaviour. |
| `SettingManager` | `QSettings` INI wrapper. Keys in namespace `SettingKeys`. |
| `AppManager` | Owns the `QTranslator`, the `QSystemTrayIcon`, the parsed language list, and stylesheet assembly. |

`InfoManager` holds the `Info` objects as **value members**, so `NetworkInfo`'s constructor
(which picks the default interface) runs exactly once, at first `InfoManager::ins()` call —
the chosen interface is never re-evaluated for the process lifetime.

### 2.5 `SignalMapper` — global event bus

`stacer/signal_mapper.h` is a `QObject` singleton with three signals and no slots:

- `sigChangedAppTheme()` — emitted by `AppManager::updateStylesheet()`; every chart widget
  connects to it to re-read colour tokens, and `SystemCleanerPage` uses it to (re)build its
  loading animations. Because it is emitted once during `App::init()`, it doubles as a
  "widgets, do your deferred initialisation now" signal.
- `sigUninstallStarted()` / `sigUninstallFinished()` — cross-thread notification from the
  background uninstall task to `UninstallerPage`.

This is the app's only publish/subscribe channel; everything else is direct
`connect(sender, signal, receiver, slot)`.

## 3. Application startup sequence

`stacer/main.cpp`:

1. Construct `QApplication`; set application name `stacer`, display name `Stacer`,
   version `1.1.0`, window icon `:/static/logo.png`.
2. Install a custom `qInstallMessageHandler`? — **no**: `messageHandler` is *defined* but
   never installed (see quirks doc). As written it would append `[ts] [LEVEL] msg` lines to
   `<AppConfigLocation>/stacer.log`, skipping warnings, truncating the file once it exceeds 1 MiB.
3. Parse CLI options with `QCommandLineParser` (`--hide`, `--nosplash`, plus `--version`,
   `--help`), then **re-scan `argv` manually** to set the two booleans.
4. Register the bundled font `:/static/font/Ubuntu-R.ttf`.
5. Show `QSplashScreen` with `:/static/splashscreen.png` unless `--nosplash`.
6. Construct `App` (the main window). Show it unless `--hide` was passed.
7. `splash->finish(&w)`, delete splash, `app.exec()`.

`App::init()` (`stacer/app.cpp`) then:

1. Centre the window on the available desktop geometry.
2. Instantiate the ten always-present pages into a `SlidingStackedWidget`.
3. Conditionally add two more pages:
   - **APT Source Manager** if `/etc/apt/sources.list.d` exists
     (`ToolManager::checkSourceRepository()`), else hide `btnAptSourceManager`.
   - **GNOME Settings** if `$DESKTOP_SESSION` matches `ubuntu` (case-insensitive) **or**
     `QSysInfo::prettyProductName()` matches `ubuntu`, else hide `btnGnomeSettings`.
   Both are inserted at fixed indices (7 and 8) into the page/button lists.
4. `AppManager::updateStylesheet()` → loads QSS, substitutes tokens, applies globally,
   emits `sigChangedAppTheme`.
5. Add a drop shadow to the sidebar; select the configured start page.
6. Build tray menu (one action per sidebar button + Quit) and show the tray icon.
7. Build the "quit or minimise to tray?" `QMessageBox`.

Page construction is **eager and synchronous**: all twelve pages are built before the window
appears, which is why several of them kick off `QtConcurrent::run` in their constructors and
why the splash screen exists at all. `ServicesPage` in particular would otherwise block for
seconds (it runs `systemctl cat` once per unit).

## 4. Page inventory

Sidebar buttons live in `app.ui`; page titles come from each page widget's `windowTitle`
(set in its `.ui` file) and are matched by string in `App::getPageByTitle()`.

| Order | Page class | Directory | Conditional |
| --- | --- | --- | --- |
| 0 | `DashboardPage` | `Pages/Dashboard` | always |
| 1 | `StartupAppsPage` | `Pages/StartupApps` | always |
| 2 | `SystemCleanerPage` | `Pages/SystemCleaner` | always |
| 3 | `SearchPage` | `Pages/Search` | always |
| 4 | `ServicesPage` | `Pages/Services` | always |
| 5 | `ProcessesPage` | `Pages/Processes` | always |
| 6 | `UninstallerPage` | `Pages/Uninstaller` | always |
| 7 | `APTSourceManagerPage` | `Pages/AptSourceManager` | `/etc/apt/sources.list.d` exists |
| 8 | `GnomeSettingsPage` | `Pages/GnomeSettings` | Ubuntu session/distro |
| 9 | `ResourcesPage` | `Pages/Resources` | always |
| 10 | `HelpersPage` | `Pages/Helpers` | always |
| 11 | `SettingsPage` | `Pages/Settings` | always |
| — | `Feedback` | `stacer/feedback.*` | modal dialog, lazily created |

Note the list order in `App::init()` (`mListPages`) and the sidebar button order
(`mListSidebarButtons`) are declared as two parallel literals that must stay in sync; the
`insert(7, …)` / `insert(8, …)` calls patch both.

## 5. Object ownership and lifetime

- Pages are children of the `SlidingStackedWidget`, which is a child of the main window —
  destroyed with it. Each page deletes its `ui` in its destructor.
- Managers, `SignalMapper` and `GnomeSettingsTool` are leaked singletons (intentional; process
  lifetime).
- `Disk*` objects are raw pointers owned by `DiskInfo`, `qDeleteAll`-ed on every
  `updateDiskInfo()` and in the destructor. **Any pointer a page holds across an update is
  dangling** — pages must re-fetch `getDisks()` after each update.
- `APTSource` is refcounted (`QSharedPointer<APTSource>` = `APTSourcePtr`).
- `Process` is a value type, copied into lists.
- Dialogs (`Feedback`, `StartupAppEdit`, `APTSourceEdit`) are held in `QSharedPointer` members
  and created on first use, then reused.
- Two dialogs pass their subject through a **static member** rather than a constructor
  argument: `StartupAppEdit::selectedFilePath` and `APTSourceEdit::selectedAptSource`.

## 6. Configuration state

`SettingManager` writes `<AppConfigLocation>/settings.ini` (i.e.
`~/.config/stacer/settings.ini`). Keys:

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `Language` | string | `en` | Locale code for `stacer_<code>.qm` |
| `ThemeName` | string | `default` | **Getter is hard-coded to `default`**; the setter still writes the file |
| `DiskName` | string | `""` | Which volume the dashboard disk bar tracks |
| `StartPage` | string | `tr("Dashboard")` | Page title shown at launch |
| `CPUAlertPercent` | int | `0` (off) | Tray-notification threshold |
| `MemoryAlertPercent` | int | `0` (off) | Tray-notification threshold |
| `DiskAlertPercent` | int | `0` (off) | Tray-notification threshold |
| `AppQuitDialogDontAsk` | bool | `false` | Suppress the close-behaviour prompt |
| `AppQuitDialogChoice` | string | `close` | `close` \| `hide` |

Because `StartPage` is stored **translated**, changing language orphans the stored value and
the app silently falls back to the first page.

Additional persistent state outside `settings.ini`:

- `~/.config/autostart/stacer.desktop` — "start on boot" toggle (Settings page).
- `<AppConfigLocation>/stacer.log` — the intended log target.
