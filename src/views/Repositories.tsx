// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Where software comes from — `PKG-5`.
 *
 * **Both of apt's formats.** Stacer enumerated with `*.list`, so deb822 `.sources` files matched no
 * glob it looked at — two repositories on this machine, invisible, keyrings included.
 *
 * **Only the files apt reads.** `/etc/apt/sources.list.d` holds 53 files here and apt reads 18; the
 * other 35 are `.save` and `.distUpgrade` copies from release upgrades. Listing them would show
 * configuration that affects nothing, some of it contradicting the live entries.
 *
 * **Entries are addressed by file and position.** Stacer found the line to edit with a substring
 * search from the top of the file, taking the first hit, against an entry that recorded no position.
 *
 * Adding a repository is deliberately not offered: a repository without its signing key is useless,
 * and fetching keys on a user's behalf is not something a storage tool should be doing.
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import { api, toAppError, type Repository, type SourceLocation } from "../lib/ipc";
import { notify } from "../lib/notices";

/** Group by file, so the list reads the way the configuration is actually laid out. */
function byFile(repos: Repository[]): [string, Repository[]][] {
  const groups = new Map<string, Repository[]>();
  for (const repo of repos) {
    const key = repo.at.file;
    const existing = groups.get(key);
    if (existing) existing.push(repo);
    else groups.set(key, [repo]);
  }
  return [...groups.entries()];
}

function shortName(path: string): string {
  return path.split("/").pop() ?? path;
}

export default function Repositories() {
  const [repos, setRepos] = useState<Repository[]>([]);
  const [busy, setBusy] = useState(false);
  const [filter, setFilter] = useState("");
  const [onlyEnabled, setOnlyEnabled] = useState(false);

  const load = useCallback(async () => {
    try {
      setRepos(await api.aptSourcesList());
    } catch (thrown) {
      notify.error(toAppError(thrown));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const act = useCallback(
    async (what: "toggle" | "remove", repo: Repository) => {
      setBusy(true);
      try {
        const at: SourceLocation = repo.at;
        setRepos(
          what === "toggle"
            ? await api.aptSourceSetEnabled(at, !repo.enabled)
            : await api.aptSourceRemove(at),
        );
      } catch (thrown) {
        const error = toAppError(thrown);
        notify.error(error);
        // Refused means the file moved underneath us; the only useful next step is to show what is
        // actually there rather than replay the edit over someone else's change.
        if (error.code === "refused") await load();
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return repos.filter((r) => {
      if (onlyEnabled && !r.enabled) return false;
      if (needle === "") return true;
      return (
        r.uris.some((u) => u.toLowerCase().includes(needle)) ||
        r.suites.some((s) => s.toLowerCase().includes(needle)) ||
        (r.label?.toLowerCase().includes(needle) ?? false) ||
        r.at.file.toLowerCase().includes(needle)
      );
    });
  }, [repos, filter, onlyEnabled]);

  const enabled = repos.filter((r) => r.enabled).length;
  const deb822 = repos.filter((r) => r.at.format === "deb822").length;
  const files = new Set(repos.map((r) => r.at.file)).size;

  return (
    <section className="stack stack-wide">
      <div className="card">
        <h2>Software sources</h2>
        <p className="muted">
          {enabled} of {repos.length} entries active, across {files} files
          {deb822 > 0 && ` · ${deb822} in the deb822 format`}. Only files apt actually reads are
          listed — `.save` and `.distUpgrade` leftovers are ignored, as apt ignores them.
        </p>
        <div className="row wrap">
          <label className="field field-inline">
            <span>Filter</span>
            <input
              type="search"
              value={filter}
              placeholder="URI, suite or file"
              onChange={(e) => setFilter(e.target.value)}
            />
          </label>
          <label className="field field-inline">
            <input
              type="checkbox"
              checked={onlyEnabled}
              onChange={(e) => setOnlyEnabled(e.target.checked)}
            />
            <span>Active only</span>
          </label>
          <button type="button" onClick={() => void load()} disabled={busy}>
            Reload
          </button>
        </div>
        <p className="muted">
          Changing an entry asks for administrator rights, and is refused if the file changed since it
          was read.
        </p>
      </div>

      {byFile(shown).map(([file, entries]) => (
        <div className="card" key={file}>
          <h2 className="repo-file">{shortName(file)}</h2>
          <p className="muted repo-path">{file}</p>
          <ul className="repo-list">
            {entries.map((repo) => (
              <li
                key={`${repo.at.file}:${repo.at.index}`}
                className={`repo-item${repo.enabled ? "" : " is-off"}`}
              >
                <label className="repo-toggle">
                  <input
                    type="checkbox"
                    checked={repo.enabled}
                    disabled={busy}
                    aria-label={`${repo.enabled ? "Disable" : "Enable"} ${repo.uris.join(" ")}`}
                    onChange={() => void act("toggle", repo)}
                  />
                </label>

                <div className="repo-body">
                  <div className="repo-head">
                    {repo.label !== null && <strong>{repo.label}</strong>}
                    <span className="repo-uri">{repo.uris.join(" ")}</span>
                    {repo.types.map((t) => (
                      <span className="repo-tag" key={t}>
                        {t}
                      </span>
                    ))}
                    {repo.at.format === "deb822" && <span className="repo-tag">deb822</span>}
                  </div>
                  <div className="muted">
                    {repo.suites.join(" ")}
                    {repo.components.length > 0 && ` — ${repo.components.join(" ")}`}
                    {repo.architectures.length > 0 && ` · ${repo.architectures.join(", ")}`}
                  </div>
                  {/* The keyring is first-class: it is the difference between a repository apt
                      trusts and one it refuses, and the field most likely to be wrong after a
                      manual edit. */}
                  {repo.signed_by !== null ? (
                    <code className="repo-key">signed by {repo.signed_by}</code>
                  ) : (
                    <span className="repo-nokey">no keyring named in this entry</span>
                  )}
                  {repo.other_options.length > 0 && (
                    <code className="repo-key">{repo.other_options.join(" ")}</code>
                  )}
                </div>

                <div className="repo-actions">
                  <span className="muted repo-where">
                    {repo.at.format === "deb822" ? "stanza" : "line"} {repo.at.index + 1}
                  </span>
                  <button type="button" disabled={busy} onClick={() => void act("remove", repo)}>
                    Remove
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </div>
      ))}

      {shown.length === 0 && (
        <div className="card">
          <p className="muted">Nothing matches.</p>
        </div>
      )}
    </section>
  );
}
