// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Every form control has an accessible name. `PLT-2`.
 *
 * An unlabelled checkbox is announced as "checkbox, unchecked" and nothing else, which in a tool whose
 * checkboxes select packages for removal is not a cosmetic problem.
 *
 * # This is a heuristic, and says so
 *
 * It is a text scan, not a JSX parser. A control counts as named if it carries `aria-label`,
 * `aria-labelledby`, or `id` (paired with a `<label htmlFor>`), **or** if a `<label` opens within the
 * few lines above it — which is how this codebase labels most things:
 *
 * ```jsx
 * <label className="field">
 *   <span>Filter</span>
 *   <input value={filter} … />
 * </label>
 * ```
 *
 * That pattern gives a real accessible name, because a wrapping `<label>` labels the control inside it.
 * The scan can be fooled — a `<label>` that closes before the input would pass — so it is a floor, not
 * a proof. A floor that fails the build is still worth more than an intention, and the cases it cannot
 * see are the ones a human review has to catch.
 */
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const roots = [join(here, "..", "src", "views"), join(here, "..", "src", "components")];

/** How far above a control a wrapping `<label>` may open. */
const LABEL_WINDOW = 6;

/** Types that carry their own name from their value, so a label is not required. */
const SELF_LABELLING = new Set(["submit", "reset", "button"]);

let problems = 0;
let checked = 0;

for (const root of roots) {
  for (const name of readdirSync(root).filter((f) => f.endsWith(".tsx"))) {
    const path = join(root, name);
    const lines = readFileSync(path, "utf8").split("\n");

    for (let i = 0; i < lines.length; i += 1) {
      const line = lines[i];
      const match = line.match(/<(input|select|textarea)\b/);
      if (!match) continue;

      // The tag may span several lines; take up to the closing `>` or `/>`.
      let tag = "";
      for (let j = i; j < Math.min(i + 12, lines.length); j += 1) {
        tag += lines[j];
        if (lines[j].includes("/>") || lines[j].trimEnd().endsWith(">")) break;
      }

      const type = tag.match(/type=["{]?([a-z]+)/)?.[1];
      if (type && SELF_LABELLING.has(type)) continue;
      checked += 1;

      const named =
        /aria-label[=}]/.test(tag) ||
        /aria-labelledby/.test(tag) ||
        /\bid=/.test(tag) ||
        lines
          .slice(Math.max(0, i - LABEL_WINDOW), i)
          .some((above) => /<label\b/.test(above));

      if (!named) {
        problems += 1;
        console.error(
          `FAIL ${name}:${i + 1} — <${match[1]}> has no accessible name\n` +
            `       ${line.trim()}\n` +
            `       Wrap it in a <label>, or give it aria-label.`,
        );
      }
    }
  }
}

if (problems > 0) {
  console.error(`\n${problems} of ${checked} controls have no accessible name.`);
  process.exit(1);
}

console.log(`labels: ${checked} form controls have an accessible name`);
