// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * The hosts file — `SYS-1`.
 *
 * **Everything already in the file survives.** Comments, blank lines, the distribution's own spacing,
 * and anything nix cannot parse all come back out byte for byte; only lines you actually edit get
 * rewritten. That is checked by a round-trip test against the real `/etc/hosts`, not just intended.
 *
 * **A commented-out entry is shown as a disabled entry**, because that is what writing it that way
 * meant. Toggling it is a checkbox rather than an exercise in remembering where the `#` goes.
 *
 * **Saving is a compare-and-swap.** The file you loaded is sent back with your edits, and the write is
 * refused if the file on disk has changed since — an edit made in a terminal is reported, never
 * overwritten. Stacer's editor rewrote the file from its table model, so anything the table did not
 * represent was simply gone.
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import { t } from "../lib/i18n";
import { BusyInline } from "../components/Busy";
import { api, toAppError, type HostLine, type HostsFile } from "../lib/ipc";
import { notify } from "../lib/notices";

/** A row being edited, or added. `id` is null for a row that is not in the file yet. */
type Draft = { id: number | null; ip: string; names: string; comment: string };

const EMPTY_DRAFT: Draft = { id: null, ip: "", names: "", comment: "" };

function draftOf(line: HostLine): Draft {
  return {
    id: line.id,
    ip: line.ip ?? "",
    names: line.names.join(" "),
    comment: line.comment ?? "",
  };
}

function names(draft: Draft): string[] {
  return draft.names.split(/\s+/).filter((n) => n.length > 0);
}

