// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * About and diagnostics — and the Phase 0 proving ground.
 *
 * This view exists to satisfy the M0 and M1 gates: every load-bearing mechanism built in Phase 0 is
 * exercisable from here. Once real features arrive, the demo controls go and the diagnostics stay.
 */
import { useEffect, useState } from "react";

import {
  api,
  toAppError,
  type Diagnostics,
  type HelperProbe,
  type Snapshot,
  type Versions,
} from "../lib/ipc";
import { notify } from "../lib/notices";
import { useOperation } from "../lib/useOperation";

const DEMO_ERRORS = [
  "cancelled",
  "auth_denied",
  "not_found",
  "unsupported",
  "refused",
  "internal",
] as const;

export default function About() {
  const [versions, setVersions] = useState<Versions | null>(null);
  const [caps, setCaps] = useState<Snapshot | null>(null);
  const [diag, setDiag] = useState<Diagnostics | null>(null);
  const [helper, setHelper] = useState<HelperProbe | null>(null);
  const [probing, setProbing] = useState(false);
  const op = useOperation();

  useEffect(() => {
    Promise.all([api.versions(), api.capabilities(), api.diagnostics()])
      .then(([v, c, d]) => {
        setVersions(v);
        setCaps(c);
        setDiag(d);
      })
      .catch((thrown) => notify.error(toAppError(thrown)));
  }, []);

  async function probeHelper() {
    setProbing(true);
    setHelper(null);
    try {
      setHelper(await api.helperProbe());
      notify.success("The helper answered.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
    } finally {
      setProbing(false);
    }
  }

  const fraction =
    op.progress && op.progress.total ? op.progress.done / op.progress.total : null;

  return (
    <section className="stack">
      <div className="card">
        <h2>Versions</h2>
        {versions ? (
          <dl className="kv">
            <dt>nix-app</dt>
            <dd>
              <code>{versions.app}</code>
            </dd>
            <dt>nix-core</dt>
            <dd>
              <code>{versions.core}</code>
            </dd>
            <dt>Kernel</dt>
            <dd>
              <code>{diag?.kernel ?? "unknown"}</code>
            </dd>
          </dl>
        ) : (
          <p className="empty">Loading…</p>
        )}
      </div>

      <div className="card">
        <h2>What this system can do</h2>
        <p className="muted">
          Detected by probing for the tool, never by reading a distribution name.
        </p>
        {caps ? (
          caps.present.length > 0 ? (
            <ul className="chips">
              {caps.present.map((c) => (
                <li key={c} className="chip">
                  {c}
                </li>
              ))}
            </ul>
          ) : (
            <p className="empty">Nothing detected.</p>
          )
        ) : (
          <p className="empty">Probing…</p>
        )}
        <button
          type="button"
          onClick={() =>
            api
              .capabilitiesRefresh()
              .then((next) => {
                setCaps(next);
                notify.info("Capabilities re-probed.");
              })
              .catch((thrown) => notify.error(toAppError(thrown)))
          }
        >
          Re-probe
        </button>
      </div>

      <div className="card">
        <h2>Privileged helper</h2>
        <p className="muted">
          One authentication opens a privileged session, rather than a prompt per action. A refused
          authorisation is reported as refused — never as success.
        </p>
        <button type="button" onClick={() => void probeHelper()} disabled={probing}>
          {probing ? "Asking…" : "Ask the helper to identify itself"}
        </button>
        {helper && (
          <dl className="kv">
            <dt>Effective uid</dt>
            <dd>
              <code>{helper.uid}</code>
            </dd>
            <dt>Elevated</dt>
            <dd>{helper.elevated ? "yes" : "no"}</dd>
            <dt>Kernel, read through the helper</dt>
            <dd>
              <code>{helper.kernel}</code>
            </dd>
          </dl>
        )}
      </div>

      <div className="card">
        <h2>Long operations</h2>
        <p className="muted">
          The primitive every scan, search and package query will reuse: progress events, a terminal
          outcome, and cancellation that actually stops the work.
        </p>
        <div className="row">
          <button
            type="button"
            disabled={op.running}
            onClick={() => void op.start(() => api.demoOperation(12))}
          >
            Run for 12 steps
          </button>
          <button
            type="button"
            disabled={op.running}
            onClick={() => void op.start(() => api.demoOperation(12, 5))}
          >
            Fail at step 5
          </button>
          <button type="button" disabled={!op.running} onClick={() => void op.cancel()}>
            Stop
          </button>
        </div>
        {op.running && (
          <div className="progress">
            <div
              className="progress-bar"
              style={{ width: fraction === null ? "100%" : `${Math.round(fraction * 100)}%` }}
            />
            <p className="muted">{op.progress?.message ?? "Starting…"}</p>
          </div>
        )}
        {!op.running && op.outcome && (
          <p className="muted">
            Last run: <strong>{op.outcome}</strong>
          </p>
        )}
      </div>

      <div className="card">
        <h2>Error surface</h2>
        <p className="muted">
          Every failure carries a stable code, a plain-language message, a remedy where one exists,
          and the underlying cause. Try each — they land in the notifications panel.
        </p>
        <div className="row wrap">
          {DEMO_ERRORS.map((code) => (
            <button
              key={code}
              type="button"
              onClick={() =>
                api.demoFailure(code).catch((thrown) => notify.error(toAppError(thrown)))
              }
            >
              {code}
            </button>
          ))}
        </div>
      </div>

      <div className="card">
        <h2>Diagnostics</h2>
        {diag ? (
          <>
            <dl className="kv">
              <dt>Logging installed</dt>
              <dd>{diag.logging_initialised ? "yes" : "no"}</dd>
              <dt>Logs</dt>
              <dd>
                <code>{diag.log_dir ?? "unavailable"}</code>
              </dd>
              <dt>Config</dt>
              <dd>
                <code>{diag.config_dir ?? "unavailable"}</code>
              </dd>
              <dt>State</dt>
              <dd>
                <code>{diag.state_dir ?? "unavailable"}</code>
              </dd>
            </dl>
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard
                  .writeText(JSON.stringify(diag, null, 2))
                  .then(() => notify.success("Diagnostics copied."))
                  .catch(() => notify.warning("Could not reach the clipboard."));
              }}
            >
              Copy diagnostics
            </button>
          </>
        ) : (
          <p className="empty">Collecting…</p>
        )}
      </div>
    </section>
  );
}
