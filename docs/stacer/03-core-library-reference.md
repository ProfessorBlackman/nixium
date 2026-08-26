# 03 — `stacer-core` API Reference

Complete signature-level reference for the system-access library. Paths and commands are
cross-referenced in [04-system-interfaces.md](04-system-interfaces.md).

Error convention throughout: `CommandUtil` **throws a bare `QString`**; call sites use
`try { … } catch (QString &ex) { qCritical() << ex; }` and return an empty/zero value. There
are no error types, no error codes, and no propagation to the UI except in a handful of pages.

---

## Utils

### `CommandUtil` (`Utils/command_util.{h,cpp}`)

```cpp
static QString exec(const QString &cmd, QStringList args = {}, QByteArray data = {});
static QString sudoExec(const QString &cmd, QStringList args = {}, QByteArray data = {});
static bool    isExecutable(const QString &cmd);
```

`exec` semantics:

1. `QProcess::start(cmd, args)` — argument vector, **no shell**, so no quoting issues
   (except where callers deliberately invoke `bash -c "<string>"`).
2. If `data` is non-empty: `write(data)`, `waitForBytesWritten()`, `closeWriteChannel()` —
   this is how `tee` receives new file contents.
3. `waitForFinished(600000)` — **blocking, 10-minute cap**.
4. Reads `readAllStandardOutput()` only. **stderr is discarded**; exit code is never checked.
5. `kill()` then `close()` unconditionally, then throws `process->errorString()` if
   `process->error() != QProcess::UnknownError`.
6. Returns stdout `.trimmed()`.

Consequence: a command that fails cleanly (non-zero exit, message on stderr) looks like
"succeeded with empty output". Only spawn failures (binary missing, crash, timeout) throw.

`sudoExec` pushes the command to the front of the args and runs `pkexec` with them, catching
and logging any throw, and returning `""` on failure. So a user cancelling the polkit prompt
is indistinguishable from success.

`isExecutable` = `!QStandardPaths::findExecutable(cmd).isEmpty()` — a `PATH` lookup.

### `FileUtil` (`Utils/file_util.{h,cpp}`)

```cpp
static QString     readStringFromFile(const QString &path, OpenMode = ReadOnly);
static QStringList readListFromFile  (const QString &path, OpenMode = ReadOnly);
static bool        writeFile(const QString &path, const QString &content,
                             OpenMode = WriteOnly|Truncate);
static QStringList directoryList(const QString &path);   // file names only, no dot entries
static quint64     getFileSize(const QString &path);     // recursive for directories
```

- `readListFromFile` = read whole file, `trimmed()`, `split("\n")`. An unreadable/missing file
  yields a list with **one empty string**, not an empty list — callers checking
  `isEmpty()` are fooled.
- `writeFile` appends a newline (`stream << content.toUtf8() << endl`) and returns
  false if the file could not be opened. No atomic replace, no permission handling.
- `getFileSize` recurses with `QDir::NoDotAndDotDot | Files | Dirs`; it follows symlinked
  directories and does no cycle detection.

### `FormatUtil` (`Utils/format_util.{h,cpp}`)

```cpp
static QString formatBytes(const quint64 &bytes);
static const quint64 KIBI=1024, MEBI=1048576, GIBI=1073741824, TEBI=1099511627776;
```

Thresholds: `1` → `"1 byte"`; `< 1024` → `"N bytes"`; then `%.1f KiB|MiB|GiB|TiB`.
Uses the deprecated `QString::sprintf`. This is the single formatting function used by every
chart label, table cell and tree item in the app.

---

## Info — read-only samplers

### `CpuInfo` (`Info/cpu_info.{h,cpp}`)

