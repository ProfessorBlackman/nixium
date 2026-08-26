# 04 — System Interfaces

The complete inventory of what Stacer reads, writes and executes. This is the contract the
Rust rebuild has to reproduce.

---

## 1. `/proc` — kernel state

| Path | Read by | Parsed as |
| --- | --- | --- |
| `/proc/cpuinfo` | `CpuInfo::getCpuCoreCount` | count of `^processor` lines |
| `/proc/cpuinfo` | `CpuInfo::getCpuPhysicalCoreCount` | distinct `(physical id, core id)` pairs |
| `/proc/cpuinfo` | `CpuInfo::getClocks` | every `^cpu MHz` line, value after `:` |
| `/proc/stat` | `CpuInfo::getCpuPercents` | lines `0..nCpus`, 10 jiffy fields each, delta-based utilisation |
| `/proc/loadavg` | `CpuInfo::getLoadAvgs` | first three whitespace fields |
| `/proc/meminfo` | `MemoryInfo::updateMemoryInfo` | 8 filtered lines by positional index, kB → bytes |
| `/proc/mounts` | *(macro `PROC_MOUNTS` defined in `disk_info.h`, never used)* | — |

Sampling cadence: `/proc/stat`, `/proc/loadavg`, `/proc/meminfo` at **1 Hz** while the
Dashboard or Resources page exists (both pages run their own 1 s timers regardless of
visibility).

## 2. `/sys` — device state

| Path | Read by | Meaning |
| --- | --- | --- |
| `/sys/block/` | `DiskInfo::getDiskNames` | directory listing; entries with a `device/` child are real disks |
| `/sys/block/<dev>/stat` | `DiskInfo::getDiskIO` | field 2 × 512 = bytes read, field 6 × 512 = bytes written |
| `/sys/class/net/<if>/statistics/rx_bytes` | `NetworkInfo::getRXbytes` | cumulative counter |
| `/sys/class/net/<if>/statistics/tx_bytes` | `NetworkInfo::getTXbytes` | cumulative counter |

Rates are always computed by the *caller* as `(current - previous)` over its own 1 s tick;
`stacer-core` only exposes raw counters.

## 3. Qt-mediated system state (no direct file access)

| API | Provides | Underlying source |
| --- | --- | --- |
| `QStorageInfo::mountedVolumes()` | mount list, size/free, fs type, device, display name | `/proc/mounts` + `statvfs` |
| `QStorageInfo::root()` | the root volume, used as the default disk | same |
| `QNetworkInterface::allInterfaces()` | interface names + flags | netlink/ioctl |
| `QSysInfo::machineHostName()` | hostname | `gethostname` |
| `QSysInfo::kernelType/kernelVersion` | `linux`, release string | `uname` |
| `QSysInfo::prettyProductName()` | distribution name | `/etc/os-release` |
| `QSysInfo::currentCpuArchitecture()` | e.g. `x86_64` | compile/runtime detection |
| `QStandardPaths::AppConfigLocation` | `~/.config/stacer` | XDG |
| `QStandardPaths::ConfigLocation` | `~/.config` | XDG |
| `QStandardPaths::HomeLocation` | `$HOME` | — |
| `QStandardPaths::findExecutable` | tool availability probing | `PATH` scan |
| `QFileSystemWatcher` | live reload of `~/.config/autostart` | inotify |
| `QNetworkAccessManager` | GitHub latest-release check | HTTPS |
| `QDesktopServices::openUrl` | open folders / web links | xdg-open |

## 4. Files read

