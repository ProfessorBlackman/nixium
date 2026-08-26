# 07 — Feature: Dashboard

**Files:** `stacer/Pages/Dashboard/{dashboard_page,circlebar,linebar}.{h,cpp,ui}`
**Window title:** `Dashboard` · **Sidebar button:** `btnDash` · Default start page.

## What it shows

1. Three donut gauges — **CPU**, **MEMORY**, **DISK** (`CircleBar`).
2. Two horizontal bars — **DOWNLOAD**, **UPLOAD** (`LineBar`) with per-second rate and
   cumulative total.
3. A system information list (`QListView` + `QStringListModel`).
4. A hidden "update available" banner (`widgetUpdateBar`) with a download button.

Gauge gradients are hard-coded per metric:
CPU `#A8E063 → #56AB2F`, Memory `#FFB75E → #ED8F03`, Disk `#DC2430 → #7B4397`.

## Timers

| Timer | Interval | Slots |
| --- | --- | --- |
| `mTimer` | 1 s | `updateCpuBar`, `updateMemoryBar`, `updateNetworkBar` |
| `timerDisk` | 5 s | `updateDiskBar` |

All four update functions are also called once during `init()` so the page is populated
immediately. **The timers start in the constructor and are never stopped**, so the Dashboard
keeps polling while other pages are visible and while the window is hidden in the tray.

## Metric computation

### CPU

```cpp
int cpuUsedPercent = im->getCpuPercents().at(0);       // aggregate line of /proc/stat
double ghz         = im->getCpuClock() / 1000.0;        // lscpu CPU MHz
mCpuBar->setValue(cpuUsedPercent, "<ghz> GHz\n<pct>%");
```

`getCpuClock()` spawns `bash -c "LANG=nl_NL.UTF-8 lscpu"` — **once per second, forever**, which
is the app's single largest source of idle CPU cost.

Note that `getCpuPercents()` mutates the shared static delta state inside `CpuInfo`; the
Resources page calls the same function on its own 1 s timer, so when both pages exist the two
callers halve each other's sampling window. Values remain plausible but are not what either
page thinks it is measuring.

### Memory

```cpp
im->updateMemoryInfo();
memUsedPercent = memUsed / memTotal * 100;              // guarded against memTotal == 0
label = "<formatBytes(used)> / <formatBytes(total)>";
```

### Disk

```cpp
im->updateDiskInfo();                                   // rebuilds all Disk* objects
disk = first disk whose name == SettingManager::getDiskName()
     ?: first disk whose name == QStorageInfo::root().displayName()
     ?: disks.at(0);
diskPercent = used / size * 100;                         // guarded against size == 0
label = "<formatBytes(used)> / <formatBytes(size)>";
```

### Network

```cpp
static quint64 l_RX = getRXbytes(), l_TX = getTXbytes();
static quint64 max_RX = 1 MiB, max_TX = 1 MiB;          // auto-scaling ceiling

d_RX = getRXbytes() - l_RX;                              // bytes in the last tick
downPercent = d_RX / max_RX * 100;
max_RX = max(max_RX, d_RX);                              // ceiling only ever grows
```

So the bars are relative to the highest throughput observed since launch (floor 1 MiB/s), and
the label shows `"<rate>/s"` plus `tr("Total: %1")` for the cumulative counter. Because the
maximum never decays, a single burst permanently flattens the bars.

## Threshold alerts

Each of the three gauges checks its configured threshold
(`CPUAlertPercent` / `MemoryAlertPercent` / `DiskAlertPercent`; `0` disables) and, when
exceeded, calls

```cpp
AppManager::ins()->getTrayIcon()->showMessage(
    tr("High CPU Usage"), tr("The amount of CPU used is over %1%.").arg(pct),
    QSystemTrayIcon::Warning);
```

Re-notification is suppressed by a **function-static `bool isShow`** that is cleared on the
first breach and only re-armed when the value drops back *below* the threshold. Since the flag
is function-static rather than per-instance, it is process-global — fine here because there is
only ever one Dashboard.

Titles used: `High CPU Usage`, `High Memory Usage`, `High Disk Usage`.

## System information list

Built once in `systemInformationInit()` from a **locally constructed `SystemInfo`** (a second
`lscpu` run at startup, in addition to `InfoManager`'s):

```
Hostname: %1        Platform: %1          Distribution: %1
Kernel Release: %1  CPU Model: %1         CPU Core: %1        CPU Speed: %1
```

Each line is a translated `tr("Hostname: %1")`-style string, so the labels are localised but
the list is a flat `QStringListModel` — not a key/value table.

## Update check

In `init()`:

```cpp
QNetworkAccessManager *nam = new QNetworkAccessManager(this);
nam->get(QNetworkRequest(QUrl("https://api.github.com/repos/oguzhaninan/Stacer/releases/latest")));
// on finished, if no error:
//   parse JSON, regex ([0-9].[0-9].[0-9]) against tag_name
//   if extracted version != qApp->applicationVersion()  →  emit sigShowUpdateBar()
```

`sigShowUpdateBar` is connected directly to `widgetUpdateBar->show()`. The button opens the
releases page via `QDesktopServices::openUrl`. Comparison is a plain string inequality, so any
difference (including a *lower* upstream version) shows the banner.

## Port notes

- Replace the 1 Hz `lscpu` subprocess with a read of
  `/sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq` or `/proc/cpuinfo`'s `cpu MHz`.
- The auto-scaling network ceiling should decay (or use a rolling window maximum).
- Alert de-duplication belongs in state, not a function-static.
- Prefer one shared sampler with a single delta history over letting two pages both call the
  same stateful `getCpuPercents()`.