```cpp
int           getCpuPhysicalCoreCount() const;   // cached in a function-static
int           getCpuCoreCount() const;           // cached; logical CPUs
QList<int>    getCpuPercents() const;            // [aggregate, cpu0, cpu1, …]
QList<double> getLoadAvgs() const;               // {1min, 5min, 15min}
double        getAvgClock() const;               // MHz, via lscpu
QList<double> getClocks() const;                 // per-core MHz, from /proc/cpuinfo
private:
int           getCpuPercent(const QList<double> &cpuTimes, const int &processor = 0) const;
```

- **Physical cores:** parse `/proc/cpuinfo`, collect the set of `(physical id, core id)`
  pairs, return its size. Assumes `core id` appears *after* `physical id` in each block.
  Returns 0 on architectures that omit those fields (e.g. many ARM SoCs).
- **Logical cores:** count of lines matching `^processor`.
- **Load averages:** first three whitespace-separated fields of `/proc/loadavg`.
- **Per-CPU utilisation:** read `/proc/stat`, take lines `0 .. logicalCores` (so index 0 is
  the aggregate `cpu` line, index *n* is `cpu(n-1)`), split each into its 10 time fields, and
  delta against the previous sample:

  ```
  idle_i  = fields[3] + fields[4]              # idle + iowait
  total_i = sum(all fields)
  util    = 100 * (Δtotal - Δidle) / Δtotal    # clamped to 0..100
  ```

  Previous values live in **function-static `QVector<double>`s sized on first call**
  (`N = logicalCores + 1`). The first call therefore reports utilisation since boot, and each
  subsequent call reports utilisation since the previous call — the sampling interval is
  implicitly whatever the caller's timer is (1 s in practice). Two callers polling the same
  process interfere with each other's deltas (the Dashboard and Resources pages both do this).
- **Average clock:** runs `bash -c "LANG=nl_NL.UTF-8 lscpu"`, filters `^CPU MHz`, takes the
  value after `:`. The `LANG` pin is an attempt at stable field names but selects *Dutch*, and
  Dutch uses `,` as decimal separator — `toDouble()` then truncates at the comma.
  Throws (uncaught here) if no `CPU MHz` line exists — common on VMs and modern kernels that
  only report `CPU max MHz`.

### `MemoryInfo` (`Info/memory_info.{h,cpp}`)

```cpp
void   updateMemoryInfo();
quint64 getMemTotal/getMemFree/getMemUsed() const;
quint64 getSwapTotal/getSwapFree/getSwapUsed() const;
```

`updateMemoryInfo()` filters `/proc/meminfo` with
`^MemTotal|^MemFree|^Buffers|^Cached|^SwapTotal|^SwapFree|^Shmem|^SReclaimable`,
then reads the surviving lines **by positional index 0..7**, converting kB → bytes (`<< 10`):

| Index assumed | Field assigned |
| --- | --- |
| 0 | memTotal |
| 1 | memFree |
| 2 | buffers |
| 3 | cached |
| 4 | swapTotal |
| 5 | swapFree |
| 6 | sreclaimable |
| 7 | shmem |

Derived values (the documented intent, per the comment citing the Red Hat / SO formulas):

```
cached   = Cached + SReclaimable - Shmem
memUsed  = MemTotal - (MemFree + Buffers + cached)
swapUsed = SwapTotal - SwapFree
```

**On current kernels `Shmem` appears before `SReclaimable` in `/proc/meminfo`, so indices 6
and 7 are swapped** and the app effectively computes `Cached + Shmem - SReclaimable`. See
[20-known-quirks-and-bugs.md](20-known-quirks-and-bugs.md). Also note `^Cached` cannot
accidentally match `SwapCached`, but *would* match a future field beginning with "Cached".

### `DiskInfo` (`Info/disk_info.{h,cpp}`)

```cpp
struct Disk { QString name, device, fileSystemType; quint64 size, free, used; };

void              updateDiskInfo();       // rebuilds the Disk* list (deletes the old one)
QList<Disk*>      getDisks() const;
QList<quint64>    getDiskIO() const;      // {totalReadBytes, totalWriteBytes}
QStringList       getDiskNames() const;   // physical block devices
QList<QString>    fileSystemTypes();      // distinct, for the Resources filter combo
QList<QString>    devices();              // distinct, for the Resources filter combo
```

