/**
 * Settings. Reads and writes the real persisted store (task 0.6).
 */
import { useEffect, useState } from "react";

import { api, toAppError, type Settings, type Theme } from "../lib/ipc";
import { notify } from "../lib/notices";
import { applyTheme } from "../lib/theme";

const THEMES: Array<{ value: Theme; label: string }> = [
  { value: "system", label: "Follow the desktop" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
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
