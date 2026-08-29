// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * How much of the interface is translatable. `PLT-1`.
 *
 * `PLT-1`'s criterion is that no user-facing string is hardcoded. The retrofit is not finished — the
 * translation layer, the 26 harvested locales and live switching are in, and the views are not yet
 * converted — so this counts what is left rather than letting the gap sit in a commit message where
 * nobody will look at it again.
 *
 * It **fails the build if the number goes up**. A ratchet rather than a pass/fail: the debt is real and
 * cannot be paid in one commit, but it can be stopped from growing, and every new view lands
 * translatable because adding an untranslated string now breaks CI.
 *
 * # This is a heuristic
 *
 * It looks for capitalised prose in JSX text nodes and string literals, which is what user-facing copy
 * looks like in this codebase. It will miss a template literal built from fragments, and it will
 * occasionally flag a class name or an identifier. The number is a trend line, not an inventory — which
 * is all a ratchet needs.
 */
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const roots = [join(here, "..", "src", "views"), join(here, "..", "src", "components")];

/**
 * The ceiling. Lower it whenever a view is converted; never raise it.
 *
 * Set to the measured count when the translation layer landed. **The number over-counts**, and
 * deliberately is not tuned to stop doing so: `Shell.tsx` contributes 27 despite being fully
 * converted, because its navigation definitions hold the English source in a `const` array and are
 * translated where they are *rendered* — which is correct, and indistinguishable from an untranslated
 * literal without parsing the file properly.
 *
 * A ratchet does not need an accurate inventory. It needs a number that cannot go up, and one that
 * falls when a view is genuinely converted. Both hold with the over-count in it.
 */
const CEILING = 361;

/** Things that look like prose but are not shown to anyone. */
const IGNORE = [
  /^[a-z-]+$/, // class names, ids, css values
  /^\d/, // numbers and versions
  /\.(tsx?|json|css|md)$/, // file names
  /^https?:/, // urls
  /^[A-Z_]+$/, // constants
];

let total = 0;
const perFile = [];

for (const root of roots) {
  for (const name of readdirSync(root).filter((f) => f.endsWith(".tsx"))) {
    const text = readFileSync(join(root, name), "utf8");

    // Strip the module doc comment: it is prose, and it is not rendered.
    const body = text.replace(/^\/\*\*[\s\S]*?\*\//m, "");

    const found = new Set();
    const jsxText = body.matchAll(/>\s*([A-Z][^<>{}\n]{2,80}?)\s*</g);
    const literals = body.matchAll(/"([A-Z][^"\\\n]{2,80})"/g);

    for (const [, candidate] of [...jsxText, ...literals]) {
      const value = candidate.trim();
      if (!value || !/[a-z]/.test(value)) continue;
      if (IGNORE.some((pattern) => pattern.test(value))) continue;
      // Already translated.
      if (body.includes(`t("${value}")`) || body.includes(`tf("${value}"`)) continue;
      found.add(value);
    }

    if (found.size > 0) perFile.push({ name, count: found.size });
    total += found.size;
  }
}

perFile.sort((a, b) => b.count - a.count);

console.log(`i18n: ${total} user-facing strings not yet translatable (ceiling ${CEILING})`);
for (const row of perFile.slice(0, 8)) {
  console.log(`  ${String(row.count).padStart(4)}  ${row.name}`);
}
if (perFile.length > 8) console.log(`  … and ${perFile.length - 8} more files`);

if (total > CEILING) {
  console.error(
    `\nFAIL ${total} exceeds the ceiling of ${CEILING}. New user-facing text must go through t().\n` +
      `If you converted a view, lower CEILING in this script instead.`,
  );
  process.exit(1);
}
