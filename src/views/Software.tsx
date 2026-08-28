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

import { formatBytes, formatCount } from "../lib/format";
import { api, toAppError, type Manager, type Package, type ResidualConfig } from "../lib/ipc";
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

export default function Software() {
  const [packages, setPackages] = useState<Package[]>([]);
  const [residual, setResidual] = useState<ResidualConfig[] | null>(null);
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<SortKey>("size");
  const [explicitOnly, setExplicitOnly] = useState(false);
  const [measuring, setMeasuring] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(null);

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
            <span>Filter</span>
            <input
              type="search"
              value={filter}
              placeholder="name or description"
              onChange={(e) => setFilter(e.target.value)}
            />
          </label>
          <label className="field field-inline">
            <span>Sort by</span>
            <select value={sort} onChange={(e) => setSort(e.target.value as SortKey)}>
              <option value="size">Size</option>
              <option value="name">Name</option>
              <option value="changed">Last updated</option>
            </select>
          </label>
          <label className="field field-inline">
            <input
              type="checkbox"
              checked={explicitOnly}
              onChange={(e) => setExplicitOnly(e.target.checked)}
            />
            <span>Asked for, not pulled in</span>
          </label>
          <button type="button" onClick={() => void refresh()}>
            Refresh
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
          Recorded sizes come from the package manager, which computes them when the package is built.
          Measure a package to see what it occupies on this disk — the two are different figures, and
          the difference is worth seeing.
        </p>
      </div>

      <div className="card">
        <div className="pkg-scroll">
          <table className="pkg-table">
            <thead>
              <tr>
                <th scope="col">Package</th>
                <th scope="col">Version</th>
                <th scope="col" className="pkg-num">
                  Recorded
                </th>
                <th scope="col" className="pkg-num">
                  On disk
                </th>
                <th scope="col">Updated</th>
                <th scope="col" />
              </tr>
            </thead>
            <tbody>
              {shown.slice(0, 500).map((pkg) => (
                <tr
                  key={pkg.id}
                  className={pkg.id === selected ? "is-selected" : undefined}
                  onClick={() => setSelected(pkg.id)}
                >
                  <td>
                    <span className="pkg-id">{pkg.id}</span>
                    {!pkg.explicit && <span className="pkg-dep">dependency</span>}
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
                      onClick={(e) => {
                        e.stopPropagation();
                        void measure(pkg);
                      }}
                    >
                      {measuring === pkg.id ? "Measuring…" : pkg.measured ? "Re-measure" : "Measure"}
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

      {detail !== null && (
        <div className="card">
          <h2 className="pkg-id">{detail.id}</h2>
          <p className="muted">{detail.summary}</p>
          <dl className="pkg-pairs">
            <dt>Version</dt>
            <dd className="pkg-version">{detail.version}</dd>
            <dt>Manager</dt>
            <dd>{detail.manager}</dd>
            <dt>Installed</dt>
            <dd>{detail.explicit ? "you asked for it" : "pulled in as a dependency"}</dd>
            <dt>Last updated</dt>
            <dd>{changedOn(detail.changed_at)}</dd>
            <dt>Recorded size</dt>
            <dd>{formatBytes(detail.recorded_bytes)}</dd>
            {detail.measured ? (
              <>
                <dt>Files contain</dt>
                <dd>
                  {formatBytes(detail.measured.apparent_bytes)} in{" "}
                  {formatCount(detail.measured.files)} files
                </dd>
                <dt>Occupies on disk</dt>
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
                <dt>Against the manager</dt>
                <dd>{discrepancy(detail)}</dd>
                {detail.measured.unreadable > 0 && (
                  <>
                    <dt>Incomplete</dt>
                    <dd>
                      {formatCount(detail.measured.unreadable)} listed paths could not be read, so the
                      measurement is a floor rather than a total.
                    </dd>
                  </>
                )}
              </>
            ) : (
              <>
                <dt>On disk</dt>
                <dd className="muted">not measured yet</dd>
              </>
            )}
          </dl>
        </div>
      )}

      <div className="card">
        <h2>Left-behind configuration</h2>
        <p className="muted">
          Packages removed without purging keep their configuration files. Small individually, and
          genuinely dead weight.
        </p>
        {residual === null ? (
          <button type="button" onClick={() => void loadResidual()}>
            Look for it
          </button>
        ) : residual.length === 0 ? (
          <p className="muted">Nothing left behind.</p>
        ) : (
          <table className="pkg-table">
            <thead>
              <tr>
                <th scope="col">Package</th>
                <th scope="col" className="pkg-num">
                  Configuration left
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
