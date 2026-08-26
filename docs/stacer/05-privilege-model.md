# 05 — Privilege Model

## 1. Design

Stacer runs entirely as the invoking desktop user. There is **no** helper daemon, **no** setuid
binary, **no** D-Bus service and **no** polkit policy file of its own. Elevation is achieved by
re-running individual commands through `pkexec`:

```cpp
QString CommandUtil::sudoExec(const QString &cmd, QStringList args, QByteArray data)
{
    args.push_front(cmd);
    try { return CommandUtil::exec("pkexec", args, data); }
    catch (QString &ex) { qCritical() << ex; }
    return QString("");
}
```

Consequences of this design:

- **One authentication prompt per operation.** Toggling five services means five polkit
  dialogs. Cleaning files is a single `rm -rf` with many arguments, so that one is a single
  prompt.
- **No privilege caching**, no session, no batching.
- **Cancelling the prompt is silent.** `pkexec` exits non-zero with a message on stderr;
  `CommandUtil` reads only stdout and never checks the exit code, so the caller sees an empty
  string and reports success. The UI then shows stale state (see the "re-read after write"
  pattern below).
- **`pkexec` requires a graphical polkit agent.** Without one (bare WM, TTY, some remote
  sessions) every privileged action fails silently.
- Because `pkexec` is invoked with an argument vector rather than a shell string, there is no
  shell-injection surface — but the *arguments* are still user-controlled data (file paths,
  package names, repository strings) passed to root-run tools.

## 2. Operations requiring root

| Operation | Command | Page |
| --- | --- | --- |
| Enable/disable a unit | `pkexec systemctl enable\|disable <unit>` | Services |
| Start/stop a unit | `pkexec systemctl start\|stop <unit>` | Services |
| Remove distro packages | `pkexec apt-get remove -y …` (or `dnf`/`yum`/`pacman`) | Uninstaller |
| Remove snaps | `pkexec snap remove …` | Uninstaller |
| Add an APT repository | `pkexec add-apt-repository -y <repo> [-s]` | APT Source Manager |
| Edit/enable/disable/remove an APT entry | `pkexec tee <file>` with new content on stdin | APT Source Manager |
| Replace `/etc/hosts` | `pkexec mv /tmp/stacer_etc_host_new_content /etc/hosts` | Helpers → Host Manage |
| Delete system files (logs, crash reports, package cache) | `pkexec rm -rf <paths…>` | System Cleaner |
| Kill another user's process | `pkexec kill <pid>` | Processes |
| Search as root | `pkexec find …` | Search |
| Delete / trash a file owned by another user | `pkexec rm -rf` / `pkexec mv` | Search |

## 3. Operations that stay unprivileged

- All `/proc`, `/sys` and `QStorageInfo` sampling.
- `ps`, `lscpu`, `systemctl list-unit-files|cat|is-active|is-enabled` (querying systemd needs
  no privilege).
- Package **listing** (`dpkg --get-selections`, `rpm -qa`, `pacman -Q`, `snap list`).
- `gsettings get`/`set` — dconf is per-user, so desktop tweaks never elevate.
- `~/.config/autostart` and `~/.cache` manipulation.
- Trash operations on the user's own files.
- Killing one's own processes (`kill <pid>` when the row's user equals `getUserName()`).

The Processes and Search pages both make the privileged/unprivileged decision by comparing the
target's owner to the current username, and only escalate when they differ.

## 4. The "write then re-read" pattern

Because failures are invisible, the UI verifies by re-querying. `ServiceItem` is the clearest
example:

```cpp
tm->changeServiceStatus(name, status);                       // pkexec systemctl enable/disable
ui->checkServiceStartup->setChecked(tm->serviceIsEnabled(name));   // re-read the truth
```

So a cancelled prompt makes the checkbox snap back. Other pages (APT source toggling, cleaner)
do **not** do this and can display state that was never applied.

## 5. Risk notes for the rebuild

1. **`rm -rf` as root with a list built from UI selections.** The System Cleaner passes every
   checked tree item's stored path into one `pkexec rm -rf` call. Paths come from
   `QDir::entryInfoList` so they are real, but there is no confirmation step, no dry run and no
   protection against a user checking the `/var/log` parent-level entries. Cleaning `/var/log`
   files can break running services' logging.
2. **`pkexec tee <path>`** hands a root-writable file handle whatever the app streams. A
   parsing bug in `AptSourceTool::changeSource` (it locates lines by *substring* match) can
   rewrite the wrong line of a sources file.
3. **`/etc/hosts` via `/tmp` staging.** `FileUtil::writeFile("/tmp/stacer_etc_host_new_content")`
   uses a fixed, predictable path in a world-writable directory, then `pkexec mv`s it over
   `/etc/hosts`. On a shared machine this is a symlink/replace race: another user can
   pre-create or swap that path between the write and the `mv`. The rebuild should write to a
   private temp file (`mkstemp` in a user-owned directory) or, better, pipe the content to a
   privileged helper.
4. **stderr and exit codes are discarded everywhere.** Any rewrite should surface both.
5. **Per-action prompting is a UX and security wash.** A single, audited privileged helper
   (polkit policy + D-Bus service, or a short-lived elevated worker with a well-defined
   command allow-list) is both safer and less annoying. If keeping the per-command model,
   at minimum check exit status and report failures.
6. **No input validation on the APT repository string** before handing it to
   `add-apt-repository`; and none on package names before `apt-get remove` (they are read back
   out of `QListWidgetItem::text()`, see [20-known-quirks-and-bugs.md](20-known-quirks-and-bugs.md)).