| Path | Purpose | Feature |
| --- | --- | --- |
| `/etc/passwd` | user names for the search owner filter | Search |
| `/etc/group` | group names for the search group filter | Search |
| `/etc/hosts` | host table | Helpers → Host Manage |
| `/etc/apt/sources.list` | APT entries | APT Source Manager |
| `/etc/apt/sources.list.d/*.list` | APT entries (sorted by mtime) | APT Source Manager |
| `/var/crash/*` | crash report file list + sizes | System Cleaner |
| `/var/log/*` (files only) | log file list + sizes | System Cleaner |
| `/var/cache/apt/archives/*` | package cache list + sizes | System Cleaner |
| `/var/cache/pacman/pkg/*` | package cache list + sizes | System Cleaner (Arch) |
| `~/.cache/*` | app cache list + sizes | System Cleaner |
| `~/.local/share/Trash/` | total trash size | System Cleaner |
| `~/.config/autostart/*.desktop` | autostart entries | Startup Apps |
| `~/.config/autostart/stacer.desktop` | own autostart state | Settings |
| `~/.config/stacer/settings.ini` | preferences | all |

## 5. Files written

| Path | How | Privilege |
| --- | --- | --- |
| `~/.config/stacer/settings.ini` | `QSettings` | user |
| `~/.config/autostart/<app>.desktop` | `FileUtil::writeFile` | user |
| `~/.config/autostart/stacer.desktop` | `FileUtil::writeFile` / `QFile::remove` | user |
| `~/.local/share/Trash/info/<name>.trashinfo` | `FileUtil::writeFile` | user |
| `/tmp/stacer_etc_host_new_content` | `FileUtil::writeFile` | user |
| `/etc/hosts` | `pkexec mv` from the temp file | **root** |
| `/etc/apt/sources.list{,.d/*}` | `pkexec tee <path>` with stdin | **root** |
| arbitrary paths | `pkexec rm -rf` / `rm -rf` | **root** or user |
| `~/.local/share/Trash/files/` | `mv` or `pkexec mv` | user / **root** |

Directories created if missing: `~/.config/autostart` (both Startup Apps and Settings pages).
Directories removed recursively: `~/.local/share/Trash/files`, `~/.local/share/Trash/info`
(via `QDir::removeRecursively`, unprivileged).

## 6. External binaries executed

Every invocation goes through `CommandUtil`. "sudo" means wrapped in `pkexec`.

### Information gathering (unprivileged)

| Command | Caller |
| --- | --- |
| `bash -c "LANG=nl_NL.UTF-8 lscpu"` | `SystemInfo` ctor, `CpuInfo::getAvgClock` |
| `whoami` | `SystemInfo` ctor (only if `$USER`/`$USERNAME` unset) |
| `ps ax -weo pid,rss,pmem,vsize,uname:50,pcpu,start_time,state,group,nice,cputime,session,cmd --no-headings` | `ProcessInfo::updateProcesses` |
| `systemctl list-unit-files -t service -a --state=enabled,disabled` | `ServiceTool` |
| `systemctl cat <unit>` | `ServiceTool::getServiceDescription` (per unit) |
| `systemctl is-active <unit>` | `ServiceTool` (per unit, and after each toggle) |
| `systemctl is-enabled <unit>` | `ServiceTool` (after each toggle) |
| `bash -c "dpkg --get-selections 2> /dev/null"` | `PackageTool` (APT) |
| `bash -c "rpm -qa 2> /dev/null"` | `PackageTool` (DNF/YUM) |
| `bash -c "pacman -Q 2> /dev/null"` | `PackageTool` (Arch) |
| `snap list` | `PackageTool::getSnapPackages` |
| `gsettings list-relocatable-schemas` | `GnomeSettingsTool::checkUnityAvailable` |
| `gsettings get <schema>[:<path>] <key>` | every GNOME settings control, on page load |
| `find <dir> [predicates…]` | Search page |
| `curl -d <json> -H 'Content-Type: application/json' -X POST <url>` | Feedback dialog |

### Mutations (privileged unless noted)

