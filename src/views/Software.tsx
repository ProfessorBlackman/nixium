// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Installed software — `PKG-1`.
 *
 * **Two sizes, and neither one is a guess.** The manager's own figure sorts the list, because it is
 * free. Measuring one package walks its file list and reports what the filesystem has actually
 * committed, which is the number that matters when the question is "what do I get back?" — and the two
 * are never conflated, because they are not the same metric. dpkg's `Installed-Size` is computed at
 * build time with per-file rounding; a theme package here contains 76.1 MB, records 96.3 MB, and
 * occupies **181.3 MB**.
 *
 * **Packages are identified the way the manager identifies them.** `libc6:amd64` and `libc6:i386` are
 * two installed packages of different sizes, and 41 names on this machine are installed for both
 * architectures. Stacer round-tripped names through a padded UI label, which is how a display string
 * ends up deciding what gets removed.
 *
 * **Every manager on the machine, not the first one found.**
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import { t } from "../lib/i18n";
import { Spinner } from "../components/Busy";
import { formatBytes, formatCount } from "../lib/format";
import {
  api,
  toAppError,
  type Concern,
  type Manager,
  type Package,
  type RemovalOutcome,
  type RemovalPreview,
  type ResidualConfig,
} from "../lib/ipc";
import { notify } from "../lib/notices";

type SortKey = "size" | "name" | "changed";

