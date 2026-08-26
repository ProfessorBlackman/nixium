# 19 — Data Flow and Concurrency

## 1. There is no central data store

Every page owns its own polling loop and its own copy of state. `InfoManager` is a *stateless
forwarder* except for the `Info` objects' internal caches. Two pages sampling the same metric do
so independently and interfere through the samplers' static delta state.

```
DashboardPage   ──1 s──┐
                        ├──► InfoManager ──► CpuInfo::getCpuPercents()   ◄── shared static deltas
ResourcesPage   ──1 s──┘

ProcessesPage   ──1..10 s──► InfoManager ──► ProcessInfo ──► `ps` subprocess
```

## 2. Timers

| Page | Timer | Interval | Started | Stopped |
| --- | --- | --- | --- | --- |
| Dashboard | `mTimer` | 1 s | constructor | never |
| Dashboard | `timerDisk` | 5 s | constructor | never |
| Resources | `mTimer` | 1 s | constructor | never |
| Processes | `mTimer` | 1 s (slider 1–10 s) | constructor | never |

Because pages are constructed eagerly at startup and timers are never stopped, **all four
timers run for the entire process lifetime**, including while the window is hidden in the tray.
Per second, at idle, the app performs roughly:

- 2 × `/proc/stat` + `/proc/loadavg` + `/proc/meminfo` reads
- 2 × `/sys/class/net/*/statistics/{rx,tx}_bytes` reads
- 1 × `/sys/block/*/stat` read per disk
- 1 × `bash -c lscpu` **subprocess** (Dashboard CPU gauge)
- 1 × `ps ax -weo …` **subprocess** (Processes page)
- full teardown/rebuild of the process table model

plus a `QStorageInfo::mountedVolumes()` enumeration every 5 s. The two subprocesses per second
are the dominant cost and the first thing to remove in a rewrite.

## 3. Thread usage

Everything runs on the GUI thread except these `QtConcurrent::run` calls:

| Page | Function | Touches widgets from the worker? |
| --- | --- | --- |
| `ServicesPage` | `getServices()` | **No** — signals back to `loadServices()` ✅ |
| `SystemCleanerPage` | `systemScan()` | **Yes** — builds the whole tree, toggles buttons ❌ |
| `SystemCleanerPage` | `systemClean()` | **Yes** ❌ |
| `SearchPage` | `searching()` | **Yes** — spinner, error label, table model ❌ |
| `UninstallerPage` | `loadPackages()`, `loadSnapPackages()` | **Yes** — list population ❌ |
| `UninstallerPage` | uninstall lambda | Mostly no (uses `SignalMapper` signals) ⚠ |
| `Feedback` | send lambda | Mostly no (uses own signals), except two `setText` calls ⚠ |

The System Cleaner constructor even registers metatypes with the comment *"needed to suppress qt
warnings (signal/slot <> threads)"* — an acknowledgement of the violation rather than a fix.

These work in practice because Qt's widget calls are not internally synchronised but rarely
*crash* on x86 with a single writer; they are nonetheless undefined behaviour and a likely
source of the sporadic freezes users reported.

The correct pattern is the one `ServicesPage` uses: worker computes, emits a signal, an
auto-connection queues it to the GUI thread, the slot updates widgets.

## 4. Blocking operations on the GUI thread

`CommandUtil::exec` is synchronous with a 10-minute timeout, so any direct call from the GUI
thread freezes the window:

- **Every** `gsettings get`/`set` (GNOME Settings page: ~30 reads on page construction, one
  write per control change).
- Every `pkexec` invocation — the window is frozen for as long as the polkit dialog is open
  (service toggles, APT edits, hosts save, cleaner delete, process kill).
- `ServiceItem`'s verification re-reads (`systemctl is-enabled/is-active`) after each toggle.
- `CpuInfo::getAvgClock()`'s `lscpu`, once per second, from the Dashboard timer.
- `SystemInfo`'s constructor `lscpu`, twice at startup.
- `FileUtil::getFileSize()` recursion (System Cleaner scan is on a worker; but the Cleaner's
  post-clean size recomputation runs in the same worker, and `getFileSize` is also called from
  the GUI thread when rebuilding root labels).

