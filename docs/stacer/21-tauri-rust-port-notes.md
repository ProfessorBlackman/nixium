# 21 — Port Notes: Stacer → nix (Rust + Tauri)

Derived from the analysis in this doc set. This is a mapping and a set of decisions to make —
not a plan of record.

## 1. What maps where

The original's `stacer-core` / GUI split lines up almost exactly with Tauri's
`src-tauri` / frontend split. `stacer-core` has no Qt GUI dependency, so it is a direct
translation target for the Rust side; `Managers/` becomes the command/event surface; `Pages/`
becomes React views.

```
stacer-core/Info/*      →  src-tauri: samplers + one-shot queries
stacer-core/Tools/*     →  src-tauri: mutating operations (+ privileged helper)
stacer-core/Utils/*     →  src-tauri: small helpers (byte formatting can live in the frontend)
stacer/Managers/*       →  #[tauri::command] surface + emitted events + a settings store
stacer/Pages/*          →  React routes/views
stacer/static/themes/*  →  CSS variables (the values.ini token list is the palette contract)
translations/*.ts       →  i18n JSON
```

## 2. Suggested backend surface

### Streaming (backend pushes, frontend subscribes)

| Event | Payload | Cadence | Replaces |
| --- | --- | --- | --- |
| `metrics://cpu` | aggregate % , per-core % , per-core MHz, load averages | 1 s | `CpuInfo` |
| `metrics://memory` | total/used/free/available, swap total/used | 1 s | `MemoryInfo` |
| `metrics://network` | per-interface rx/tx bytes + rates | 1 s | `NetworkInfo` |
| `metrics://disk-io` | per-device read/write bytes + rates | 1 s | `DiskInfo::getDiskIO` |
| `metrics://mounts` | mount list with size/used/fs type/device | 5 s | `DiskInfo::updateDiskInfo` |

Rules the original got wrong and the rebuild should get right:

- **One owner of delta state.** Sample once per tick in the backend and fan out; never let two
  views drive the same stateful sampler.
- **Pause when nobody is listening**, and stop when the window is hidden if no alert threshold
  is armed.
- **Keep history in the backend** (60-sample ring buffers per series) so a view that mounts late
  gets the full window immediately, and the frontend never has to shift points by hand.

### Commands (request/response)

| Command | Returns / does | Replaces |
| --- | --- | --- |
| `system_info()` | hostname, distro, kernel, arch, CPU model/cores/speed, uptime | `SystemInfo` |
| `list_processes()` | structured rows (pid, rss, vsize, %mem, %cpu, user, state, start, cmd…) | `ProcessInfo` |
| `kill_process(pid, signal)` | result, with a real error | Processes page |
| `list_services()` | name, description, enabled, active, sub-state | `ServiceTool` |
| `set_service_enabled(unit, bool)` / `set_service_active(unit, bool)` | result | `ServiceTool` |
| `list_packages()` / `list_snaps()` | name, version, size, summary | `PackageTool` |
| `remove_packages(ids)` | result + what would cascade | `PackageTool` |
| `scan_cleanable(categories)` | streamed entries + running totals, cancellable | System Cleaner |
| `clean(paths)` | freed bytes, per-path result | System Cleaner |
| `list_autostart()` / `upsert_autostart()` / `remove_autostart()` / `set_autostart_enabled()` | XDG `.desktop` CRUD | Startup Apps |
| `list_apt_sources()` / `mutate_apt_source()` / `add_apt_repository()` | APT entries | APT Source Manager |
| `search(query)` | streamed results, cancellable | Search |
| `read_hosts()` / `write_hosts()` | `/etc/hosts` entries with comments preserved | Host Manage |
| `get_settings()` / `set_setting()` | preferences | `SettingManager` |

Everything that can take more than ~100 ms should be async with a cancellation token —
notably package listing, the service scan, the cleaner scan and search.

## 3. Data sources: keep or change

| Original | Recommendation |
| --- | --- |
| `/proc/stat`, `/proc/loadavg`, `/proc/meminfo`, `/proc/cpuinfo` | **Keep** — parse into maps, never by line index |
| `/sys/block/*/stat`, `/sys/class/net/*/statistics/*` | **Keep** — but re-enumerate devices periodically (the original caches disk names forever) |
| `QStorageInfo::mountedVolumes()` | Replace with `/proc/self/mountinfo` + `statvfs`; **filter pseudo-filesystems by default** |
| `ps` subprocess | Replace with direct `/proc/<pid>` reads (`stat`, `statm`, `status`, `cmdline`) |
| `lscpu` subprocess (1 Hz!) | Replace with `/proc/cpuinfo` `cpu MHz` or `/sys/.../cpufreq/scaling_cur_freq` |
| `systemctl` × `2N` subprocesses | Replace with the systemd D-Bus API (one round trip, plus change signals) |
| `dpkg`/`rpm`/`pacman`/`snap` subprocesses | Keep subprocesses (no good Rust bindings) but parse machine-readable output (`dpkg-query -W -f=…`, `rpm -qa --qf`, `pacman -Qq`) |
| `find` subprocess | Replace with a Rust directory walker — real errors, streaming, cancellation |
| `gsettings` × ~30 subprocesses | Read/write dconf directly, or batch; gate per-key on schema existence |
| `curl` subprocess (feedback) | Drop the feature, or use a real HTTP client |
| First non-loopback interface, chosen once | Re-evaluate on network change; let the user pick, or aggregate all interfaces |