/** Absolute date from a unix second count. Absolute, not relative: an install date is a fact. */
function changedOn(seconds: number | null): string {
  if (seconds === null) return "—";
  return new Date(seconds * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/**
 * How far the manager's own figure is from the space actually committed.
 *
 * Signed and labelled in both directions. An earlier design subtracted with a floor at zero and called
 * the result "growth", which reported four packages in five as unchanged — dpkg's estimate is usually
 * higher than the file contents and lower than the disk occupancy, and a figure that cannot go
 * negative cannot say so.
 */
function discrepancy(pkg: Package): string | null {
  if (!pkg.measured) return null;
  const delta = pkg.measured.disk_bytes - pkg.recorded_bytes;
  if (delta === 0) return "matches the manager's figure";
  const sign = delta > 0 ? "more" : "less";
  return `${formatBytes(Math.abs(delta))} ${sign} on disk than recorded`;
}

/**
 * The same wording as `Concern::explanation` in Rust.
 *
 * Duplicated rather than sent over the wire because it is display copy, and a `Concern` is a closed
 * enum — a variant added in Rust breaks this switch at compile time, which is the property that makes
 * the duplication safe.
 */
const CONCERN_TEXT: Record<Concern, string> = {
  cascade: "would be removed as well, because something being removed needs it",
  important: "is part of the base system",
  display_manager:
    "runs the graphical login on this machine — removing it boots to a text console",
  running_kernel: "is part of the kernel this machine is running right now",
  required: "is required for the system to function",
  essential: "is marked essential; removing it breaks the system",
};

/**
 * The twin of `RemovalOutcome::matched_preview`.
 *
 * Rust methods do not cross into TypeScript, and the two fields it reads are both here, so this is a
 * one-line derivation rather than a value to serialise.
 */
function matchedPreview(outcome: RemovalOutcome): boolean {
  return outcome.remaining.length === 0 && outcome.unexpected.length === 0;
}

/** Concerns worth showing individually. A cascade is shown as a count, not one line per package. */
function notableConcerns(preview: RemovalPreview) {
  return preview.flagged.filter((f) => f.concern !== "cascade");
}

export default function Software() {
  const [packages, setPackages] = useState<Package[]>([]);
  const [residual, setResidual] = useState<ResidualConfig[] | null>(null);
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<SortKey>("size");
  const [explicitOnly, setExplicitOnly] = useState(false);
  const [measuring, setMeasuring] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(null);
  const [chosen, setChosen] = useState<string[]>([]);
  const [preview, setPreview] = useState<RemovalPreview | null>(null);
  const [outcome, setOutcome] = useState<RemovalOutcome | null>(null);
  const [removing, setRemoving] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setPackages(await api.packagesList());
      setUnavailable(null);
    } catch (thrown) {
      const error = toAppError(thrown);
      setUnavailable(error.message);
      setPackages([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const measure = useCallback(
    async (pkg: Package) => {
      setMeasuring(pkg.id);
      try {
        const measured = await api.packageMeasure(pkg.manager as Manager, pkg.id, pkg.version);
        // Replace in place rather than refetching: the inventory has not changed, only this row's
        // knowledge of itself.
        setPackages((current) =>
          current.map((p) => (p.id === pkg.id ? { ...p, measured } : p)),
        );
      } catch (thrown) {
        notify.error(toAppError(thrown));
      } finally {
        setMeasuring(null);
      }
    },
    [],
  );

  const loadResidual = useCallback(async () => {
    try {
      setResidual(await api.packagesResidual());
    } catch (thrown) {
      notify.error(toAppError(thrown));
      setResidual([]);
    }
  }, []);

  const toggle = useCallback((id: string) => {
    // Changing the selection invalidates the preview. It is cleared rather than left on screen,
    // because a preview that no longer describes the selection is worse than none.
    setPreview(null);
    setOutcome(null);
    setChosen((current) =>
      current.includes(id) ? current.filter((c) => c !== id) : [...current, id],
    );
  }, []);

  const askWhatWouldHappen = useCallback(async () => {
    if (chosen.length === 0) return;
    try {
      setOutcome(null);
      setPreview(await api.packagesRemovalPreview(chosen));
    } catch (thrown) {
      notify.error(toAppError(thrown));
      setPreview(null);
    }
  }, [chosen]);

  const remove = useCallback(async () => {
    if (preview === null || preview.risk === "refused") return;
    setRemoving(true);
    try {
      const result = await api.packagesRemove(chosen);
      setOutcome(result);
      setPreview(null);
      setChosen([]);
      if (!matchedPreview(result)) {
        // Said loudly rather than left in a panel the user might scroll past: the operation
        // diverged from what they approved.
        notify.warning(
          "The removal did not match what was previewed.",
          "Check the summary below before continuing.",
        );
      }
      await refresh();
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setRemoving(false);
    }
  }, [preview, chosen, refresh]);

  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const rows = packages.filter((p) => {
      if (explicitOnly && !p.explicit) return false;
      if (needle === "") return true;
      return p.id.toLowerCase().includes(needle) || p.summary.toLowerCase().includes(needle);
    });

    const sorted = [...rows];
    if (sort === "name") {
      sorted.sort((a, b) => a.id.localeCompare(b.id));
    } else if (sort === "changed") {
      // Unknown dates last, rather than sorting as the epoch.
      sorted.sort((a, b) => (b.changed_at ?? -1) - (a.changed_at ?? -1));
    } else {
      sorted.sort(
        (a, b) =>
          (b.measured?.disk_bytes ?? b.recorded_bytes) - (a.measured?.disk_bytes ?? a.recorded_bytes),
      );
    }
    return sorted;
  }, [packages, filter, sort, explicitOnly]);

  const totals = useMemo(() => {
    const recorded = packages.reduce((sum, p) => sum + p.recorded_bytes, 0);
    const measured = packages.filter((p) => p.measured !== null);
    const disk = measured.reduce((sum, p) => sum + (p.measured?.disk_bytes ?? 0), 0);
    return { recorded, measuredCount: measured.length, disk };
  }, [packages]);

  const detail = useMemo(
    () => (selected === null ? null : (shown.find((p) => p.id === selected) ?? null)),
    [selected, shown],
  );

  return (
    <section className="view">
      <div className="card">
        <div className="row">
          <label className="field field-inline">
            <span>{t("Filter")}</span>
            <input
              type="search"
              value={filter}
              placeholder={t("name or description")}
              onChange={(e) => setFilter(e.target.value)}
            />
          </label>
          <label className="field field-inline">
            <span>{t("Sort by")}</span>
            <select value={sort} onChange={(e) => setSort(e.target.value as SortKey)}>
              <option value="size">{t("Size")}</option>
              <option value="name">{t("Name")}</option>
              <option value="changed">{t("Last updated")}</option>
            </select>
          </label>
          <label className="field field-inline">
            <input
              type="checkbox"
              checked={explicitOnly}
              onChange={(e) => setExplicitOnly(e.target.checked)}
            />
            <span>{t("Asked for, not pulled in")}</span>
          </label>
          <button type="button" onClick={() => void refresh()}>
            {t("Refresh")}
          </button>
        </div>

        {unavailable === null ? (
          <p className="muted">
            {formatCount(shown.length)} of {formatCount(packages.length)} packages ·{" "}
            {formatBytes(totals.recorded)} recorded
            {totals.measuredCount > 0
              ? ` · ${formatCount(totals.measuredCount)} measured, ${formatBytes(totals.disk)} on disk`
              : ""}
          </p>
        ) : (
          <p className="muted">{unavailable}</p>
        )}
        <p className="muted">
          {t(
            "Recorded sizes come from the package manager, which computes them when the package is built. Measure a package to see what it occupies on this disk — the two are different figures, and the difference is worth seeing.",
          )}
        </p>
      </div>

      <div className="card">
        <div className="pkg-scroll">
          <table className="pkg-table">
            <thead>
              <tr>
                <th scope="col" className="pkg-pick">
                  <span className="visually-hidden">{t("Select")}</span>
                </th>
                <th scope="col">{t("Package")}</th>
                <th scope="col">{t("Version")}</th>
                <th scope="col" className="pkg-num">
                  {t("Recorded")}
                </th>
                <th scope="col" className="pkg-num">
                  {t("On disk")}
                </th>
                <th scope="col">{t("Updated")}</th>
                <th scope="col" />
              </tr>
            </thead>
            <tbody>
              {/* No `onClick` on the row itself. A clickable `<tr>` has no keyboard path and no role
                  a screen reader can act on, so selection is the button on the name — reachable by
                  Tab, announced as a button, and doing the same thing. */}
              {shown.slice(0, 500).map((pkg) => (
                <tr key={pkg.id} className={pkg.id === selected ? "is-selected" : undefined}>
                  <td className="pkg-pick">
                    <input
                      type="checkbox"
                      checked={chosen.includes(pkg.id)}
                      aria-label={`Select ${pkg.id} for removal`}
                      onChange={() => toggle(pkg.id)}
                    />
                  </td>
                  <td>
                    <button
                      type="button"
                      className="pkg-select pkg-id"
                      aria-pressed={pkg.id === selected}
                      onClick={() => setSelected(pkg.id)}
                    >
                      {pkg.id}
                    </button>
                    {!pkg.explicit && <span className="pkg-dep">{t("dependency")}</span>}
                    <div className="pkg-summary muted">{pkg.summary}</div>
                  </td>
                  <td className="pkg-version">{pkg.version}</td>
                  <td className="pkg-num">{formatBytes(pkg.recorded_bytes)}</td>
                  <td className="pkg-num">
                    {pkg.measured ? (
                      <span title={discrepancy(pkg) ?? undefined}>
                        {formatBytes(pkg.measured.disk_bytes)}
                      </span>
                    ) : (
                      <span className="muted">—</span>
                    )}
                  </td>
                  <td className="pkg-date">{changedOn(pkg.changed_at)}</td>
                  <td>
                    <button
                      type="button"
                      disabled={measuring !== null}
                      onClick={() => void measure(pkg)}
                    >
                      {measuring === pkg.id && <Spinner />}
                      {measuring === pkg.id
                        ? t("Measuring…")
                        : pkg.measured
                          ? t("Re-measure")
                          : t("Measure")}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {shown.length > 500 && (
          <p className="muted">
            Showing the first 500 of {formatCount(shown.length)}. Narrow the filter to see the rest.
          </p>
        )}
      </div>

      {chosen.length > 0 && (
        <div className="card">
          <h2>Remove {chosen.length === 1 ? "1 package" : `${chosen.length} packages`}</h2>
          <p className="muted">
            {chosen.map((id) => (
              <span key={id} className="pkg-id pkg-chip">
                {id}
              </span>
            ))}
          </p>

          <div className="row">
            <button type="button" onClick={() => void askWhatWouldHappen()}>
              {t("What would this do?")}
            </button>
            <button
              type="button"
              onClick={() => {
                setChosen([]);
                setPreview(null);
              }}
            >
              {t("Clear")}
            </button>
          </div>

          {preview !== null && (
            <div className={`pkg-verdict pkg-verdict-${preview.risk}`}>
              {preview.risk === "refused" ? (
                <>
                  <h3>{t("nix will not do this")}</h3>
                  <ul>
                    {notableConcerns(preview)
                      .filter((f) => f.concern !== "important")
                      .map((f) => (
                        <li key={`${f.package}-${f.concern}`}>
                          <span className="pkg-id">{f.package}</span> {t(CONCERN_TEXT[f.concern])}
                        </li>
                      ))}
                  </ul>
                  <p className="muted">
                    {t(
                      "This is a refusal, not a warning — nothing in this window can approve it. The helper that would carry it out decides for itself and refuses as well. If you are certain, run it yourself:",
                    )}
                  </p>
                  <p className="pkg-id pkg-command">
                    sudo apt-get remove {preview.requested.join(" ")}
                  </p>
                </>
              ) : (
                <>
                  <h3>
                    {preview.removing.length === 1
                      ? "1 package would be removed"
                      : `${preview.removing.length} packages would be removed`}
                    {preview.freed_bytes > 0 && ` · ${formatBytes(preview.freed_bytes)} freed`}
                  </h3>

                  {notableConcerns(preview).length > 0 && (
                    <ul className="pkg-concerns">
                      {notableConcerns(preview).map((f) => (
                        <li key={`${f.package}-${f.concern}`}>
                          <span className="pkg-id">{f.package}</span> {t(CONCERN_TEXT[f.concern])}
                        </li>
                      ))}
                    </ul>
                  )}

                  {preview.removing.length > preview.requested.length && (
                    <p className="muted">
                      Also going:{" "}
                      {preview.removing
                        .filter((name) => !preview.requested.includes(name))
                        .join(", ")}
                    </p>
                  )}

                  {preview.installing.length > 0 && (
                    <p className="muted">
                      The manager would also install or upgrade: {preview.installing.join(", ")}.
                      That is unusual for a removal and worth reading twice.
                    </p>
                  )}

                  <p className="muted">
                    {t("The figure above is what the package manager expects to free, not a measurement.")}
                  </p>

                  <button
                    type="button"
                    className={preview.risk === "dangerous" ? "danger" : undefined}
                    disabled={removing}
                    onClick={() => void remove()}
                  >
                    {removing
                      ? "Removing…"
                      : preview.risk === "dangerous"
                        ? "Remove anyway"
                        : "Remove"}
                  </button>
                </>
              )}
            </div>
          )}
        </div>
      )}

      {outcome !== null && (
        <div className="card">
          <h2>{t("What actually happened")}</h2>
          {matchedPreview(outcome) ? (
            <p>
              {outcome.removed.length === 1
                ? "1 package removed"
                : `${outcome.removed.length} packages removed`}
              , exactly as previewed. The manager expected to free{" "}
              {formatBytes(outcome.expected_freed_bytes)}.
            </p>
          ) : (
            <>
              <p>
                {t(
                  "This did not match the preview. Checked against the package database afterwards rather than taken from the manager's exit status.",
                )}
              </p>
              {outcome.remaining.length > 0 && (
                <p>
                  <strong>{t("Still installed:")}</strong> {outcome.remaining.join(", ")}
                </p>
              )}
              {outcome.unexpected.length > 0 && (
                <p>
                  <strong>{t("Removed without being previewed:")}</strong> {outcome.unexpected.join(", ")}
                </p>
              )}
            </>
          )}
          {outcome.removed.length > 0 && (
            <p className="muted">Removed: {outcome.removed.join(", ")}</p>
          )}
        </div>
      )}

      {detail !== null && (
        <div className="card">
          <h2 className="pkg-id">{detail.id}</h2>
          <p className="muted">{detail.summary}</p>
          <dl className="pkg-pairs">
            <dt>{t("Version")}</dt>
            <dd className="pkg-version">{detail.version}</dd>
            <dt>{t("Manager")}</dt>
            <dd>{detail.manager}</dd>
            <dt>{t("Installed")}</dt>
            <dd>{detail.explicit ? "you asked for it" : "pulled in as a dependency"}</dd>
            <dt>{t("Last updated")}</dt>
            <dd>{changedOn(detail.changed_at)}</dd>
            <dt>{t("Recorded size")}</dt>
            <dd>{formatBytes(detail.recorded_bytes)}</dd>
            {detail.measured ? (
              <>
                <dt>{t("Files contain")}</dt>
                <dd>
                  {formatBytes(detail.measured.apparent_bytes)} in{" "}
                  {formatCount(detail.measured.files)} files
                </dd>
                <dt>{t("Occupies on disk")}</dt>
                <dd>
                  {formatBytes(detail.measured.disk_bytes)}
                  {detail.measured.disk_bytes > detail.measured.apparent_bytes && (
                    <span className="muted">
                      {" "}
                      · {formatBytes(detail.measured.disk_bytes - detail.measured.apparent_bytes)} of
                      that is block allocation, not content
                    </span>
                  )}
                </dd>
                <dt>{t("Against the manager")}</dt>
                <dd>{discrepancy(detail)}</dd>
                {detail.measured.unreadable > 0 && (
                  <>
                    <dt>{t("Incomplete")}</dt>
                    <dd>
                      {formatCount(detail.measured.unreadable)} listed paths could not be read, so the
                      measurement is a floor rather than a total.
                    </dd>
                  </>
                )}
              </>
            ) : (
              <>
                <dt>{t("On disk")}</dt>
                <dd className="muted">{t("not measured yet")}</dd>
              </>
            )}
          </dl>
        </div>
      )}

      <div className="card">
        <h2>{t("Left-behind configuration")}</h2>
        <p className="muted">
          {t(
            "Packages removed without purging keep their configuration files. Small individually, and genuinely dead weight.",
          )}
        </p>
        {residual === null ? (
          <button type="button" onClick={() => void loadResidual()}>
            {t("Look for it")}
          </button>
        ) : residual.length === 0 ? (
          <p className="muted">{t("Nothing left behind.")}</p>
        ) : (
          <table className="pkg-table">
            <thead>
              <tr>
                <th scope="col">{t("Package")}</th>
                <th scope="col" className="pkg-num">
                  {t("Configuration left")}
                </th>
              </tr>
            </thead>
            <tbody>
              {residual.map((r) => (
                <tr key={r.name}>
                  <td className="pkg-id">{r.name}</td>
                  <td className="pkg-num">{formatBytes(r.bytes)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}