| Command | Caller | Privilege |
| --- | --- | --- |
| `gsettings set <schema>[:<path>] <key> <value>` | GNOME settings | user (per-user dconf) |
| `kill <pid>` | Processes page, own process | user |
| `pkexec kill <pid>` | Processes page, other user's process | root |
| `pkexec systemctl enable\|disable <unit>` | Services page | root |
| `pkexec systemctl start\|stop <unit>` | Services page | root |
| `pkexec apt-get remove -y <pkgs…>` | Uninstaller (APT) | root |
| `pkexec dnf remove -y <pkgs…>` | Uninstaller (DNF) | root |
| `pkexec yum remove -y <pkgs…>` | Uninstaller (YUM) | root |
| `pkexec pacman <pkgs…> --noconfirm -R` | Uninstaller (Arch) | root |
| `pkexec snap remove <pkgs…>` | Uninstaller (snap) | root |
| `pkexec add-apt-repository -y <repo> [-s]` | APT Source Manager | root |
| `pkexec tee <sources file>` (content on stdin) | APT Source Manager | root |
| `pkexec mv /tmp/stacer_etc_host_new_content /etc/hosts` | Host Manage | root |
| `pkexec rm -rf <files…>` | System Cleaner | root |
| `rm -rf <path>` / `pkexec rm -rf <path>` | Search → Delete | user / root |
| `mv <path> ~/.local/share/Trash/files` / `pkexec mv …` | Search → Move Trash | user / root |

Tool-availability probes (`QStandardPaths::findExecutable`): `apt-get`, `dnf`, `yum`, `pacman`,
`zypper`, `snap`, `gsettings`.

## 7. Network endpoints

| URL | Method | Trigger | Transport |
| --- | --- | --- | --- |
| `https://api.github.com/repos/oguzhaninan/Stacer/releases/latest` | GET | Dashboard construction | `QNetworkAccessManager` |
| `https://github.com/oguzhaninan/Stacer/releases/latest` | opened in browser | "download update" button | `QDesktopServices` |
| `https://stacer-web-api.herokuapp.com/feedback` | POST JSON | Feedback dialog | `curl` subprocess |
| `https://www.patreon.com/oguzhaninan` | opened in browser | Settings → Donate | `QDesktopServices` |

The update check parses `tag_name` with `([0-9].[0-9].[0-9])` and compares it as a string to
`qApp->applicationVersion()`. The Heroku feedback endpoint no longer exists (free Heroku dynos
were retired), so the feedback feature is dead in practice.

## 8. Environment variables

| Variable | Used for |
| --- | --- |
| `$USER`, `$USERNAME` | current username (before falling back to `whoami`) |
| `$DESKTOP_SESSION` | matched against `ubuntu` to decide whether to show the GNOME Settings page |
| `$LANG` | *set* to `nl_NL.UTF-8` for `lscpu` invocations |
| XDG config vars | indirectly, through `QStandardPaths` |

## 9. Desktop-environment integration

- **System tray** — `QSystemTrayIcon` with a context menu mirroring the sidebar plus Quit;
  used for the CPU/memory/disk threshold notifications (`showMessage`, warning icon).
- **Autostart** — XDG `~/.config/autostart` `.desktop` files; enabled/disabled state is encoded
  as `Hidden=` (preferred) or `X-GNOME-Autostart-enabled=`.
- **Trash** — the freedesktop.org trash spec is implemented *by hand*: move the file into
  `~/.local/share/Trash/files` and write a `.trashinfo` file with `Path=` and `DeletionDate=`
  into `~/.local/share/Trash/info`.
- **Icon theme** — `QIcon::fromTheme(name, fallback)` is used to give package and file entries
  real icons, falling back to a bundled image or `application-x-executable`.
- **polkit** — every privileged action is an independent `pkexec` invocation, so each one
  raises its own authentication dialog.

## 10. Distro/feature gating summary

| Feature | Gate |
| --- | --- |
| APT Source Manager page | `/etc/apt/sources.list.d` exists |
| GNOME Settings page | `$DESKTOP_SESSION` or distro name contains `ubuntu` |
| Unity sub-page | `gsettings list-relocatable-schemas` matches Stacer's Unity schema set |
| Snap tab in Uninstaller | `snap` on `PATH` |
| Package list / uninstall | `PackageTool::currentPackageTool != UNKNOWN` (and not `ZYPPER`) |
| Package cache cleaning | APT → `/var/cache/apt/archives`, Arch/DNF/YUM → `/var/cache/pacman/pkg` (see quirks) |
| Services page | `systemctl` present (no explicit check — an empty list is shown otherwise) |
