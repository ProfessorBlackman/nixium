// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * WCAG contrast audit of the theme tokens. `PLT-2`.
 *
 * Contrast is the one accessibility property that cannot be eyeballed — a palette can look fine to
 * the person who chose it and be unreadable to someone else, and "looks fine on my monitor" is
 * exactly the judgement the WCAG ratio exists to replace. So it is computed, for both themes, and
 * `make check` fails on a regression.
 *
 * The palette came from Stacer's own `values.ini` files, so this is also the first time anyone has
 * checked those colours against a standard — Stacer's light theme shipped unreachable behind a
 * commented-out switcher, which is a good reason to doubt it was ever looked at.
 *
 * Thresholds are WCAG 2.1 AA: **4.5:1** for body text, **3:1** for large text and for the boundary of
 * a control a user must find. Each pair below says which it is and why.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(join(here, "..", "src", "styles.css"), "utf8");

/** Pull one `:root`-style block's custom properties into a map. */
function tokens(selector) {
  // The blocks are written as `selector {\n  --a: v;\n ... }`; take the first match.
  const at = css.indexOf(selector);
  if (at === -1) throw new Error(`no block for ${selector}`);
  const open = css.indexOf("{", at);
  const close = css.indexOf("}", open);
  const body = css.slice(open + 1, close);

  const found = new Map();
  for (const line of body.split("\n")) {
    const match = line.match(/^\s*(--[a-z0-9-]+)\s*:\s*([^;]+);/i);
    if (match) found.set(match[1], match[2].trim());
  }
  return found;
}

function srgbToLinear(channel) {
  const c = channel / 255;
  return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

/** Relative luminance, per WCAG 2.1. */
function luminance(hex) {
  const value = hex.replace("#", "");
  const full =
    value.length === 3
      ? value
          .split("")
          .map((c) => c + c)
          .join("")
      : value;
  if (!/^[0-9a-f]{6}$/i.test(full)) throw new Error(`not a hex colour: ${hex}`);

  const [r, g, b] = [0, 2, 4].map((i) => Number.parseInt(full.slice(i, i + 2), 16));
  return 0.2126 * srgbToLinear(r) + 0.7152 * srgbToLinear(g) + 0.0722 * srgbToLinear(b);
}

function ratio(a, b) {
  const [x, y] = [luminance(a), luminance(b)];
  const [light, dark] = x > y ? [x, y] : [y, x];
  return (light + 0.05) / (dark + 0.05);
}

/**
 * The pairs that actually appear on screen.
 *
 * Written out rather than derived, because which foreground sits on which background is a fact about
 * the stylesheet's rules, not about the token names — and a generated cross product would report
 * failures for combinations nobody renders.
 */
const PAIRS = [
  // Body text. AA large-text is not enough for these: they are the interface.
  { fg: "--ink", bg: "--ground", min: 4.5, what: "primary text on the page" },
  { fg: "--ink", bg: "--surface", min: 4.5, what: "primary text on a card" },
  { fg: "--ink", bg: "--surface-alt", min: 4.5, what: "primary text on a button" },
  { fg: "--ink-mid", bg: "--surface", min: 4.5, what: "secondary text on a card" },
  { fg: "--ink-mid", bg: "--sidebar", min: 4.5, what: "sidebar item label" },
  { fg: "--ink-mute", bg: "--surface", min: 4.5, what: "muted explanatory text on a card" },
  { fg: "--ink-mute", bg: "--ground", min: 4.5, what: "muted text on the page" },
  { fg: "--ink-mute", bg: "--surface-alt", min: 4.5, what: "muted text on a button or chip" },

  // Accent and its soft background, used for the selected nav item and the confirm panel.
  { fg: "--accent-ink", bg: "--accent-soft", min: 4.5, what: "selected sidebar item" },
  { fg: "--on-accent", bg: "--accent", min: 4.5, what: "text on an accent-filled control" },

  // The safety tiers. These carry meaning, so their text must be readable, not merely tinted.
  { fg: "--safe", bg: "--safe-soft", min: 4.5, what: "safe tier label" },
  { fg: "--review", bg: "--review-soft", min: 4.5, what: "review tier label" },
  { fg: "--risky", bg: "--risky-soft", min: 4.5, what: "risky tier label" },
  { fg: "--review", bg: "--surface", min: 4.5, what: "an amber note on a card" },
  { fg: "--risky", bg: "--surface", min: 4.5, what: "a red warning on a card" },
  { fg: "--safe", bg: "--surface", min: 4.5, what: "a green confirmation on a card" },

  // Non-text: a control's boundary only needs 3:1, but it does need that — an input the user cannot
  // find is not usable however readable its label is.
  { fg: "--rule-strong", bg: "--surface", min: 3, what: "button and input borders" },
  { fg: "--accent", bg: "--surface", min: 3, what: "the focus ring" },
  { fg: "--accent", bg: "--ground", min: 3, what: "the focus ring on the page" },

  // The faintest text in the palette. Used for disabled and "not measured" states, which are
  // deliberately quiet — but AA large is the floor, not a suggestion.
  { fg: "--ink-faint", bg: "--surface", min: 3, what: "disabled or absent-value text" },
];

const THEMES = [
  { name: "light", selector: ":root {" },
  { name: "dark", selector: ':root[data-theme="dark"] {' },
];

let failures = 0;
let checked = 0;

for (const theme of THEMES) {
  const palette = tokens(theme.selector);
  for (const pair of PAIRS) {
    const fg = palette.get(pair.fg);
    const bg = palette.get(pair.bg);
    if (!fg || !bg) {
      console.error(`MISSING ${theme.name}: ${pair.fg} or ${pair.bg} is not defined`);
      failures += 1;
      continue;
    }

    const value = ratio(fg, bg);
    checked += 1;
    if (value < pair.min) {
      failures += 1;
      console.error(
        `FAIL ${theme.name.padEnd(5)} ${value.toFixed(2)}:1 < ${pair.min}:1  ` +
          `${pair.fg} on ${pair.bg} — ${pair.what}`,
      );
    } else if (process.env.CONTRAST_VERBOSE) {
      console.log(
        `pass ${theme.name.padEnd(5)} ${value.toFixed(2)}:1 ≥ ${pair.min}:1  ` +
          `${pair.fg} on ${pair.bg} — ${pair.what}`,
      );
    }
  }
}

if (failures > 0) {
  console.error(
    `\n${failures} of ${checked + failures} token pairs fail WCAG AA. ` +
      `Adjust the palette in src/styles.css — both themes define every token, so fix the one that failed.`,
  );
  process.exit(1);
}

console.log(`contrast: ${checked} token pairs pass WCAG AA across ${THEMES.length} themes`);