export default function Hosts() {
  const [file, setFile] = useState<HostsFile | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [busy, setBusy] = useState(false);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setFile(await api.hostsLoad());
      setDraft(null);
      setUnavailable(null);
    } catch (thrown) {
      const error = toAppError(thrown);
      setUnavailable(error.message);
      setFile(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * Send the document back and adopt whatever comes out.
   *
   * The returned file is re-read from disk, so its `original` matches what is there now — which is
   * what lets a second save in the same session have a valid precondition instead of the stale one
   * from before the first.
   */
  const save = useCallback(
    async (next: HostsFile) => {
      setBusy(true);
      try {
        setFile(await api.hostsSave(next));
        setDraft(null);
        notify.success(t("Hosts file saved."));
      } catch (thrown) {
        const error = toAppError(thrown);
        notify.error(error);
        // A refusal means the file moved underneath us, and the only useful next step is to show what
        // is actually there. The user's edit is deliberately not re-applied on top: silently replaying
        // it over someone else's change is the behaviour this whole mechanism exists to prevent.
        if (error.code === "refused") await load();
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  // Editing works on a copy, so a rejected change cannot leave the view showing something the file
  // does not contain.
  const withCopy = useCallback(
    (mutate: (lines: HostLine[]) => void): HostsFile | null => {
      if (file === null) return null;
      const lines = file.lines.map((l) => ({ ...l, names: [...l.names] }));
      mutate(lines);
      return { ...file, lines };
    },
    [file],
  );

  const commitDraft = useCallback(() => {
    if (draft === null) return;
    const parts = names(draft);
    if (draft.ip.trim() === "" || parts.length === 0) {
      notify.warning(t("An entry needs an address and at least one hostname."));
      return;
    }

    const next = withCopy((lines) => {
      const comment = draft.comment.trim() === "" ? null : draft.comment.trim();
      if (draft.id === null) {
        const id = lines.reduce((max, l) => Math.max(max, l.id), -1) + 1;
        lines.push({
          id,
          kind: "entry",
          raw: "",
          ip: draft.ip.trim(),
          names: parts,
          comment,
          enabled: true,
          edited: true,
        });
      } else {
        const line = lines.find((l) => l.id === draft.id);
        if (line) {
          line.ip = draft.ip.trim();
          line.names = parts;
          line.comment = comment;
          line.edited = true;
        }
      }
    });
    if (next) void save(next);
  }, [draft, withCopy, save]);

  const toggle = useCallback(
    (line: HostLine) => {
      const next = withCopy((lines) => {
        const target = lines.find((l) => l.id === line.id);
        if (target) {
          target.enabled = !target.enabled;
          target.edited = true;
        }
      });
      if (next) void save(next);
    },
    [withCopy, save],
  );

  const remove = useCallback(
    (line: HostLine) => {
      const label = line.names.join(" ") || line.raw;
      const next = withCopy((lines) => {
        const at = lines.findIndex((l) => l.id === line.id);
        if (at >= 0) lines.splice(at, 1);
      });
      if (next) {
        notify.info(`Removing ${label}.`);
        void save(next);
      }
    },
    [withCopy, save],
  );

  const entries = useMemo(
    () => (file === null ? [] : file.lines.filter((l) => l.kind === "entry")),
    [file],
  );

  if (unavailable !== null) {
    return (
      <section className="stack">
        <div className="card">
          <h2>{t("The hosts file")}</h2>
          <p className="muted">{unavailable}</p>
        </div>
      </section>
    );
  }

  return (
    <section className="stack stack-wide">
      <div className="card">
        <h2>{t("The hosts file")}</h2>
        <p className="muted">
          Names on this list are resolved here rather than by DNS. {entries.length} entr
          {entries.length === 1 ? "y" : "ies"}, and {file?.lines.length ?? 0} lines in total —
          comments and spacing included, all of which are kept exactly as they are.
        </p>
        <div className="row wrap">
          <button type="button" onClick={() => setDraft({ ...EMPTY_DRAFT })} disabled={busy}>
            {t("Add an entry")}
          </button>
          <button type="button" onClick={() => void load()} disabled={busy}>
            {t("Reload from disk")}
          </button>
          {busy && <BusyInline label={t("Saving to /etc/hosts…")} />}
          <label className="field field-inline">
            <input
              type="checkbox"
              checked={showAll}
              onChange={(e) => setShowAll(e.target.checked)}
            />
            <span>{t("Show comments and blank lines")}</span>
          </label>
        </div>
        <p className="muted">
          {t(
            "Saving asks for administrator rights, and is refused if the file changed since it was loaded — so an edit made in a terminal is reported rather than overwritten.",
          )}
        </p>
      </div>

      {draft !== null && (
        <div className="card card-confirm">
          <h2>{draft.id === null ? "New entry" : "Edit entry"}</h2>
          <div className="hosts-form">
            <label className="field">
              <span>{t("Address")}</span>
              <input
                value={draft.ip}
                placeholder={t("127.0.0.1 or ::1")}
                onChange={(e) => setDraft({ ...draft, ip: e.target.value })}
              />
            </label>
            <label className="field">
              <span>{t("Hostnames")}</span>
              <input
                value={draft.names}
                placeholder={t("db.internal db")}
                onChange={(e) => setDraft({ ...draft, names: e.target.value })}
              />
              <small>{t("Space-separated. The first is the canonical name, the rest are aliases.")}</small>
            </label>
            <label className="field">
              <span>{t("Comment")}</span>
              <input
                value={draft.comment}
                placeholder={t("why this entry exists")}
                onChange={(e) => setDraft({ ...draft, comment: e.target.value })}
              />
            </label>
          </div>
          <div className="row">
            <button type="button" className="danger" onClick={commitDraft} disabled={busy}>
              {busy ? "Saving…" : "Save to /etc/hosts"}
            </button>
            <button type="button" onClick={() => setDraft(null)} disabled={busy}>
              {t("Cancel")}
            </button>
          </div>
        </div>
      )}

      <div className="card">
        <table className="hosts-table">
          <thead>
            <tr>
              <th scope="col" className="hosts-on">
                On
              </th>
              <th scope="col">{t("Address")}</th>
              <th scope="col">{t("Hostnames")}</th>
              <th scope="col" />
            </tr>
          </thead>
          <tbody>
            {(showAll ? (file?.lines ?? []) : entries).map((line) =>
              line.kind === "entry" ? (
                <tr key={line.id} className={line.enabled ? undefined : "is-disabled"}>
                  <td className="hosts-on">
                    <input
                      type="checkbox"
                      checked={line.enabled}
                      disabled={busy}
                      aria-label={`${line.enabled ? "Disable" : "Enable"} ${line.names.join(" ")}`}
                      onChange={() => toggle(line)}
                    />
                  </td>
                  <td className="hosts-ip">{line.ip}</td>
                  <td>
                    <span className="hosts-names">{line.names.join(" ")}</span>
                    {line.comment !== null && (
                      <div className="muted hosts-comment">{line.comment}</div>
                    )}
                  </td>
                  <td className="hosts-actions">
                    <button type="button" disabled={busy} onClick={() => setDraft(draftOf(line))}>
                      {t("Edit")}
                    </button>
                    <button type="button" disabled={busy} onClick={() => remove(line)}>
                      {t("Remove")}
                    </button>
                  </td>
                </tr>
              ) : (
                /* Shown, not editable. A line nix does not understand is still the user's line, and
                   the honest thing is to display it rather than pretend the file is only its table. */
                <tr key={line.id} className="hosts-verbatim">
                  <td />
                  <td colSpan={3}>
                    <code>{line.raw === "" ? " " : line.raw}</code>
                    {line.kind === "unparsed" && (
                      <span className="hosts-tag">{t("nix does not recognise this line")}</span>
                    )}
                  </td>
                </tr>
              ),
            )}
          </tbody>
        </table>
        {entries.length === 0 && !showAll && <p className="muted">{t("No entries.")}</p>}
      </div>
    </section>
  );
}
