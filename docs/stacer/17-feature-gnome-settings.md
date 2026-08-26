# 17 — Feature: GNOME Settings (Unity / Compiz tweaks)

**Files:** `stacer/Pages/GnomeSettings/{gnome_settings_page,unity_settings,window_manager_settings,appearance_settings}.{h,cpp,ui}`
**Backend:** `stacer-core/Tools/gnome_settings_tool.*`, `stacer-core/Tools/gnome_schema.h`
**Window title:** `Gnome Settings` · **Sidebar button:** `btnGnomeSettings`

**Gate:** the page is only created when `$DESKTOP_SESSION` **or**
`QSysInfo::prettyProductName()` matches `ubuntu` case-insensitively. This is the most
era-specific part of Stacer: it targets **Ubuntu with Unity 7 / Compiz** (16.04-era). On
modern GNOME Shell most of these keys do not exist and `gsettings` writes fail silently.

## Structure

`GnomeSettingsPage` holds three sub-pages inside its own `SlidingStackedWidget`, switched by
three buttons:

| Button | Sub-page | Availability |
| --- | --- | --- |
| `btnUnitySettings` | `UnitySettings` | only if `GnomeSettingsTool::checkUnityAvailable()` |
| `btnWindowManager` | `WindowManagerSettings` | always |
| `btnAppearance` | `AppearanceSettings` | always |

If Unity is unavailable the button is hidden and `btnWindowManager` is pre-checked.

