// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Services — `SVC-1` to `SVC-5`.
 *
 * **Nothing here is filtered out.** Stacer listed with `--state=enabled,disabled`, which on this
 * machine would have shown 183 of 491 unit files — hiding 205 `static` units, 34 `transient`, 14
 * `masked` and 34 aliases, plus every instantiated template. `masked` is the state a user most needs
 * to see, because it is the one that makes something refuse to start no matter what asks for it.
 *
 * **The list updates itself.** systemd emits a signal when a unit changes, so enabling something in a
 * terminal is reflected here without a refresh button. Stacer loaded its list once per run.
 *
 * **nix writes no privileged code for any of this.** systemd asks polkit itself, so the prompt is
 * systemd's own, with systemd's own wording — and a refusal comes back as a refusal.
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import {
  api,
  onUnitChanged,
  toAppError,
  type Action,
  type Page,
  type Scope,
  type Timer,
  type Unit,
  type UnitFile,
} from "../lib/ipc";
import { t } from "../lib/i18n";
import { notify } from "../lib/notices";

/** Relative time from a signed second count, which is what a timer's countdown is. */
function relative(seconds: number): string {
  const overdue = seconds < 0;
  const s = Math.abs(seconds);
  const text =
    s < 90 ? `${Math.round(s)}s` : s < 5400 ? `${Math.round(s / 60)} min` : `${(s / 3600).toFixed(1)} h`;
  return overdue ? `${text} overdue` : `in ${text}`;
}

