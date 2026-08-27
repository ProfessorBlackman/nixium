// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Trends — `STO-16`.
 *
 * Answers "what grew?" rather than "what is big?", which the explorer already covers. Collection is
 * **opt-in and off by default**: a storage tool that starts scanning on a schedule without being
 * asked has made a decision it had no standing to make.
 *
 * Two rules the display obeys:
 *
 * - **Gaps are gaps.** A missing sample is drawn as a break in the line, never bridged. The machine
 *   was off, or on battery, or nix was not running — and a plausible number in that space would turn
 *   "we do not know" into something a user might act on (§P8).
 * - **A trend needs two points.** With one sample there is nothing to say, and the view says that
 *   rather than showing a percentage of nothing.
 */
import { useCallback, useEffect, useState } from "react";

import { formatBytes } from "../lib/format";
import {
  api,
  toAppError,
  type GrowthReport,
  type Sample,
  type Series,
  type TimerState,
} from "../lib/ipc";
import { notify } from "../lib/notices";

const DAY = 86_400;

/** A signed byte figure, with its direction said in words as well as its sign. */
function Delta({ delta }: { delta: number }) {
  if (delta === 0) return <span className="muted">unchanged</span>;
  const grew = delta > 0;
  return (
    <span className={grew ? "delta-grew" : "delta-shrank"}>
      {grew ? "+" : "−"}
      {formatBytes(Math.abs(delta))}
    </span>
  );
}

/**
 * The series as a bar per interval, with gaps left empty.
 *
 * Deliberately not a smoothed line: a line has to decide what happens between points, and the honest
 * answer here is "nothing is known". Bars with holes in them cannot imply otherwise.
 */
function Bars({ series }: { series: Series }) {
  const values = series.points.map((p) => p?.total_allocated ?? 0);
  const peak = Math.max(...values, 1);
  return (
    <div className="trend-bars" role="img" aria-label="Total storage over time, with gaps where no sample was taken">
      {series.points.map((point, i) => (
        <div
          key={i}
          className={point ? "trend-bar" : "trend-bar is-gap"}
          style={point ? { height: `${Math.max(2, (point.total_allocated / peak) * 100)}%` } : undefined}
          title={
            point
              ? `${new Date(point.at * 1000).toLocaleDateString()}: ${formatBytes(point.total_allocated)}`
              : "No sample — nix was not running, or the machine was off or on battery"
          }
        />
      ))}
    </div>
  );
}

export default function Trends() {
  const [samples, setSamples] = useState<Sample[] | null>(null);
  const [series, setSeries] = useState<Series | null>(null);
  const [growth, setGrowth] = useState<GrowthReport | null>(null);
  const [timer, setTimer] = useState<TimerState | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [s, ser, g, t] = await Promise.all([
        api.historySamples(),
        api.historySeries(DAY),
        api.historyGrowth(Math.floor(Date.now() / 1000) - 7 * DAY, 10),
        api.timerState(),
      ]);
      setSamples(s);
      setSeries(ser);
      setGrowth(g);
      setTimer(t);
    } catch (thrown) {
      notify.error(toAppError(thrown));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const enable = useCallback(async () => {
    setBusy(true);
    try {
      setTimer(await api.timerInstall());
      notify.success("Daily collection enabled.", "It runs at idle priority and never on battery.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setBusy(false);
    }
  }, []);

  const disable = useCallback(async () => {
    setBusy(true);
    try {
      setTimer(await api.timerUninstall());
      await refresh();
      notify.info("Collection disabled.", "The timer was removed and the collected data deleted.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  const sampleNow = useCallback(async () => {
    setBusy(true);
    try {
      const home = await api.homeDirectory();
      await api.historySnapshotNow(home);
      await refresh();
      notify.success("Sample recorded from the last scan.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  return (
    <section className="view">
      <div className="card">
        <h2>Collection</h2>
        {timer === null ? (
          <p className="muted">Checking…</p>
        ) : (
          <>
            <p className="muted">
              Off by default. When enabled, nix records one sample a day: category totals and the
              largest directories, a few kilobytes each. Not a copy of your filesystem — the question
              this answers is &ldquo;what grew&rdquo;, and that needs trends rather than detail.
            </p>
            {timer.tier === "session" ? (
              <p className="caveat">
                This system cannot install a user timer, so samples can only be taken while nix is
                open. Expect gaps, and nix will show them as gaps.
              </p>
            ) : (
              <p className="muted">
                The job runs at <code>Nice=19</code> with idle I/O priority, never on battery, and a
                run missed while the machine was off happens at your next login instead of vanishing.
                nix does not enable lingering — collection needs you to be logged in at some point.
              </p>
            )}
            {timer.orphaned && (
              <p className="caveat">
                There is a collection job from a previous version of nix installed, and it no longer
                matches what this version would run — most likely because the program moved. Left
                alone it would fail silently every day. Enabling collection again rewrites it.
              </p>
            )}
            <div className="row">
              {timer.enabled ? (
                <button type="button" onClick={() => void disable()} disabled={busy}>
                  Disable and delete collected data
                </button>
              ) : (
                <button
                  type="button"
                  onClick={() => void enable()}
                  disabled={busy || timer.tier === "session"}
                >
                  {timer.orphaned ? "Repair and enable" : "Enable daily collection"}
                </button>
              )}
              <button type="button" onClick={() => void sampleNow()} disabled={busy}>
                Record one now
              </button>
            </div>
          </>
        )}
      </div>

      <div className="card">
        <h2>Over time</h2>
        {samples === null ? (
          <p className="muted">Loading…</p>
        ) : samples.length === 0 ? (
          <p className="muted">
            Nothing collected yet. Record one now to start a series, or enable daily collection.
          </p>
        ) : samples.length === 1 ? (
          <p className="muted">
            One sample so far, of {formatBytes(samples[0].total_allocated)}. A trend needs two points —
            there is nothing to say about change yet, and nix will not invent it.
          </p>
        ) : (
          <>
            {series && <Bars series={series} />}
            {series && series.gaps > 0 && (
              <p className="muted">
                {series.gaps} interval{series.gaps === 1 ? "" : "s"} with no sample, shown as gaps. nix
                never draws a line through a period it knows nothing about.
              </p>
            )}
            {growth?.total ? (
              <p>
                Over the last week: <Delta delta={growth.total.delta} />, from{" "}
                {formatBytes(growth.total.from)} to {formatBytes(growth.total.to)}.
              </p>
            ) : (
              <p className="muted">
                Not enough samples inside the last week to say what changed.
              </p>
            )}
          </>
        )}
      </div>

      {growth && growth.directories.length > 0 && (
        <div className="card">
          <h2>What moved</h2>
          <p className="muted">
            Directories that were among the largest in both samples. A directory absent from the older
            one is not listed: it may simply not have been big enough to record then, which is not the
            same as having been empty.
          </p>
          <ul className="find-list">
            {growth.directories.map(([path, change]) => (
              <li key={path}>
                <span className="find-bytes">
                  <Delta delta={change.delta} />
                </span>
                <code title={path}>{path}</code>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
