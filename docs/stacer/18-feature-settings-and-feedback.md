# 18 — Feature: Settings and Feedback

## Settings page

**Files:** `stacer/Pages/Settings/settings_page.{h,cpp,ui}`
**Window title:** `Settings` · **Sidebar button:** `btnSettings`

Every control writes straight to `SettingManager` (i.e. `~/.config/stacer/settings.ini`) on
change; there is no Apply button.

| Control | Setting | Effect |
| --- | --- | --- |
| `cmbLanguages` | `Language` | Combo populated from `:/static/languages.json` (`text` shown, `value` stored). **Takes effect on next launch** — no live retranslation. |
| `cmbThemes` | `ThemeName` | **Entirely commented out** in 1.1.0. |
| `cmbDisks` | `DiskName` | Items are `"<device>  (<displayName>)"` with `displayName` as data; selects which volume the Dashboard disk gauge tracks. Defaults to `QStorageInfo::root().displayName()`. |
| `cmbStartPage` | `StartPage` | Fixed list of nine translated page names: Dashboard, Startup Apps, System Cleaner, Search, Services, Processes, Helpers, Uninstaller, Resources. Note it omits APT Source Manager, GNOME Settings and Settings itself. |
| `checkAutostart` | *(no INI key)* | Writes/removes `~/.config/autostart/stacer.desktop`. |
| `spinCpuPercent` | `CPUAlertPercent` | Dashboard tray-alert threshold; 0 disables. |
| `spinMemoryPercent` | `MemoryAlertPercent` | as above |
| `spinDiskPercent` | `DiskAlertPercent` | as above |
| `checkAppQuitDontAsk` | `AppQuitDialogDontAsk` | Suppress the close-behaviour prompt. |
| `btnDonate` | — | Opens `https://www.patreon.com/oguzhaninan`. |

### Autostart-self

```ini
[Desktop Entry]
Name=Stacer
Comment=Linux System Optimizer and Monitoring
Exec=stacer --hide
Type=Application
Terminal=false
Hidden=false
```

Written to `~/.config/autostart/stacer.desktop` (the directory is created if needed); unchecking
removes the file. The initial checkbox state is derived by reading the file and testing
`Hidden=` for `"false"` — consistent with the Startup Apps page's convention, and note that
`--hide` makes Stacer start into the tray.

Because this entry is in the same directory the Startup Apps page manages, Stacer appears in
its own startup list and can be toggled from either place.

### Notes for the rebuild

- Storing `StartPage` as a **translated** string is a bug; store a stable page id.
- Language change should retranslate live (or at least tell the user a restart is needed).
- Theme selection needs restoring — the `light` theme assets and `values.ini` already exist.
- The start-page list should be generated from the actual page registry, not hard-coded.

---

## Feedback dialog

**Files:** `stacer/feedback.{h,cpp,ui}` · opened from the sidebar's `btnFeedback`.

A `QDialog` with `txtName`, `txtEmail`, `txtMessage`, an error label, Send and Close.
Created lazily on first click and reused (`QSharedPointer<Feedback>` member of `App`).

Validation:

- Email must match `\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,4}\b` (case-insensitive,
  `exactMatch`) — rejects TLDs longer than four characters.
- Message must be ≥ 5 characters.
- Name and email must be non-empty.

Submission runs on a `QtConcurrent` thread and **posts via a `curl` subprocess** rather than
`QNetworkAccessManager`:

```cpp
args << "-d" << json.toJson()
     << "-H" << "Content-Type: application/json"
     << "-X" << "POST" << "https://stacer-web-api.herokuapp.com/feedback";
QString result = CommandUtil::exec("curl", args);
// success when the parsed JSON has  "success": true
```

The payload is `{name, email, message}`. This is why `curl` is a documented runtime dependency.

Thread safety here is handled properly-ish: the worker communicates through the dialog's own
signals (`setErrorMessageS`, `clearInputsS`, `disableElementsS`) which are connected to slots on
the same object, so they queue onto the GUI thread — **except** for two direct
`ui->btnSend->setText(...)` calls in the worker body, which are cross-thread widget writes.

Messages shown: `tr("Email address is not valid !")`,
`tr("Your message must be at least 5 characters !")`, `tr("Fields cannot be left blank !")`,
`tr("Something went wrong, try again !")`, and on success a green
`tr("Your Feedback has been successfully sended.")`.

### Notes for the rebuild

The Heroku endpoint is gone (free dynos retired) and the upstream project is abandoned, so this
feature has no working backend. Either drop it or point it at something real (a GitHub issue
template opened in the browser is the obvious low-maintenance replacement). If any HTTP is
needed, use a real HTTP client, not a `curl` subprocess.