Existing crates worth evaluating before hand-rolling: `sysinfo` (broad, cross-platform),
`procfs` (typed `/proc` access), `zbus` (systemd D-Bus), `walkdir`/`jwalk` (search),
`nix` (signals, `statvfs`), `freedesktop-desktop-entry` (`.desktop` parsing),
`trash` (freedesktop trash spec). `sysinfo` covers CPU/memory/disk/process cheaply; `procfs`
is the better fit where the original's exact semantics matter.

## 4. Privilege model — the decision that matters most

The original's approach (one `pkexec <command>` per action, exit code ignored) is the single
biggest thing to redesign. See [05-privilege-model.md](05-privilege-model.md). Options:

1. **One privileged helper binary + polkit policy.** The helper exposes a small, typed,
   allow-listed set of operations (write hosts file, write apt source, remove packages,
   delete paths from an approved category, toggle unit). The GUI talks to it over a socket or
   D-Bus. One authentication, auditable surface, real errors.
2. **Per-action `pkexec`, done properly** — check exit status, capture stderr, surface failures,
   batch where possible. Cheaper to build; still one prompt per action.
3. **Delegate what already has a policy:** systemd's D-Bus API and PackageKit both integrate
   with polkit natively, so services and packages need no custom helper at all.

Whatever is chosen: **never** shell out to `rm -rf` as root with a UI-built argument list, and
never stage a root-destined file at a fixed `/tmp` path (the `/etc/hosts` race).

## 5. Feature-by-feature disposition

| Feature | Disposition |
| --- | --- |
| Dashboard | Port as-is. Fix the auto-scaling network ceiling (decay/rolling max) and drop the 1 Hz `lscpu`. |
| Resources | Port. Move history buffers to the backend; filter pseudo-filesystems in the disk chart; support >20 cores. |
| Processes | Port. Read `/proc` directly, diff-update rows, add confirmation + SIGTERM→SIGKILL. Compute real instantaneous %CPU. |
| Services | Port via D-Bus. Add refresh + live state; keep the running × enabled filter. |
| System Cleaner | Port carefully. Async cancellable scan, confirmation, per-manager cache cleaning (`apt-get clean` etc.), spec-compliant trash, sane `/var/log` handling. |
| Startup Apps | Port. Fix the default-enabled semantics; atomic writes; show system-wide entries read-only. |
| Uninstaller | Port. Structured package data, cascade preview, confirmation. Consider flatpak; implement or hide zypper. |
| APT Source Manager | Port **and extend to deb822 `.sources`**; track line numbers; support `signed-by`. |
| Search | Port with a native walker; streaming results; confirmation on destructive actions. |
| Helpers → Hosts | Port. Atomic privileged write, external-change detection, real IP validation, delete removes the line. |
| GNOME Settings | **Rethink.** The Unity/Compiz surface is dead. Either drop it or build a small current-GNOME tweak page gated on `$XDG_CURRENT_DESKTOP` and per-key schema existence. |
| Settings | Port. Store a stable page **id** (not a translated name), restore theme selection, live language switching. |
| Feedback | Drop, or replace with "open a GitHub issue" in the browser. |
| Update check | Drop (upstream is abandoned) or point at the new project's releases with real semver comparison. |
| Tray + quit-to-tray | Port — this is genuinely useful and cheap (`tauri-plugin-*` tray APIs). |
| Splash screen | Drop. It only existed because all 12 pages were built eagerly; lazy views make it unnecessary. |

## 6. Frontend notes

- **Lazy views.** Nothing should sample or query until its view is mounted. This alone removes
  the splash screen and most of the idle cost.
- **Theming:** port `values.ini` to CSS custom properties. Both `default` (dark) and `light`
  palettes already exist in the original and are complete — restoring light/dark is nearly free.
  Token list is in [06-ui-shell-and-theming.md](06-ui-shell-and-theming.md).
- **Charts:** the original's donut gauges (hole 0.67, −115°…115°, two-stop conical gradient) and
  60-second spline charts with the legend used as a live readout are the visual identity worth
  keeping. The gauge gradients are documented in
  [07-feature-dashboard.md](07-feature-dashboard.md).
- **Formatting:** reproduce `FormatUtil::formatBytes` exactly (binary units, one decimal,
  `"1 byte"` / `"N bytes"` special cases) so numbers match user expectations from the original.
- **i18n:** 26 locales exist as Qt `.ts` files. They are convertible (XML → JSON) and are worth
  harvesting rather than restarting translation from scratch. Remember `ar` needs RTL layout.
- **Empty/loading states:** every list in the original has a "not found" placeholder and a
  spinner-swaps-for-button pattern. Keep the affordance, drop the animated GIFs.

## 7. Things the original never did, worth considering

- Per-process CPU/IO history and a per-process kill/renice/priority UI.
- Temperature and fan sensors (`/sys/class/hwmon`).
- Battery/power state.
- GPU usage.
- Systemd timers and user units (the original only lists system `.service` units, and skips
  templates, `static` and `masked` units entirely).
- Flatpak packages.
- journald log browsing (the cleaner treats `/var/log` as loose files only).
- An "explain what this will delete" preview before cleaning.
- Any form of error reporting to the user — the original's logger was never even installed.