export default function Services() {
  const [scope, setScope] = useState<Scope>("system");
  const [units, setUnits] = useState<Unit[]>([]);
  const [files, setFiles] = useState<UnitFile[] | null>(null);
  const [timers, setTimers] = useState<Timer[] | null>(null);
  const [filter, setFilter] = useState("");
  const [kind, setKind] = useState("service");
  const [selected, setSelected] = useState<string | null>(null);
  const [logs, setLogs] = useState<Page | null>(null);
  const [busy, setBusy] = useState(false);
  const [unavailable, setUnavailable] = useState<string | null>(null);

  const refresh = useCallback(
    async (which: Scope) => {
      try {
        setUnits(await api.unitsList(which));
        setUnavailable(null);
      } catch (thrown) {
        const error = toAppError(thrown);
        // A user manager may simply not exist — in a sandbox, or over ssh. That is not a fault.
        setUnavailable(error.message);
        setUnits([]);
      }
    },
    [],
  );

  useEffect(() => {
    void refresh(scope);
    setFiles(null);
    setTimers(null);
  }, [scope, refresh]);

  // SVC-3. One subscription for the process; systemd is silent until something asks it to speak.
  useEffect(() => {
    void api.unitsWatch().catch(() => {
      // Without the watcher the list still works, it just will not update itself.
    });
    const subscription = onUnitChanged(() => void refresh(scope));
    return () => {
      void subscription.then((un) => un());
    };
  }, [scope, refresh]);

  // SVC-5. Re-fetched from a cursor, so a quiet unit costs nothing to follow.
  useEffect(() => {
    if (selected === null) {
      setLogs(null);
      return;
    }
    let live = true;
    let cursor: string | undefined;

    const poll = async () => {
      try {
        const page = await api.unitLogs(scope, selected, 60, cursor);
        if (!live) return;
        if (page.cursor) cursor = page.cursor;
        setLogs((previous) =>
          previous && cursor
            ? { entries: [...previous.entries, ...page.entries].slice(-200), cursor: page.cursor }
            : page,
        );
      } catch (thrown) {
        if (live) {
          setLogs({ entries: [], cursor: null });
          notify.error(toAppError(thrown));
        }
      }
    };

    setLogs(null);
    void poll();
    const timer = setInterval(() => void poll(), 3000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [selected, scope]);

  const kinds = useMemo(() => {
    const found = new Set(units.map((u) => u.name.split(".").pop() ?? ""));
    return ["all", ...[...found].filter(Boolean).sort()];
  }, [units]);

  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return units
      .filter((u) => kind === "all" || u.name.endsWith(`.${kind}`))
      .filter(
        (u) =>
          !needle ||
          u.name.toLowerCase().includes(needle) ||
          u.description.toLowerCase().includes(needle),
      )
      .sort((a, b) => {
        // Failures first: it is what a service list is usually opened to find.
        const problem = (u: Unit) => (u.active_state === "failed" ? 0 : u.load_state === "masked" ? 1 : 2);
        return problem(a) - problem(b) || a.name.localeCompare(b.name);
      });
  }, [units, filter, kind]);

  const chosen = shown.find((u) => u.name === selected) ?? null;
  const chosenFile = files?.find((f) => f.name === selected) ?? null;

  const act = useCallback(
    async (unit: string, action: Action) => {
      setBusy(true);
      try {
        await api.unitAct(scope, unit, action);
        notify.success(`${action} ${unit}.`);
        await refresh(scope);
      } catch (thrown) {
        // A denied authorisation arrives as a denial, never as a success.
        notify.error(toAppError(thrown));
      } finally {
        setBusy(false);
      }
    },
    [scope, refresh],
  );

  const failed = units.filter((u) => u.active_state === "failed");
  const masked = units.filter((u) => u.load_state === "masked");

  return (
    <section className="view">
      <div className="card">
        <div className="row">
          <label className="field field-inline">
            <span>{t("Manager")}</span>
            <select value={scope} onChange={(e) => setScope(e.currentTarget.value as Scope)}>
              <option value="system">{t("System")}</option>
              <option value="user">{t("User")}</option>
            </select>
          </label>
          <label className="field field-inline">
            <span>{t("Kind")}</span>
            <select value={kind} onChange={(e) => setKind(e.currentTarget.value)}>
              {kinds.map((k) => (
                <option key={k} value={k}>
                  {k}
                </option>
              ))}
            </select>
          </label>
          <input
            type="search"
            placeholder={t("Filter by name or description")}
            value={filter}
            onChange={(e) => setFilter(e.currentTarget.value)}
            aria-label={t("Filter units")}
          />
        </div>

        {unavailable ? (
          <p className="muted">{unavailable}</p>
        ) : (
          <p className="muted">
            {shown.length} of {units.length} units
            {failed.length > 0 && <> · {failed.length} failed</>}
            {masked.length > 0 && <> · {masked.length} masked</>}. Static, generated, transient and
            template units are all included — a service list that hides them hides most of what is
            running.
          </p>
        )}

        <div className="unit-scroll">
          <ul className="unit-list">
            {shown.slice(0, 400).map((unit) => (
              <li
                key={unit.name}
                className={
                  [
                    unit.name === selected ? "is-selected" : "",
                    unit.active_state === "failed" ? "is-failed" : "",
                    unit.load_state === "masked" ? "is-masked" : "",
                  ]
                    .filter(Boolean)
                    .join(" ") || undefined
                }
              >
                <button type="button" onClick={() => setSelected(unit.name)}>
                  <span className="unit-name">{unit.name}</span>
                  <span className="unit-state">
                    {unit.load_state === "masked" ? "masked" : unit.active_state}
                    <span className="muted"> {unit.sub_state}</span>
                  </span>
                  <span className="unit-description muted">{unit.description}</span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      </div>

      {chosen && (
        <div className="card">
          <h2>{chosen.name}</h2>
          <p className="muted">{chosen.description}</p>
          <p className="muted">
            {chosen.load_state} · {chosen.active_state} ({chosen.sub_state})
            {chosen.following && <> · follows {chosen.following}</>}
            {chosenFile && <> · {chosenFile.state}</>}
          </p>

          {chosen.load_state === "masked" && (
            <p className="caveat">
              {t(
                "Masked. It will refuse to start no matter what asks for it, including a dependency — which is different from being disabled, and is why this state is worth seeing.",
              )}
            </p>
          )}

          <div className="row">
            <button type="button" disabled={busy} onClick={() => void act(chosen.name, "start")}>
              {t("Start")}
            </button>
            <button type="button" disabled={busy} onClick={() => void act(chosen.name, "stop")}>
              {t("Stop")}
            </button>
            <button type="button" disabled={busy} onClick={() => void act(chosen.name, "restart")}>
              {t("Restart")}
            </button>
            <button type="button" disabled={busy} onClick={() => void act(chosen.name, "reload")}>
              {t("Reload")}
            </button>
          </div>

          <p className="muted">
            {t(
              "Above changes what is running now. Below changes what happens at boot — a different intention, and doing one when you meant the other is a surprise on the next restart.",
            )}
          </p>

          <div className="row">
            {files === null ? (
              <button
                type="button"
                onClick={() =>
                  void api
                    .unitFiles(scope)
                    .then(setFiles)
                    .catch((thrown) => notify.error(toAppError(thrown)))
                }
              >
                {t("Load boot settings")}
              </button>
            ) : (
              <>
                <button
                  type="button"
                  disabled={busy || (chosenFile !== null && !["enabled", "disabled", "enabled-runtime"].includes(chosenFile.state))}
                  onClick={() => void act(chosen.name, chosenFile?.state === "enabled" ? "disable" : "enable")}
                >
                  {chosenFile?.state === "enabled" ? "Disable at boot" : "Enable at boot"}
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void act(chosen.name, chosen.load_state === "masked" ? "unmask" : "mask")}
                >
                  {chosen.load_state === "masked" ? "Unmask" : "Mask"}
                </button>
              </>
            )}
          </div>

          {files !== null && chosenFile !== null && !["enabled", "disabled", "enabled-runtime"].includes(chosenFile.state) && (
            <p className="muted">
              This is a <code>{chosenFile.state}</code> unit, so it cannot be enabled or disabled.
              {chosenFile.state === "static" &&
                " It has no [Install] section — something else pulls it in when it is needed."}
              {chosenFile.state === "generated" && " It was produced by a generator rather than written to disk."}
            </p>
          )}
          {files !== null && chosenFile === null && (
            <p className="muted">
              {t("No unit file on disk — it is transient, created at runtime, so there is nothing to enable.")}
            </p>
          )}

          <h3>{t("Recent log")}</h3>
          {logs === null ? (
            <p className="muted">{t("Reading…")}</p>
          ) : logs.entries.length === 0 ? (
            <p className="muted">{t("Nothing in the journal for this unit.")}</p>
          ) : (
            <ul className="log-list">
              {logs.entries.slice(-60).map((entry) => (
                <li key={entry.cursor} className={entry.severity === "error" || entry.severity === "critical" || entry.severity === "alert" || entry.severity === "emergency" ? "is-problem" : undefined}>
                  <span className="log-time">
                    {new Date(Number(entry.at_us) / 1000).toLocaleTimeString()}
                  </span>
                  <span className="log-message">{entry.message}</span>
                </li>
              ))}
            </ul>
          )}
          <p className="muted">
            {t(
              "Followed by asking what has appeared since the last entry, so a quiet unit costs nothing to watch.",
            )}
          </p>
        </div>
      )}

      {/* SVC-4. Absent from Stacer entirely. */}
      <div className="card">
        <h2>{t("Timers")}</h2>
        {timers === null ? (
          <button
            type="button"
            onClick={() =>
              void api
                .unitsTimers(scope)
                .then(setTimers)
                .catch((thrown) => notify.error(toAppError(thrown)))
            }
          >
            {t("Show timers")}
          </button>
        ) : timers.length === 0 ? (
          <p className="muted">{t("No timers in this manager.")}</p>
        ) : (
          <>
            <ul className="unit-list">
              {timers.map((timer) => (
                <li key={timer.name}>
                  <button type="button" onClick={() => setSelected(timer.name)}>
                    <span className="unit-name">{timer.name}</span>
                    <span className="unit-state">
                      {timer.seconds_until !== null
                        ? relative(Number(timer.seconds_until))
                        : timer.scheduled
                          ? "scheduled"
                          : "not scheduled"}
                    </span>
                    <span className="unit-description muted">starts {timer.unit}</span>
                  </button>
                </li>
              ))}
            </ul>
            <p className="muted">
              A calendar timer has a wall-clock time; one using <code>OnBootSec</code> or{" "}
              <code>OnUnitActiveSec</code> does not, so its countdown is measured against uptime
              instead. Both give a real answer; neither is invented.
            </p>
          </>
        )}
      </div>
    </section>
  );
}
