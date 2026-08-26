# 13 — Feature: Uninstaller

**Files:** `stacer/Pages/Uninstaller/uninstaller_page.{h,cpp}`, `uninstallerpage.ui`
**Window title:** `Uninstaller` · **Sidebar button:** `btnUninstaller`
**Backend:** `stacer-core/Tools/package_tool.*`

## UI

- `btnSystemPackages` / `btnSnapPackages` — tab-style buttons switching a `QStackedWidget`
  (index 0 = distro packages, index 1 = snaps). Their labels carry live counts:
  `tr("Packages (%1)")`, `tr("Snap Packages (%1)")`.
- `listWidgetPackages`, `listWidgetSnapPackages` — checkable `QListWidget`s, one item per
  package, icon from `QIcon::fromTheme(package, ":/static/themes/common/img/package.png")`.
- `txtPackageSearch` — filter for the currently visible list.
- `btnUninstall` — `tr("Uninstall Selected (%1)")`, hidden when both lists are empty.
- `lblLoadingUninstaller` — `loading.gif` spinner shown during load and removal.
- `notFoundWidget`, `notFoundWidget_2` — empty-state placeholders per list.

The snap tab button is only visible when `CommandUtil::isExecutable("snap")`.

## Loading

Both lists are populated from the constructor on worker threads:

```cpp
QtConcurrent::run(this, &UninstallerPage::loadPackages);
QtConcurrent::run(this, &UninstallerPage::loadSnapPackages);
```

Both functions **mutate widgets directly from the worker thread** (same threading violation as
the System Cleaner). `loadPackages()` also emits `uninstallStarted()` at its top, which disables
the lists and shows the spinner — reusing the "busy" state for initial load.

Package sources by distro (`ToolManager::getPackages()` switch on
`PackageTool::currentPackageTool`):

| Detected | List command | Removal command |
| --- | --- | --- |
| `APT` | `bash -c "dpkg --get-selections 2>/dev/null"`, keep lines matching `\s+install$`, take field 0 | `pkexec apt-get remove -y <pkgs>` |
| `DNF` | `bash -c "rpm -qa 2>/dev/null"` (full NVRA strings) | `pkexec dnf remove -y <pkgs>` |
| `YUM` | same as DNF | `pkexec yum remove -y <pkgs>` |
| `PACMAN` | `bash -c "pacman -Q 2>/dev/null"`, take field 0 | `pkexec pacman <pkgs> --noconfirm -R` |
| `ZYPPER` | *(none — returns an empty list)* | *(none)* |
| `UNKNOWN` | empty | no-op |
| snaps | `snap list`, drop the header row, take field 0 | `pkexec snap remove <pkgs>` |

Note the RPM branch lists **full package NVRA strings** (`bash-5.1-2.fc35.x86_64`) rather than
names, so the list is noisier than on APT and the removal argument is version-pinned.

## Selection and removal

Items are checkable; clicking a row only updates the button label:

```cpp
ui->btnUninstall->setText(tr("Uninstall Selected (%1)")
    .arg(getSelectedSnapPackages().count() + getSelectedPackages().count()));
```

`getSelectedPackages()` / `getSelectedSnapPackages()` collect **`item->text().trimmed()`** from
checked rows. Because the display text was created as `QString("  %1").arg(package)` (two
leading spaces, for icon padding), `trimmed()` recovers the original name — a fragile
round-trip through the display string instead of storing the name in item data.

`on_btnUninstall_clicked()`:

```cpp
QtConcurrent::run([=] {
    emit SignalMapper::ins()->sigUninstallStarted();
    ToolManager::ins()->uninstallPackages(selectedPackages);       // pkexec, distro command
    ToolManager::ins()->uninstallSnapPackages(selectedSnapPackages);
    emit SignalMapper::ins()->sigUninstallFinished();
});
```

- Both calls run **unconditionally**, so an empty snap selection still invokes
  `pkexec snap remove` with no package arguments (a second, pointless polkit prompt).
- `sigUninstallStarted` → `uninstallStarted()` disables the lists and search, hides the button,
  shows the spinner.
- `sigUninstallFinished` → both `loadPackages` and `loadSnapPackages`, refreshing from scratch.
- All package removals happen in one command per manager, so one polkit prompt each.
- There is **no confirmation dialog** and no dependency preview: `apt-get remove -y` will pull
  out reverse dependencies without showing the user what else is going.

## Filtering

```cpp
QList<QListWidgetItem*> matches = list->findItems(val, Qt::MatchContains);
for (all items) item->setHidden(true);
for (matches)   item->setHidden(false);
```

Applied to whichever list is on the current stacked index. Case-sensitive substring match
(`MatchContains` without `MatchCaseInsensitive`).

## Port notes

- Query the package database directly (libapt/`dpkg-query -W -f`, `rpm` via librpm or
  `dnf repoquery`, `pacman -Qq`, `snap list --unicode=never`) and keep name/version/summary as
  structured fields rather than one display string.
- Show what a removal would cascade to (`apt-get -s remove`, `dnf remove --assumeno`,
  `pacman -Rp`) and require confirmation.
- Skip empty command invocations; report exit status and stderr.
- Implement zypper, or hide the page when the manager is unsupported instead of showing an empty
  list.
- Consider flatpak, which Stacer never covers.