- `updateDiskInfo` enumerates `QStorageInfo::mountedVolumes()`, keeps `isValid()` entries, and
  stores `displayName()`, `device()`, `bytesTotal()`, `bytesTotal()-bytesFree()`,
  `bytesFree()`, `fileSystemType()`. This includes pseudo/virtual mounts (tmpfs, squashfs
  snap loops, overlays) — nothing is filtered out, which is why the disk pie chart is noisy
  and the Resources page offers device / filesystem-type filters.
- `getDiskNames` scans `/sys/block/*` and keeps entries that have a `device` subdirectory —
  i.e. real hardware, excluding loop/ram/dm devices.
- `getDiskIO` reads `/sys/block/<name>/stat` for each of those names, requires ≥ 8 fields,
  and accumulates `field[2] * 512` (sectors read) and `field[6] * 512` (sectors written).
  The disk-name list is captured in a **function-static** on first call, so hot-plugged
  devices never appear.

### `NetworkInfo` (`Info/network_info.{h,cpp}`)

```cpp
NetworkInfo();                                    // picks the default interface, once
QString  getDefaultNetworkInterface() const;
QList<QNetworkInterface> getAllInterfaces();
quint64  getRXbytes() const;
quint64  getTXbytes() const;
```

The constructor iterates `QNetworkInterface::allInterfaces()` and takes the **first**
interface that is `IsUp && IsRunning && !IsLoopBack`, then precomputes
`/sys/class/net/<if>/statistics/rx_bytes` and `…/tx_bytes`. Counters are read fresh on each
getter (`toLong()` on the trimmed contents). If no interface qualifies at construction time,
the paths contain an empty name and both getters return 0 forever — switching from Ethernet
to Wi-Fi mid-session does not re-select.

`getAllInterfaces()` is declared and exists but is not used by any page.

### `SystemInfo` (`Info/system_info.{h,cpp}`)

```cpp
SystemInfo();                                   // resolves cpuModel, cpuSpeed, cpuCore, username
QString getHostname/getPlatform/getDistribution/getKernel() const;
QString getCpuModel/getCpuSpeed/getCpuCore/getUsername() const;
QFileInfoList getCrashReports/getAppLogs/getAppCaches() const;
QStringList   getUserList/getGroupList() const;
```

| Getter | Source |
| --- | --- |
| `getHostname` | `QSysInfo::machineHostName()` |
| `getPlatform` | `"<kernelType> <currentCpuArchitecture>"` e.g. `linux x86_64` |
| `getDistribution` | `QSysInfo::prettyProductName()` (reads `/etc/os-release`) |
| `getKernel` | `QSysInfo::kernelVersion()` |
| `getCpuModel` | `lscpu` → `^Model name`, truncated at `@` if present |
| `getCpuSpeed` | `lscpu` → `^CPU max MHz`, falling back to `^CPU MHz`; `/1000` + `"GHz"` |
| `getCpuCore` | `CpuInfo::getCpuPhysicalCoreCount()` as a string |
| `getUsername` | `$USER`, else `$USERNAME`, else `whoami` |
| `getCrashReports` | `QDir("/var/crash").entryInfoList(Files)` |
| `getAppLogs` | `QDir("/var/log").entryInfoList(Files\|NoDotAndDotDot)` — files only, so `/var/log/apache2/` and friends are skipped |
| `getAppCaches` | `QDir("$HOME/.cache").entryInfoList(Files\|Dirs\|NoDotAndDotDot)` |
| `getUserList` | field 0 of every `/etc/passwd` line |
| `getGroupList` | field 0 of every `/etc/group` line |

