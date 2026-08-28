// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Settings. Reads and writes the real persisted store (task 0.6).
 */
import { useEffect, useState } from "react";

import { formatBytes } from "../lib/format";
import { api, toAppError, type Rule, type Settings, type Theme } from "../lib/ipc";
import { notify } from "../lib/notices";
import { applyTheme } from "../lib/theme";

const THEMES: Array<{ value: Theme; label: string }> = [
  { value: "system", label: "Follow the desktop" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

/** What a rule watches, in words. */
function describeMetric(metric: Rule["metric"]): string {
  switch (metric.metric) {
    case "cpu_usage":
      return "CPU usage";
    case "memory_pressure":
      return "Memory in use";
    case "swap_pressure":
      return "Swap in use";
    case "temperature":
      return "Hottest sensor";
    case "disk_usage":
      return `Disk usage on ${metric.mount}`;
    case "disk_space_remaining":
      return `Free space on ${metric.mount}`;
    default:
      // A metric this build does not know about — a settings file from a newer version. Shown as
      // itself rather than dropped, so a user can see and remove it.
      return JSON.stringify(metric);
  }
}

/**
 * Starting points, rather than a form with six fields.
 *
 * The thresholds are the ones worth being told about — 90% of a disk, 85°C, memory with almost
 * nothing left — and each carries a hysteresis margin and a cooldown so it cannot chatter.
 */
const SUGGESTED: Array<{ label: string; rule: Rule }> = [
  {
    label: "Disk nearly full",
    rule: {
      metric: { metric: "disk_usage", mount: "/" },
      threshold: 0.9,
      hysteresis: 0.03,
      cooldown_seconds: 3600,
      enabled: true,
    },
  },
  {
    label: "Low free space",
    rule: {
      metric: { metric: "disk_space_remaining", mount: "/" },
      threshold: 5_000_000_000,
      hysteresis: 1_000_000_000,
      cooldown_seconds: 3600,
      enabled: true,
    },
  },
  {
    label: "Memory pressure",
    rule: {
      metric: { metric: "memory_pressure" },
      threshold: 0.9,
      hysteresis: 0.05,
      cooldown_seconds: 600,
      enabled: true,
    },
  },
  {
    label: "Running hot",
    rule: {
      metric: { metric: "temperature" },
      threshold: 85,
      hysteresis: 5,
      cooldown_seconds: 600,
      enabled: true,
    },
  },
];

export default function SettingsView() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api
      .settingsGet()
      .then(setSettings)
      .catch((thrown) => notify.error(toAppError(thrown)));
  }, []);

  async function update(patch: Partial<Settings>) {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    setSaving(true);
    try {
      const saved = await api.settingsSave(next);
      setSettings(saved);
      if (patch.theme) applyTheme(saved.theme);
      notify.success("Settings saved.");
    } catch (thrown) {
      notify.error(toAppError(thrown));
      // Re-read, so the UI shows what is actually on disk rather than what we hoped.
      api.settingsGet().then(setSettings).catch(() => {});
    } finally {
      setSaving(false);
    }
  }

  if (!settings) return <p className="empty">Loading settings…</p>;

  return (
    <section className="stack">
      <div className="card">
        <h2>Appearance</h2>
        <label className="field">
          <span>Theme</span>
          <select
            value={settings.theme}
            disabled={saving}
            onChange={(e) => void update({ theme: e.currentTarget.value as Theme })}
          >
            {THEMES.map((t) => (
              <option key={t.value} value={t.value}>
                {t.label}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div className="card">
        <h2>Storage</h2>
        <label className="field field-inline">
          <input
            type="checkbox"
            checked={settings.show_pseudo_filesystems}
            disabled={saving}
            onChange={(e) => void update({ show_pseudo_filesystems: e.currentTarget.checked })}
          />
          <span>
            Show pseudo-filesystems
            <small>tmpfs, squashfs and overlay mounts. Hidden by default, because a snap-heavy
            install otherwise shows forty loop mounts.</small>
          </span>
        </label>

        <label className="field field-inline">
          <input
            type="checkbox"
            checked={settings.growth_history_enabled}
            disabled={saving}
            onChange={(e) => void update({ growth_history_enabled: e.currentTarget.checked })}
          />
          <span>
            Track disk usage over time
            <small>Installs a systemd user timer that records category totals daily. Off by
            default, because it changes your system. Turning it off removes the timer and deletes
            the collected data.</small>
          </span>
        </label>
      </div>

      {/* MON-6. Empty by default: a tool that notifies about thresholds nobody chose is one whose
          notifications get switched off wholesale. */}
      <div className="card">
        <h2>Alerts</h2>
        <p className="muted">
          Off unless you add one. Each fires once when it crosses, stays quiet while the condition
          lasts, and will not fire again until it has come back past the threshold by a margin and
          the cooldown has passed — so a long build is one notification rather than a hundred.
        </p>

        <ul className="alert-list">
          {(settings.alert_rules ?? []).map((rule, index) => (
            <li key={`${JSON.stringify(rule.metric)}-${index}`}>
              <span className="alert-name">{describeMetric(rule.metric)}</span>
              <span className="alert-threshold">
                {rule.metric.metric === "disk_space_remaining"
                  ? `below ${formatBytes(rule.threshold)}`
                  : rule.metric.metric === "temperature"
                    ? `above ${rule.threshold}°C`
                    : `above ${Math.round(rule.threshold * 100)}%`}
              </span>
              <label className="field field-inline">
                <input
                  type="checkbox"
                  checked={rule.enabled}
                  disabled={saving}
                  onChange={(e) => {
                    const next = [...(settings.alert_rules ?? [])];
                    next[index] = { ...rule, enabled: e.currentTarget.checked };
                    void update({ alert_rules: next });
                  }}
                />
                <span>{rule.enabled ? "on" : "off"}</span>
              </label>
              <button
                type="button"
                disabled={saving}
                onClick={() => {
                  const next = (settings.alert_rules ?? []).filter((_, i) => i !== index);
                  void update({ alert_rules: next });
                }}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>

        <div className="row">
          {SUGGESTED.map((suggestion) => (
            <button
              key={suggestion.label}
              type="button"
              disabled={
                saving ||
                (settings.alert_rules ?? []).some(
                  (r) => JSON.stringify(r.metric) === JSON.stringify(suggestion.rule.metric),
                )
              }
              onClick={() =>
                void update({ alert_rules: [...(settings.alert_rules ?? []), suggestion.rule] })
              }
            >
              {suggestion.label}
            </button>
          ))}
        </div>
      </div>

      <div className="card">
        <h2>Where this is kept</h2>
        <p className="muted">
          Settings are written atomically to <code>$XDG_CONFIG_HOME/nix/settings.json</code>, keyed
          by stable identifiers rather than display names — so changing language cannot orphan a
          preference.
        </p>
      </div>
    </section>
  );
}
