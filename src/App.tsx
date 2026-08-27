// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Application root: resolves the theme and the start view, then hands off to the shell.
 */
import { useEffect, useState } from "react";

import { Shell, type ViewId } from "./components/Shell";
import { api, toAppError } from "./lib/ipc";
import { notify } from "./lib/notices";
import { applyTheme, watchSystemTheme } from "./lib/theme";
import type { Theme } from "./lib/ipc";

export default function App() {
  const [ready, setReady] = useState(false);
  const [startView, setStartView] = useState<ViewId>("overview");
  const [preference, setPreference] = useState<Theme>("system");

  useEffect(() => {
    api
      .settingsGet()
      .then((settings) => {
        setPreference(settings.theme);
        applyTheme(settings.theme);
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

  return <Shell initialView={startView} />;
}