Every sub-page follows the same three-phase constructor: `loadDatas()` (fill combo boxes with
label/value pairs), `init()` (read every key via `gsettings get` and set the controls),
`initConnects()` (wire signals — done *after* `init()` so the initial reads don't fire writes).
Widgets using auto-connected slots (`on_checkX_clicked`) are inherently connected, which is why
sliders, spin boxes and combos are connected manually instead: their `valueChanged`/
`currentIndexChanged` signals would fire during `init()`.

Each control writes immediately on change — there is no Apply/Cancel.

## Unity sub-page

Schema constants from `gnome_schema.h`. Relocatable Compiz schemas are addressed as
`schema:path`.

| Control | Schema | Key | Path | Type |
| --- | --- | --- | --- | --- |
| Launcher auto-hide | `org.compiz.unityshell` | `launcher-hide-mode` | `/org/compiz/profiles/unity/plugins/unityshell/` | int (read as bool) |
| Reveal location (Left / Top-Left) | `org.compiz.unityshell` | `reveal-trigger` | Unity | int enum `{Left=0, TopLeft=1}` |
| Reveal sensitivity | `org.compiz.unityshell` | `edge-responsiveness` | Unity | float (slider × 0.1) |
| Minimise single-window apps | `org.compiz.unityshell` | `launcher-minimize-window` | Unity | bool |
| Launcher opacity | `org.compiz.unityshell` | `launcher-opacity` | Unity | float (slider × 0.1) |
| Launcher visibility (all / primary desktop) | `org.compiz.unityshell` | `num-launchers` | Unity | int enum `{AllDesktop=0, PrimaryDesktop=1}` |
| Launcher position (Left / Bottom) | `com.canonical.Unity.Launcher` | `launcher-position` | — | string `"Left"`/`"Bottom"` |
| Icon size | `org.compiz.unityshell` | `icon-size` | Unity | int |
| Dash background blur | `org.compiz.unityshell` | `dash-blur-experimental` | Unity | int (read as bool) |
| Search online sources | `com.canonical.Unity.Lenses` | `remote-content-search` | — | string `"all"`/`"none"` |
| More suggestions | `com.canonical.Unity.ApplicationsLens` | `display-available-apps` | — | bool |
| Recently used | `com.canonical.Unity.ApplicationsLens` | `display-recent-apps` | — | bool |
| Search your files | `com.canonical.Unity.FilesLens` | `use-locate` | — | bool |
| Panel opacity | `org.compiz.unityshell` | `panel-opacity` | Unity | float (slider × 0.1) |
| Show date/time | `com.canonical.indicator.datetime` | `show-clock` | — | bool |
| 24-hour clock | `com.canonical.indicator.datetime` | `time-format` | — | string `"24-hour"`/`"12-hour"` |
| Show seconds / date / weekday / calendar | `com.canonical.indicator.datetime` | `show-seconds`, `show-date`, `show-day`, `show-calendar` | — | bool |
| Show volume | `com.canonical.indicator.sound` | `visible` | — | bool |
| Show my name | `com.canonical.indicator.session` | `show-real-name-on-panel` | — | bool |

Sliders map 0–10 to 0.0–1.0 by multiplying/dividing by `0.1`. Note several boolean-looking keys
are written with `setValueI` (`launcher-hide-mode`, `dash-blur-experimental`) because the Compiz
schema types them as integers.

`checkUnityAvailable()` runs `gsettings list-relocatable-schemas` and returns `false` if any
listed schema is outside Stacer's own eight-schema Unity set — effectively "is this a pristine
Unity desktop". On GNOME Shell it correctly reports unavailable.

## Window Manager sub-page

| Control | Schema | Key | Path | Values |
| --- | --- | --- | --- | --- |
| Texture quality | `org.compiz.opengl` | `texture-filter` | `/org/compiz/profiles/unity/plugins/opengl/` | index 0/1/2 = Fast / Good / Best |
| Workspace switcher on/off | `org.compiz.core` | `hsize` + `vsize` | `/org/compiz/profiles/unity/plugins/core/` | both set to 2 (on) or 1 (off) |
| Horizontal workspaces | `org.compiz.core` | `hsize` | core | int |
| Vertical workspaces | `org.compiz.core` | `vsize` | core | int |
| Raise window on click | `org.gnome.desktop.wm.preferences` | `raise-on-click` | — | bool (written with `setValueI`) |
| Focus mode | `org.gnome.desktop.wm.preferences` | `focus-mode` | — | `click` / `sloppy` / `mouse` |
| Titlebar double-click action | `org.gnome.desktop.wm.preferences` | `action-double-click-titlebar` | — | see action list |
| Titlebar middle-click action | `…` | `action-middle-click-titlebar` | — | same |
| Titlebar right-click action | `…` | `action-right-click-titlebar` | — | same |

Titlebar action values: `toggle-shade`, `toggle-maximize`, `toggle-maximize-horizontally`,
`toggle-maximize-vertically`, `minimize`, `none`, `lower`, `menu`.

"Workspace switcher" is derived, not stored: it is shown as checked when
`hsize > 1 || vsize > 1`. String values read back from `gsettings` are unquoted with
`.replace("'", "")` before `findData()`.

The `org.gnome.desktop.wm.preferences` keys are the only ones here that still exist on modern
GNOME — texture quality and workspace size are Compiz-only.

## Appearance sub-page

| Control | Schema | Key |
| --- | --- | --- |
| Show desktop icons | `org.gnome.desktop.background` | `show-desktop-icons` |
| Home icon | `org.gnome.nautilus.desktop` | `home-icon-visible` |
| Network icon | `org.gnome.nautilus.desktop` | `network-icon-visible` |
| Trash icon | `org.gnome.nautilus.desktop` | `trash-icon-visible` |
| Mounted volumes icon | `org.gnome.nautilus.desktop` | `volumes-visible` |
| Desktop background mode | `org.gnome.desktop.background` | `picture-options` |
| Login background mode | `org.gnome.desktop.screensaver` | `picture-options` |
| On-screen keyboard | `org.gnome.desktop.a11y.applications` | `screen-keyboard-enabled` |
| Screen reader | `org.gnome.desktop.a11y.applications` | `screen-reader-enabled` |

Background modes: `none`, `wallpaper`, `centered`, `scaled`, `stretched`, `zoom`, `spanned`.

Unchecking "Show desktop icons" also unchecks the four icon toggles in the UI **without writing
them** — a cosmetic cascade only.

`org.gnome.nautilus.desktop` was removed in Nautilus 42+; those four toggles are dead on
current systems.

## Port notes

This page is the least portable feature in Stacer. Options for the rebuild:

1. **Drop it** and replace with a small, current "Desktop tweaks" surface (GNOME 4x keys that
   actually exist: `org.gnome.desktop.interface`, `.peripherals`, `.wm.preferences`,
   `.a11y.applications`, `org.gnome.mutter`).
2. **Detect the desktop properly** (`$XDG_CURRENT_DESKTOP`, not `$DESKTOP_SESSION`) and gate
   per-key on schema existence rather than gating the whole page on the distro name.
3. Read/write dconf directly via a Rust binding instead of spawning `gsettings` per key
   (this page issues ~30 subprocesses on load).
4. Whatever is kept, the pattern worth preserving is the declarative schema/key/path constants
   table — port `gnome_schema.h` into a data structure, not into code.
