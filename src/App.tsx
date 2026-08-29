// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Application root: resolves the theme and the start view, then hands off to the shell.
 */
import { useEffect, useState } from "react";

import { Shell, type ViewId } from "./components/Shell";
import { api, toAppError } from "./lib/ipc";
import { notify } from "./lib/notices";
import { FirstRun } from "./components/FirstRun";
import { setLocale } from "./lib/i18n";
import { applyTheme, watchSystemTheme } from "./lib/theme";
import type { Theme } from "./lib/ipc";

export default function App() {
  const [ready, setReady] = useState(false);
  const [startView, setStartView] = useState<ViewId>("overview");
  const [preference, setPreference] = useState<Theme>("system");
  // `null` while unknown, so the first run screen does not flash on top of a normal start.
  const [introduced, setIntroduced] = useState<boolean | null>(null);

  // The saved language, restored before anything renders text. Independent of the settings store,
  // which is a Rust type the frontend does not get to add fields to for a preference it owns.
  useEffect(() => {
    let code: string | null = null;
    try {
      code = localStorage.getItem("nix.locale");
    } catch {
      // Storage disabled. English, then.
    }
    if (code !== null && code !== "en") void setLocale(code);
  }, []);

  useEffect(() => {
    api
      .settingsGet()
      .then((settings) => {
        setPreference(settings.theme);
        applyTheme(settings.theme);
        setIntroduced(settings.introduced);
        // The stored value is a stable identifier, so it maps directly onto a view id.
        setStartView(settings.start_view as ViewId);
      })
      .catch((thrown) => {
        // Defaults are already applied; the failure is worth reporting but not worth blocking on.
        notify.error(toAppError(thrown));
        applyTheme("system");
      })
      .finally(() => setReady(true));
  }, []);

  // Follow the desktop while the preference is "system".
  useEffect(() => {
    if (preference !== "system") return;
    return watchSystemTheme(() => applyTheme("system"));
  }, [preference]);

  // Nothing renders until the theme is resolved, so the window cannot flash the wrong palette.
  if (!ready) return null;

  // Shown before the shell rather than over it: the point is to say what nix will and will not do
  // before the user is anywhere near a button that deletes something, and a modal floating over a
  // populated window invites dismissing it to get at what is underneath.
  if (introduced === false) return <FirstRun onDone={() => setIntroduced(true)} />;

  return <Shell initialView={startView} />;
}
