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
 * From 361 to 89 in one pass, most of it mechanical. What is left is the part a script should not
 * touch: prose interleaved with `<code>` elements, and ternaries where only a human can tell a
 * rendered label from a state value. An earlier attempt at those wrote `setStage(t("confirming"))`,
 * which would have broken the reclaim flow outright — a regex cannot distinguish the two.
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
const CEILING = 78;

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

    /*
     * Strip comments — all of them, not just the module header.
     *
     * An earlier version stripped only the first block comment, so prose in every *other* comment was
     * counted as untranslated interface text. Documenting a component therefore raised its score,
     * which is a perverse incentive in a ratchet whose whole job is to fall. It surfaced when adding
     * grouped navigation: three of the four "untranslated strings" it reported were sentences from the
     * comment explaining the grouping.
     *
     * Whole-line `//` comments only. A trailing `// …` cannot be stripped by pattern without also
     * eating the `//` in a `https://` inside a string.
     */
    let body = text
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/^\s*\/\/[^\n]*$/gm, "");

    /*
     * Strip top-level `const` tables when the file translates computed values.
     *
     * A label table holds its English *source* and is translated where it is rendered — `t(v.title)`,
     * `t(SAFETY_LABEL[item.safety])` — because a module-level `t()` runs once at load and freezes
     * whichever language was active then. That is correct, and indistinguishable from an untranslated
     * literal by looking at the declaration.
     *
     * The signal is a `t(` call whose argument is not a string literal: it means this file translates
     * something computed, so its tables are the source for those calls. Crude, and it can hide a
     * genuinely untranslated table in a file that also does this — which is the trade for the number
     * meaning something. Before this, wrapping a hundred strings moved the count by zero.
     */
    if (/\bt\(\s*[^"'`)\s]/.test(body)) {
      body = body.replace(/^const [A-Z][A-Z_0-9]*[^=]*=\s*[[{][\s\S]*?^(?:\]|\})[;,]?$/gm, "");
    }

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
