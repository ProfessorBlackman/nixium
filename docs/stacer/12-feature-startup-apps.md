# 12 — Feature: Startup Apps

**Files:** `stacer/Pages/StartupApps/{startup_apps_page,startup_app,startup_app_edit}.{h,cpp,ui}`
**Window title:** `Startup Apps` · **Sidebar button:** `btnStartupApps`

A CRUD editor for XDG autostart entries in `~/.config/autostart`.

## Target directory resolution

```cpp
mAutostartPath = QStandardPaths::writableLocation(ConfigLocation) + "/autostart";
QFileInfo asfi(mAutostartPath);
if (asfi.isDir()) mAutostartPath.append("/");
else              startups_disabled = checkIfDisabled(mAutostartPath);
```

The `else` branch handles the case where `~/.config/autostart` is a **file** rather than a
directory (a workaround some users apply to disable autostart entirely). `checkIfDisabled()`
returns true if the file contains the literal `X-GNOME-Autostart-enabled=false`. In that case
the page shows `tr("Startup Apps are disabled.")`, disables the Add button, and installs no
watcher.

Otherwise the directory is created if missing, added to a `QFileSystemWatcher`, and
`loadApps()` is connected to `directoryChanged` — so external changes refresh the list live.

## Listing

`loadApps()` iterates `QDir(mAutostartPath, "*.desktop").entryInfoList()`, reads each file into
lines, and uses `Utilities::getDesktopValue` with these regexes (defined in
`startup_app_edit.h`):

```cpp
#define NAME_REG          QRegExp("^Name=.*")
#define COMMENT_REG       QRegExp("^Comment=.*")
#define EXEC_REG          QRegExp("^Exec=.*")
#define GNOME_ENABLED_REG QRegExp("^X-GNOME-Autostart-enabled=.*")
#define HIDDEN_REG        QRegExp("^Hidden=.*")
```

An entry without a `Name=` is skipped. Enabled state:

```cpp
if (Hidden is present)  enabled = (hidden != "true");
else                    enabled = (gnomeEnabled == "true");
```

So `Hidden` takes precedence, and an entry with neither key is treated as **disabled** — which
is wrong per the spec (absence of both means enabled), and means most distro-shipped autostart
files show as off.

Each row becomes a `StartupApp` widget (name label, enable checkbox, edit button, delete
button) inside a `QListWidgetItem`. `deleteAppS` and `editStartupAppS` are connected back to
the page. The title shows `tr("Startup Applications (%1)")` and the not-found placeholder is
toggled on an empty list.

Note `loadApps()` does not read `NoDisplay`, `OnlyShowIn`, `TryExec` or `Icon`; no icon is shown
for entries.

## Toggling an entry

`StartupApp::on_checkStartup_clicked(bool status)` rewrites the file in place:

```cpp
QStringList lines = FileUtil::readListFromFile(mFilePath);
int pos = lines.indexOf(HIDDEN_REG);
QString _status = status ? "true" : "false";
if (pos != -1) {                            // Hidden= exists → invert the meaning
    _status = status ? "false" : "true";
    lines.replace(pos, "Hidden=" + _status);
} else {
    pos = lines.indexOf(GNOME_ENABLED_REG);  // else use X-GNOME-Autostart-enabled=
    if (pos != -1) lines.replace(pos, "X-GNOME-Autostart-enabled=" + _status);
}
if (pos == -1) {                             // neither key → append Hidden=
    _status = status ? "false" : "true";
    lines.append("Hidden=" + _status);
}
FileUtil::writeFile(mFilePath, lines.join('\n') + '\n');
```

The double-negative (`Hidden=false` means enabled) is handled correctly, but the
`X-GNOME-Autostart-enabled` branch writes `_status` **un-inverted** while the `Hidden` branch
inverts — the two keys have opposite polarity and the code relies on that ordering. Writing
happens with a plain truncating write (no atomic replace, no backup).

Deleting is `QFile::remove(mFilePath)` followed by `emit deleteAppS()`; the watcher would also
have triggered a reload.

## Add / edit dialog (`StartupAppEdit`)

A `QDialog` with three fields (`txtStartupAppName`, `txtStartupAppComment`,
`txtStartupAppCommand`), an error label, and Save.

- The subject is passed via the **static** `StartupAppEdit::selectedFilePath` — empty means
  "create new". The dialog is created once and reused.
- On `show()` the fields are cleared and, for an existing file, repopulated from
  `Name=`, `Comment=`, `Exec=`.
- `isValid()` requires **all three** fields non-empty (so a comment is mandatory).
- Editing: `changeDesktopValue()` replaces the matching line or appends it if absent, then
  writes back with `QIODevice::ReadWrite | Truncate` (note: `join("\n")` with no trailing
  newline here, unlike the toggle path).
- Creating: renders the template

  ```ini
  [Desktop Entry]
  Name=<name>
  Comment=<comment>
  Exec=<command>
  Type=Application
  Terminal=false
  Hidden=false
  ```

  and writes it to `<autostart>/<name-lowercased-with-dashes>.desktop`. No collision check —
  saving twice with the same name overwrites.
- `emit startupAppAdded()` then `close()`; the static path is reset to `""` afterwards.
- The dialog re-applies the app stylesheet manually
  (`setStyleSheet(AppManager::ins()->getStylesheetFileContent())`) because it is a top-level
  window, and centres itself on the available desktop geometry.

## Port notes

- Follow the full XDG autostart spec: default-enabled when no key is present, honour
  `NoDisplay`, `OnlyShowIn`/`NotShowIn`, `TryExec`; read `Icon` for display.
- Prefer writing `Hidden=` only (it is the spec key); treat `X-GNOME-Autostart-enabled` as
  read-compat.
- Write atomically (temp file + rename) and preserve unknown keys and comments — the current
  line-replace already preserves unknown keys, which is worth keeping.
- Consider also listing system-wide entries (`/etc/xdg/autostart`) read-only, which Stacer never
  shows.
- Sanitise generated filenames and handle collisions.