## 5. Signal topology

```
SignalMapper (singleton QObject)
 ├─ sigChangedAppTheme      ← AppManager::updateStylesheet()
 │                            ← ResourcesPage (on pie filter change, to re-colour)
 │     → CircleBar          (chart background, trail colour)
 │     → HistoryChart × 5   (axis/label/grid/legend colours, background)
 │     → disk pie card      (slice labels, background, title brush)
 │     → SystemCleanerPage  (recreate loading QMovies)   ← doubles as deferred init
 ├─ sigUninstallStarted     ← uninstall worker → UninstallerPage::uninstallStarted()
 └─ sigUninstallFinished    ← uninstall worker → loadPackages() + loadSnapPackages()

Page-local signals
 ├─ DashboardPage::sigShowUpdateBar   → widgetUpdateBar->show()
 ├─ ServicesPage::loadServicesS       → loadServices()            (thread hand-off)
 ├─ StartupApp::deleteAppS            → StartupAppsPage::loadApps()
 ├─ StartupApp::editStartupAppS       → StartupAppsPage::openStartupAppEdit(path)
 ├─ StartupAppEdit::startupAppAdded   → StartupAppsPage::loadApps()
 ├─ APTSourceEdit::saved              → APTSourceManagerPage::loadAptSources()
 └─ Feedback::{setErrorMessageS, clearInputsS, disableElementsS} → own slots (thread hand-off)

Filesystem
 └─ QFileSystemWatcher(~/.config/autostart) → StartupAppsPage::loadApps()
```

`sigChangedAppTheme` being emitted exactly once at startup (and then only on the Resources
pie-filter path) means several widgets rely on it as an *initialisation* hook, not just a theme
hook. That coupling is easy to miss when porting: if the rebuild has no theme-change event at
startup, those widgets never initialise.

## 6. Caches and staleness

| Cached value | Where | Invalidated |
| --- | --- | --- |
| Logical / physical core count | function-static in `CpuInfo` | never |
| Disk names for I/O stats | function-static in `DiskInfo::getDiskIO` | never |
| CPU idle/total deltas | function-static vectors in `CpuInfo` | never (shared by all callers) |
| Default network interface | `NetworkInfo` member, set in ctor | never |
| CPU model / speed / username | `SystemInfo` members, set in ctor | never |
| `PackageTool::currentPackageTool` | static const | never (set at static-init time) |
| Service list | `ServicesPage::mServices` | never (loaded once per app run) |
| Package lists | list widgets | on uninstall finish |
| APT source list | list widget | on add / delete (not on toggle or edit) |
| `/etc/hosts` content | `HostManage::mHostFileContent` | on manual save only |
| Autostart entries | list widget | on `QFileSystemWatcher` event |
| Network rate baselines / max | function-statics in the two update slots | never |

The "never invalidated" rows are what makes hot-plugging (USB disk, Wi-Fi switch, docking) and
external changes (a service enabled in a terminal) invisible to a running Stacer.

## 7. Recommended model for the rebuild

```
┌──────────────── Tauri frontend (React) ────────────────┐
│  subscribes to events, renders; no polling of its own   │
└───────────────▲──────────────────────┬─────────────────┘
                │ events               │ commands (invoke)
┌───────────────┴──────────────────────▼─────────────────┐
│  Rust backend                                           │
│   • one sampler task per metric family, single owner of  │
│     delta state, fixed tick, emits snapshots             │
│   • ring buffers for history (60 s) kept in the backend  │
│   • on-demand queries (processes, services, packages)    │
│     as async commands with cancellation                  │
│   • privileged operations behind one audited helper      │
└─────────────────────────────────────────────────────────┘
```

Key changes from the original: **one** owner of sampling state (no shared statics, no duplicate
samplers), sampling paused when no subscriber is listening, all long operations async and
cancellable, and no widget mutation from workers because the frontend only reacts to events.