The constructor runs `lscpu` under `LANG=nl_NL.UTF-8` (same Dutch-locale issue as `CpuInfo`),
falls back to `tr("Unknown")` for model and speed on throw, and is invoked once per
`SystemInfo` instance — note `DashboardPage::systemInformationInit()` constructs a **second,
local** `SystemInfo` (a second `lscpu` run) besides the one inside `InfoManager`.

### `Process` / `ProcessInfo` (`Info/process.*`, `Info/process_info.*`)

`Process` is a plain value type with getters/setters for:
`pid, rss, pmem, vsize, uname, pcpu, startTime, state, group, nice, cpuTime, session, cmd`.

`ProcessInfo::updateProcesses()` (a slot) runs

```
ps ax -weo pid,rss,pmem,vsize,uname:50,pcpu,start_time,state,group,nice,cputime,session,cmd --no-headings
```

splits each line on `\s+`, requires ≥ 13 fields, `takeFirst()`s the twelve scalars in order and
joins the remainder as `cmd`. `rss` and `vsize` are converted kB → bytes (`<< 10`).
`getProcessList()` returns the cached list by value.

`uname:50` widens the user column so long usernames don't truncate; because parsing is
whitespace-based, a username containing a space would still break the row (as would a
`cmd` — but `cmd` is last, so it is safely re-joined).

---

## Tools — mutating operations

### `ServiceTool` (`Tools/service_tool.{h,cpp}`)

```cpp
class Service { QString name, description; bool status /*enabled*/, active /*running*/; };

static QList<Service> getServicesWithSystemctl();
static bool    serviceIsActive(const QString &name);
static bool    serviceIsEnabled(const QString &name);
static bool    changeServiceStatus(const QString &name, bool enable);   // enable/disable
static bool    changeServiceActive(const QString &name, bool start);    // start/stop
static QString getServiceDescription(const QString &name);
```

`getServicesWithSystemctl()`:

```
systemctl list-unit-files -t service -a --state=enabled,disabled
```

then `.filter(QRegExp("[^@].service"))` to drop template units, and per surviving line:
`name` = first token with `.service` stripped; `status` = last token equals `enabled`;
`description` = `systemctl cat <unit>` filtered to `^Description`, split on `=`;
`active` = `systemctl is-active <unit>` equals `active`.

**That is three `systemctl` invocations per unit** — on a typical desktop with 200+ units this
is 600+ process spawns and takes seconds, which is why the page loads on a worker thread.

Mutations use `sudoExec`, i.e. `pkexec systemctl enable|disable|start|stop <unit>` — one
polkit prompt per toggle.

### `PackageTool` (`Tools/package_tool.{h,cpp}`)

```cpp
enum PackageTools { APT, DNF, YUM, PACMAN, ZYPPER, UNKNOWN };
static const PackageTools currentPackageTool;      // probed once at static-init time

static QFileInfoList getDpkgPackageCaches();       // /var/cache/apt/archives/
static QStringList   getDpkgPackages();            // dpkg --get-selections | grep '\sinstall$'
static bool          dpkgRemovePackages(QStringList);   // pkexec apt-get remove -y …

static QStringList   getRpmPackages();             // rpm -qa
static bool          dnfRemovePackages(QStringList);    // pkexec dnf remove -y …
static bool          yumRemovePackages(QStringList);    // pkexec yum remove -y …

static QFileInfoList getPacmanPackageCaches();     // /var/cache/pacman/pkg/
static QStringList   getPacmanPackages();          // pacman -Q  (first column)
static bool          pacmanRemovePackages(QStringList); // pkexec pacman <pkgs> --noconfirm -R

static QStringList   getSnapPackages();            // snap list (skip header, first column)
static bool          snapRemovePackages(QStringList);   // pkexec snap remove …
```

Detection order: `apt-get` → `dnf` → `yum` → `pacman` → `zypper` → `UNKNOWN`.
`ZYPPER` is detected but **has no list or remove implementation**, so openSUSE users get an
empty package list.

