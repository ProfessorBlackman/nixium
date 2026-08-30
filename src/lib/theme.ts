// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Theme tokens. Task 0.6 / FND-6.
 *
 * Three states, not two: an explicit choice, or "system", which follows the desktop. The token
 * palette is carried over from Stacer's own `values.ini` files, which were complete for both light
 * and dark — the theme switcher there was commented out, so the light theme shipped unreachable.
 */
import type { Theme } from "./ipc";

export type ResolvedTheme = "light" | "dark";

/** Resolve a preference against the desktop's setting. */
export function resolveTheme(preference: Theme): ResolvedTheme {
  if (preference === "light" || preference === "dark") return preference;
  const prefersDark =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches;
  return prefersDark ? "dark" : "light";
}

/** Stamp the resolved theme on the document root, where the CSS tokens key off it. */
export function applyTheme(preference: Theme): ResolvedTheme {
  const resolved = resolveTheme(preference);
  document.documentElement.dataset.theme = resolved;
  return resolved;
}

/**
 * Watch the desktop preference while the app is set to "system".
 * Returns a teardown function.
 */
export function watchSystemTheme(onChange: (resolved: ResolvedTheme) => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const handler = () => onChange(query.matches ? "dark" : "light");
  query.addEventListener("change", handler);
  return () => query.removeEventListener("change", handler);
}
