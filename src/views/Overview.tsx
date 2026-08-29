// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Overview — `MON-2`, with `MON-3`'s charts, `MON-4`'s sensors, `MON-5`'s battery and `MON-7`'s
 * interfaces.
 *
 * **Storage leads.** This is a storage-first product, so the first thing on the dashboard is where
 * the disk went and what could come back — not a CPU gauge. Stacer's dashboard led with CPU and
 * buried the disk in a corner, which told you about the resource you could do least about.
 *
 * **Nothing is scanned on mount.** Filesystem figures come from `statvfs`, which is instant. The
 * reclaimable figure is whatever the last preview found, or an honest "not measured yet" — a
 * dashboard that scans when you open it is a dashboard you stop opening.
 *
 * Sampling starts when this view mounts and stops when it unmounts (§P9), so the machine is not
 * being measured while nobody is looking.
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import Chart, { formatRate, palette, type Series } from "../components/Chart";
import { t } from "../lib/i18n";
import { formatBytes, formatPercent } from "../lib/format";
import {
  api,
  onMetricsTick,
  toAppError,
  type Filesystem,
  type Metric,
  type Reading,
} from "../lib/ipc";
import { notify } from "../lib/notices";

/** How many slots the charts hold, matching the backend's ring. */
const WINDOW = 60;

function seriesOf(readings: Reading[], pick: (r: Reading) => number): Array<number | null> {
  return readings.map(pick);
}

/** Seconds to a short human duration. */
function duration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return h > 0 ? `${h} h ${m} min` : `${m} min`;
}

/** A fired alert, in words. */
function describeAlert(metric: Metric): string {
  switch (metric.metric) {
    case "cpu_usage":
      return "CPU has been busy past your threshold.";
    case "memory_pressure":
      return "Memory is nearly full.";
    case "swap_pressure":
      return "Swap is filling up.";
    case "temperature":
      return "Something is running hot.";
    case "disk_usage":
      return `${metric.mount} is nearly full.`;
    case "disk_space_remaining":
      return `${metric.mount} is low on free space.`;
    default:
      return "A threshold you set was crossed.";
  }
}