Listing commands are wrapped in `bash -c "<cmd> 2> /dev/null"` to suppress stderr noise.
`pacmanRemovePackages` **appends** its flags after the package names (`pacman <pkgs> --noconfirm -R`)
whereas the others prepend — valid for pacman's parser, but worth noting.

### `AptSourceTool` (`Tools/apt_source_tool.{h,cpp}`)

```cpp
class APTSource {
  QString filePath, options, uri, distribution, components, source;
  bool isSource;   // deb-src vs deb
  bool isActive;   // not commented out
};
using APTSourcePtr = QSharedPointer<APTSource>;

static bool                checkSourceRepository();          // /etc/apt/sources.list.d exists?
static QList<APTSourcePtr> getSourceList();
static void                addRepository(const QString &repo, bool isSource);
static void                changeStatus(APTSourcePtr, bool enable);   // comment / uncomment
static void                changeSource(APTSourcePtr, const QString &newSource);
static void                removeAPTSource(APTSourcePtr);     // == changeSource(src, "")
```

`getSourceList()` enumerates `/etc/apt/sources.list.d/*.list` sorted by mtime, then appends
`/etc/apt/sources.list`. Per file it keeps lines matching
`^\s{0,}#{0,}\s{0,}deb` and parses one-line (legacy, non-deb822) entries:

```
deb [arch=amd64] http://packages.microsoft.com/repos/vscode stable main
│    └ options    └ uri                                     │       └ components
└ deb | deb-src                                             └ distribution
```

- `isActive = !line.startsWith('#')`
- options captured by `(\s[\[]+.*[\]]+)` and then removed from the line
- remaining tokens: `[0]` type, `[1]` uri, `[2]` distribution, `[3..]` components
  (requires > 2 tokens)
- `source` keeps the original text with `#` characters stripped

`changeSource()` reads the whole file, finds the **first line containing** `aptSource->source`
by substring match, replaces or removes it, then writes the file back via
`pkexec tee <path>` with the new content on stdin. `changeStatus()` builds the new line by
removing all `#` then optionally prefixing `"# "`.
`addRepository()` runs `pkexec add-apt-repository -y <repo> [-s]`.

Note: deb822 (`.sources`) files, introduced in newer Debian/Ubuntu, are **not** supported.

### `GnomeSettingsTool` (`Tools/gnome_settings_tool.{h,cpp}`, `Tools/gnome_schema.h`)

```cpp
static GnomeSettingsTool& ins();          // function-local static singleton
bool checkGSettings();                    // is `gsettings` on PATH
bool checkUnityAvailable();               // heuristic over `gsettings list-relocatable-schemas`
QVariant getValue(schema, key, schemaPath = {});
void     setValue(schema, key, value, schemaPath = {});
// typed wrappers: getValueS/B/I/F, setValueS/B/I/F
```

Every access shells out: `gsettings get <schema>[:<path>] <key>` /
`gsettings set <schema>[:<path>] <key> <value>`. The result is wrapped in a `QVariant` built
from the **raw string**, so `"true"`/`"false"` convert correctly for booleans but string values
arrive with their surrounding single quotes — call sites strip them with `.replace("'", "")`.

`gnome_schema.h` is a pure constants header with three namespaces:
`GSchemaPaths` (relocatable Compiz profile paths), `GSchemas` (`Unity`, `Window`, `Appearance`
schema ids), `GSchemaKeys` (matching key names), and `GValues` (two small enums). See
[17-feature-gnome-settings.md](17-feature-gnome-settings.md) for the full table.

`checkUnityAvailable()` is inverted-looking: it lists relocatable schemas and returns `false`
if **any listed schema is not in Stacer's own list of eight Unity schemas** — which on a
non-Unity system is immediately true, so it correctly reports "unavailable", but on any system
with extra relocatable schemas it also reports unavailable. It is effectively "is this a
pristine Unity desktop".
