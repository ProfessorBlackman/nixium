# Stacer (native / Qt) — Reference Documentation

Reverse-engineered documentation of **Stacer 1.1.0** (`Stacer-native/`, C++ / Qt 5 Widgets),
written as the specification source for the **nix** rebuild (Rust + Tauri).

Stacer is a Linux system optimizer and monitor. It is a single-window desktop app with a
sidebar of pages: live resource monitoring, a disk cleaner, a process manager, a systemd
service manager, an autostart editor, a package uninstaller, an APT repository editor, a
file search front-end, an `/etc/hosts` editor, and Unity/GNOME desktop tweaks.

Upstream project is abandoned (README carries an explicit end-of-life notice); this doc set
freezes its behaviour so the rebuild does not have to re-derive it.

## How to read this

| Doc | Contents |
| --- | --- |
| [01-architecture.md](01-architecture.md) | Module layout, layering, singletons, object lifecycles |
| [02-build-and-packaging.md](02-build-and-packaging.md) | CMake/qmake, resources, translations, .deb, install layout, logging |
| [03-core-library-reference.md](03-core-library-reference.md) | `stacer-core` API: every Info / Tools / Utils class and method |
| [04-system-interfaces.md](04-system-interfaces.md) | **Every** file, `/proc`, `/sys` path and external binary Stacer touches |
| [05-privilege-model.md](05-privilege-model.md) | `pkexec` escalation, which operations need root, security notes |
| [06-ui-shell-and-theming.md](06-ui-shell-and-theming.md) | Window shell, sidebar, page slide animation, QSS theming, i18n, tray |
| [07-feature-dashboard.md](07-feature-dashboard.md) | Dashboard: circle bars, net bars, alerts, update check |
| [08-feature-resources.md](08-feature-resources.md) | Resources: 60-second history charts, disk pie chart |
| [09-feature-processes.md](09-feature-processes.md) | Process table, filtering, kill |
| [10-feature-services.md](10-feature-services.md) | systemd unit list, enable/disable, start/stop |
| [11-feature-system-cleaner.md](11-feature-system-cleaner.md) | Scan/clean of caches, logs, crash reports, trash |
| [12-feature-startup-apps.md](12-feature-startup-apps.md) | XDG autostart `.desktop` CRUD |
| [13-feature-uninstaller.md](13-feature-uninstaller.md) | Distro packages + snaps, removal |
| [14-feature-apt-source-manager.md](14-feature-apt-source-manager.md) | `sources.list` parse/edit/add/remove |
| [15-feature-search.md](15-feature-search.md) | `find(1)` query builder, result actions (trash/delete) |
| [16-feature-helpers-hosts.md](16-feature-helpers-hosts.md) | `/etc/hosts` table editor |
| [17-feature-gnome-settings.md](17-feature-gnome-settings.md) | `gsettings` / Compiz / Unity tweak surface |
| [18-feature-settings-and-feedback.md](18-feature-settings-and-feedback.md) | Preferences, autostart-self, alert thresholds, feedback form |
| [19-data-flow-and-concurrency.md](19-data-flow-and-concurrency.md) | Timers, threads, sampling cadence, cross-thread UI writes |
| [20-known-quirks-and-bugs.md](20-known-quirks-and-bugs.md) | Defects and fragile parsing to **not** reproduce |
| [21-tauri-rust-port-notes.md](21-tauri-rust-port-notes.md) | Feature → Rust/Tauri mapping, suggested IPC surface, gaps |

## A note on licensing

Stacer is licensed **GPL-3.0**. These documents describe its behaviour and quote short excerpts of
its source for analysis and commentary — the only Stacer-derived material anywhere in this
repository. **nix itself shares no code with Stacer**; it is a from-scratch rewrite, and the
similarity of licence is a coincidence of what desktop system utilities conventionally use.

nix is GPL-3.0-or-later, so there is no compatibility question either way.

## At a glance

- **Version documented:** 1.1.0 (`stacer/main.cpp`, `release.sh`)
- **Language / toolkit:** C++11, Qt 5 (`Core Gui Widgets Charts Svg Concurrent Network`)
- **Size:** ~2.1k LOC library + ~6.7k LOC GUI + 26 `.ui` forms + 2 QSS themes
- **Runtime deps:** `systemd` (systemctl), `curl`, `pkexec` (polkit); optional
  `apt-get`/`dnf`/`yum`/`pacman`/`zypper`, `snap`, `gsettings`, `add-apt-repository`, `lscpu`,
  `ps`, `find`, `rm`, `mv`, `tee`, `kill`, `bash`
- **Config:** `$XDG_CONFIG_HOME/stacer/settings.ini` + `stacer.log` (Qt `AppConfigLocation`)
- **Privilege model:** no daemon, no setuid; individual commands re-run under `pkexec`
- **Distro coverage:** Debian/Ubuntu is first class; Fedora/Arch/openSUSE partially; some
  pages hide themselves when their backing tool is missing
- **Translations:** 26 locales via Qt Linguist `.ts` → `.qm`

## Structural map of the original source

```
Stacer-native/
├── CMakeLists.txt          top-level: finds Qt5, adds both subprojects
├── Stacer.pro              qmake equivalent (subdirs)
├── stacer-core/            STATIC library — all system access, no GUI
│   ├── Info/               read-only sampling: cpu, memory, disk, network, system, process
│   ├── Tools/              mutating operations: services, packages, apt sources, gsettings
│   └── Utils/              command exec, file IO, byte formatting
├── stacer/                 GUI executable
│   ├── main.cpp            entry point, splash, CLI flags, file logger
│   ├── app.{h,cpp,ui}      main window, sidebar, tray, quit dialog
│   ├── Managers/           singleton facades over stacer-core + app/settings state
│   ├── Pages/              one directory per sidebar page
│   ├── static/             QSS themes, images, fonts, languages.json (compiled into .qrc)
│   └── static.qrc          Qt resource manifest
├── translations/           26 × stacer_<locale>.ts
├── applications/           stacer.desktop
├── icons/                  hicolor 16→256 px
└── debian/                 packaging control files
```