export default function Overview() {
  const [readings, setReadings] = useState<Reading[]>([]);
  const [filesystems, setFilesystems] = useState<Filesystem[]>([]);
  const [reclaimable, setReclaimable] = useState<[number, number] | null>(null);

  // Subscribe on mount, release on unmount. The returned history is what makes a late mount show a
  // full chart rather than an empty one.
  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const history = await api.metricsSubscribe();
        if (live) setReadings(history);
      } catch (thrown) {
        notify.error(toAppError(thrown));
      }
    })();

    const subscription = onMetricsTick((reading) => {
      setReadings((previous) => [...previous, reading].slice(-WINDOW));

      // MON-6. The state machine lives in the backend, so hysteresis and cooldown survive this view
      // unmounting — and only a *fresh* crossing comes back, never a rule that is merely still over.
      void api
        .alertsEvaluate()
        .then((fired) => {
          for (const metric of fired) {
            notify.warning(describeAlert(metric), "You set a threshold for this in Settings.");
          }
        })
        .catch(() => {
          // A failed evaluation must not take the dashboard down with it.
        });
    });

    return () => {
      live = false;
      void subscription.then((un) => un());
      void api.metricsUnsubscribe();
    };
  }, []);

  // Storage, from figures that cost nothing to read.
  useEffect(() => {
    void (async () => {
      try {
        const [fs, last] = await Promise.all([api.filesystems(), api.reclaimLastTotal()]);
        setFilesystems(fs.filter((f) => !f.pseudo));
        setReclaimable(last);
      } catch (thrown) {
        notify.error(toAppError(thrown));
      }
    })();
  }, []);

  const latest = readings.at(-1) ?? null;
  const cores = latest?.cpu.per_core.length ?? 0;
  const corePalette = useMemo(() => palette(cores), [cores]);

  const cpuSeries: Series[] = useMemo(
    () => [
      {
        label: "CPU",
        points: seriesOf(readings, (r) => r.cpu.total * 100),
        colour: "var(--accent)",
      },
    ],
    [readings],
  );

  const coreSeries: Series[] = useMemo(
    () =>
      Array.from({ length: cores }, (_, core) => ({
        label: `Core ${core}`,
        points: readings.map((r) => (r.cpu.per_core[core] ?? 0) * 100),
        colour: corePalette[core] ?? "var(--accent)",
      })),
    [readings, cores, corePalette],
  );

  const memorySeries: Series[] = useMemo(
    () => [
      {
        label: "Memory",
        points: seriesOf(readings, (r) =>
          r.memory.total > 0 ? (1 - r.memory.available / r.memory.total) * 100 : 0,
        ),
        colour: "var(--accent)",
      },
    ],
    [readings],
  );

  const networkSeries: Series[] = useMemo(
    () => [
      { label: "Received", points: seriesOf(readings, (r) => r.network.received_per_second), colour: "var(--safe)" },
      { label: "Sent", points: seriesOf(readings, (r) => r.network.sent_per_second), colour: "var(--review)" },
    ],
    [readings],
  );

  const diskSeries: Series[] = useMemo(
    () => [
      { label: "Read", points: seriesOf(readings, (r) => r.disk.totals.read_per_second), colour: "var(--safe)" },
      { label: "Written", points: seriesOf(readings, (r) => r.disk.totals.written_per_second), colour: "var(--review)" },
    ],
    [readings],
  );

  const featured = useMemo(() => {
    const interfaces = latest?.network.interfaces ?? [];
    const physical = interfaces.filter((i) => i.physical && i.carrier && i.operstate !== "down");
    return physical.reduce<typeof physical[number] | null>(
      (best, i) =>
        best === null || i.received_per_second + i.sent_per_second > best.received_per_second + best.sent_per_second
          ? i
          : best,
      null,
    );
  }, [latest]);

  const peakPercent = useCallback((v: number) => `${Math.round(v)}%`, []);

  return (
    <section className="view">
      {/* Storage first: this is a storage tool. */}
      <div className="card">
        <h2>{t("Storage")}</h2>
        {reclaimable ? (
          <p>
            <strong className="summary-figure">{formatBytes(reclaimable[1])}</strong> could be
            reclaimed, from {formatBytes(reclaimable[0])} found.
          </p>
        ) : (
          <p className="muted">
            {t(
              "Not measured yet. nix does not scan when you open this page — the Reclaim view looks when you ask it to.",
            )}
          </p>
        )}
        <ul className="fs-list">
          {filesystems.map((fs) => (
            <li key={fs.mount_point}>
              <span className="fs-mount" title={fs.device}>
                {fs.mount_point}
              </span>
              <span className="fs-bar" aria-hidden>
                <span
                  className={fs.total > 0 && fs.used / fs.total > 0.9 ? "fs-fill is-full" : "fs-fill"}
                  style={{ width: `${fs.total > 0 ? (fs.used / fs.total) * 100 : 0}%` }}
                />
              </span>
              <span className="fs-figures">
                {formatBytes(fs.available)} free of {formatBytes(fs.total)}
              </span>
            </li>
          ))}
        </ul>
      </div>

      <div className="card">
        <h2>CPU</h2>
        {latest ? (
          <>
            <Chart
              series={cpuSeries}
              max={100}
              capacity={WINDOW}
              caption={`${formatPercent(latest.cpu.total)} · load ${latest.load.one.toFixed(2)}`}
              formatPeak={peakPercent}
            />
            <Chart series={coreSeries} max={100} capacity={WINDOW} height={60} caption={`${cores} cores`} />
            <p className="muted">
              {latest.cpu.frequency_khz !== null &&
                `${(latest.cpu.frequency_khz / 1000).toFixed(0)} MHz · `}
              {latest.load.running} of {latest.load.total} tasks runnable
            </p>
          </>
        ) : (
          <p className="muted">{t("Waiting for a second reading — one sample of a counter is not a rate.")}</p>
        )}
      </div>

      <div className="card">
        <h2>{t("Memory")}</h2>
        {latest ? (
          <>
            <Chart
              series={memorySeries}
              max={100}
              capacity={WINDOW}
              caption={`${formatBytes(latest.memory.total - latest.memory.available)} in use of ${formatBytes(latest.memory.total)}`}
              formatPeak={peakPercent}
            />
            <p className="muted">
              {formatBytes(latest.memory.available)} available, {formatBytes(latest.memory.buffers_cache)}{" "}
              in caches.{" "}
              {latest.memory.swap_total > 0
                ? `Swap ${formatBytes(latest.memory.swap_used)} of ${formatBytes(latest.memory.swap_total)}.`
                : "No swap configured."}
            </p>
          </>
        ) : (
          <p className="muted">{t("Waiting…")}</p>
        )}
      </div>

      <div className="card">
        <h2>{t("Disk and network")}</h2>
        {latest ? (
          <>
            <Chart
              series={diskSeries}
              capacity={WINDOW}
              caption={`read ${formatRate(latest.disk.totals.read_per_second)} · write ${formatRate(latest.disk.totals.written_per_second)}`}
              formatPeak={formatRate}
            />
            <Chart
              series={networkSeries}
              capacity={WINDOW}
              caption={
                featured
                  ? `${featured.name}: down ${formatRate(latest.network.received_per_second)} · up ${formatRate(latest.network.sent_per_second)}`
                  : "no connected interface"
              }
              formatPeak={formatRate}
            />
            {!featured && (
              <p className="muted">
                {t(
                  "No hardware interface has a link. Totals count physical interfaces only, so each byte is counted once rather than once per bridge it crosses.",
                )}
              </p>
            )}
          </>
        ) : (
          <p className="muted">{t("Waiting…")}</p>
        )}
      </div>

      {/* MON-5. Hidden entirely on a desktop rather than shown empty. */}
      {latest && latest.power.batteries.length > 0 && (
        <div className="card">
          <h2>{t("Battery")}</h2>
          {latest.power.batteries.map((battery) => (
            <div key={battery.name}>
              <p>
                <strong className="summary-figure">{battery.percent}%</strong>{" "}
                <span className="muted">
                  {battery.state.replace("_", " ")}
                  {battery.watts > 0 && ` · ${battery.watts.toFixed(1)} W`}
                  {battery.seconds_remaining !== null &&
                    ` · ${duration(battery.seconds_remaining)} remaining`}
                </span>
              </p>
              <p className="muted">
                {battery.health_percent !== null && (
                  <>
                    Health {battery.health_percent}% — it holds that much of what it did when new.
                    {battery.health_percent < 70 && " Worth replacing before long."}
                  </>
                )}
                {battery.cycles !== null && ` ${battery.cycles} cycles.`}
              </p>
            </div>
          ))}
          {latest.power.on_mains !== null && (
            <p className="muted">{latest.power.on_mains ? "On mains power." : "Running on battery."}</p>
          )}
        </div>
      )}

      {/* MON-4. */}
      {latest && latest.sensors.temperatures.length > 0 && (
        <div className="card">
          <h2>{t("Temperatures")}</h2>
          <ul className="sensor-list">
            {latest.sensors.temperatures.slice(0, 8).map((t) => (
              <li key={`${t.chip}-${t.label}`}>
                <span className="sensor-name">
                  {t.label} <span className="muted">{t.chip}</span>
                </span>
                <span className="sensor-bar" aria-hidden>
                  <span
                    className={
                      t.critical_celsius !== null && t.celsius / t.critical_celsius > 0.85
                        ? "sensor-fill is-hot"
                        : "sensor-fill"
                    }
                    style={{
                      width: `${t.critical_celsius !== null ? Math.min(100, (t.celsius / t.critical_celsius) * 100) : 50}%`,
                    }}
                  />
                </span>
                <span className="sensor-value">{t.celsius.toFixed(0)}°C</span>
              </li>
            ))}
          </ul>
          {latest.sensors.fans.length > 0 ? (
            <p className="muted">
              {latest.sensors.fans.map((f) => `${f.label} ${f.rpm} RPM`).join(" · ")}
            </p>
          ) : (
            <p className="muted">{t("This machine reports no fan speeds, which is common and not a fault.")}</p>
          )}
        </div>
      )}

      {/* MON-7. */}
      {latest && latest.network.interfaces.length > 0 && (
        <div className="card">
          <h2>{t("Interfaces")}</h2>
          <p className="muted">
            {t(
              "Which one is featured is decided from this reading, not remembered — unplug the Ethernet and join Wi-Fi and the answer changes on the next tick.",
            )}
          </p>
          <ul className="iface-list">
            {latest.network.interfaces
              .filter((i) => i.physical || i.received_per_second + i.sent_per_second > 0)
              .slice(0, 12)
              .map((i) => (
                <li key={i.name} className={i.name === featured?.name ? "is-featured" : undefined}>
                  <span className="iface-name">
                    {i.name}
                    {!i.physical && <span className="muted"> {t("virtual")}</span>}
                  </span>
                  <span className="iface-state">
                    {i.carrier ? i.operstate : "no link"}
                    {i.speed_mbps !== null && ` · ${i.speed_mbps} Mb/s`}
                  </span>
                  <span className="iface-rates">
                    ↓ {formatRate(i.received_per_second)} ↑ {formatRate(i.sent_per_second)}
                  </span>
                  {i.mac && <code className="iface-mac">{i.mac}</code>}
                </li>
              ))}
          </ul>
        </div>
      )}
    </section>
  );
}
