// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * What starts at login — `PKG-4`.
 *
 * **The state shown is the real one.** XDG says an entry runs unless something says otherwise, so
 * absence of `Hidden` and `X-GNOME-Autostart-enabled` means enabled. Stacer read that backwards, and
 * since neither key appears in any of the 44 entries on this machine, every entry it listed was shown
 * as disabled while actually running.
 *
 * **Both directories.** Stacer read only the user's, so the 42 entries in `/etc/xdg/autostart` — the
 * ones a distribution ships, and the ones a user most wants to stop — were invisible.
 *
 * **Turning a system entry off needs no password.** XDG already answers it: a file of the same name in
 * the user directory shadows the system one, so nix writes a copy carrying `Hidden=true` and never
 * touches `/etc`.
 *
 * `NoDisplay` is shown as a label rather than used as a filter. 40 of the 42 system entries set it,
 * and hiding them would leave this screen nearly empty while the thing the user came to stop is very
 * likely among them.
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import { api, toAppError, type AutostartEntry } from "../lib/ipc";
import { notify } from "../lib/notices";

export default function Startup() {
  const [entries, setEntries] = useState<AutostartEntry[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState({ name: "", exec: "", comment: "" });
  const [showBackground, setShowBackground] = useState(false);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setEntries(await api.autostartList());
      setUnavailable(null);
    } catch (thrown) {
      setUnavailable(toAppError(thrown).message);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = useCallback(async (entry: AutostartEntry) => {
    setBusy(entry.id);
    try {
      setEntries(await api.autostartSetEnabled(entry.id, !entry.enabled));
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setBusy(null);
    }
  }, []);

  const remove = useCallback(async (entry: AutostartEntry) => {
    setBusy(entry.id);
    try {
      setEntries(await api.autostartRemove(entry.id));
      notify.success(`Removed ${entry.name}.`);
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setBusy(null);
    }
  }, []);

  const add = useCallback(async () => {
    setBusy("__new__");
    try {
      setEntries(
        await api.autostartAdd(draft.name, draft.exec, draft.comment.trim() || undefined),
      );
      setDraft({ name: "", exec: "", comment: "" });
      setAdding(false);
      notify.success("Added to your startup entries.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setBusy(null);
    }
  }, [draft]);

  // Entries that ask not to be shown are still listed, but behind a toggle — they are overwhelmingly
  // desktop plumbing, and leading with them would bury the handful the user recognises.
  const shown = useMemo(
    () => entries.filter((e) => showBackground || !e.no_display),
    [entries, showBackground],
  );
  const background = entries.length - entries.filter((e) => !e.no_display).length;
  const running = entries.filter((e) => e.enabled && e.runs_in_this_session).length;

  if (unavailable !== null) {
    return (
      <section className="stack">
        <div className="card">
          <h2>Startup applications</h2>
          <p className="muted">{unavailable}</p>
        </div>
      </section>
    );
  }

  return (
    <section className="stack stack-wide">
      <div className="card">
        <h2>Startup applications</h2>
        <p className="muted">
          {running} of {entries.length} will start in this session. An entry runs unless something
          turns it off — nix reads that the way the specification defines it.
        </p>
        <div className="row wrap">
          <button type="button" onClick={() => setAdding(!adding)} disabled={busy !== null}>
            {adding ? "Cancel" : "Add an entry"}
          </button>
          <button type="button" onClick={() => void load()} disabled={busy !== null}>
            Reload
          </button>
          <label className="field field-inline">
            <input
              type="checkbox"
              checked={showBackground}
              onChange={(e) => setShowBackground(e.target.checked)}
            />
            <span>Show background services ({background})</span>
          </label>
        </div>
        <p className="muted">
          Turning off an entry the system installed writes an override in your own configuration. The
          system file is never modified, and no password is needed.
        </p>
      </div>

      {adding && (
        <div className="card card-confirm">
          <h2>New startup entry</h2>
          <div className="hosts-form">
            <label className="field">
              <span>Name</span>
              <input
                value={draft.name}
                placeholder="My backup script"
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              />
            </label>
            <label className="field">
              <span>Command</span>
              <input
                value={draft.exec}
                placeholder="/home/you/bin/backup.sh"
                onChange={(e) => setDraft({ ...draft, exec: e.target.value })}
              />
            </label>
            <label className="field">
              <span>Comment</span>
              <input
                value={draft.comment}
                placeholder="what this does"
                onChange={(e) => setDraft({ ...draft, comment: e.target.value })}
              />
            </label>
          </div>
          <button type="button" onClick={() => void add()} disabled={busy !== null}>
            {busy === "__new__" ? "Adding…" : "Add"}
          </button>
        </div>
      )}

      <div className="card">
        <ul className="startup-list">
          {shown.map((entry) => (
            <li
              key={entry.id}
              className={[
                "startup-item",
                entry.enabled ? "" : "is-off",
                entry.runs_in_this_session ? "" : "is-other-session",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              <label className="startup-toggle">
                <input
                  type="checkbox"
                  checked={entry.enabled}
                  disabled={busy !== null}
                  aria-label={`${entry.enabled ? "Disable" : "Enable"} ${entry.name}`}
                  onChange={() => void toggle(entry)}
                />
              </label>

              <div className="startup-body">
                <div className="startup-head">
                  <strong>{entry.name}</strong>
                  {entry.origin === "system" && <span className="startup-tag">system</span>}
                  {entry.shadowed && (
                    <span className="startup-tag" title="You have overridden the system default">
                      overridden
                    </span>
                  )}
                  {entry.no_display && (
                    <span className="startup-tag" title="This entry asks not to be shown in a UI">
                      background
                    </span>
                  )}
                </div>
                {entry.comment !== null && <div className="muted">{entry.comment}</div>}
                <code className="startup-exec">{entry.exec}</code>

                {/* Reasons it will not run despite being enabled. Said plainly, because an entry that
                    looks on and does nothing is the most confusing state this screen can show. */}
                {!entry.runs_in_this_session && (
                  <div className="startup-note">
                    Not for this desktop
                    {entry.only_show_in.length > 0 && ` — only ${entry.only_show_in.join(", ")}`}
                    {entry.not_show_in.length > 0 && ` — excluded from ${entry.not_show_in.join(", ")}`}
                  </div>
                )}
                {entry.try_exec_missing && (
                  <div className="startup-note">
                    Will not run: <code>{entry.try_exec}</code> is not installed
                  </div>
                )}
              </div>

              <div className="startup-actions">
                {entry.origin === "user" ? (
                  <button type="button" disabled={busy !== null} onClick={() => void remove(entry)}>
                    Remove
                  </button>
                ) : (
                  <span className="muted" title="A system entry belongs to a package; turn it off instead">
                    installed by a package
                  </span>
                )}
              </div>
            </li>
          ))}
        </ul>
        {shown.length === 0 && <p className="muted">Nothing to show.</p>}
      </div>
    </section>
  );
}
