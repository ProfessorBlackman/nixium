// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Bump the version in the three files that carry it.
 *
 * # Why this is a script and not a note in a README
 *
 * The version lives in `package.json`, `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml`, and it
 * must be identical in all three — the release tag comes from one, the binary's `--version` from
 * another, the deb's metadata from a third. Editing three files by hand is three chances to miss one,
 * and `scripts/check-version.mjs` exists because that is a real failure rather than a hypothetical.
 *
 * One command edits all three and then runs that check, so a partial bump cannot be committed.
 *
 * # Why it is not automatic
 *
 * The release workflow publishes when the version changes. Bumping on every push would therefore
 * publish on every push — a release per commit, which for a desktop application means an update
 * notification per commit. Deciding *that this set of changes is a release* is a judgement about the
 * changes, and nothing in the repository knows enough to make it.
 *
 * What a machine could decide is the *number*, given a commit convention that says which commits are
 * features and which are fixes. This project's commit messages are prose rather than
 * `feat:`/`fix:` prefixes, so that is a change to how commits are written, not a script — and it is
 * worth having only if the prefixes are enforced, since a convention nobody checks produces a version
 * number nobody can trust.
 *
 * Usage:
 *   node scripts/bump-version.mjs patch|minor|major
 *   node scripts/bump-version.mjs 1.2.3
 */
import { readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const requested = process.argv[2];

if (!requested) {
  console.error("usage: node scripts/bump-version.mjs patch|minor|major|<x.y.z>");
  process.exit(2);
}

/** The current version, and the assurance that the three files already agree. */
function current() {
  try {
    return execFileSync("node", [join(root, "scripts/check-version.mjs"), "--quiet"], {
      encoding: "utf8",
    }).trim();
  } catch (error) {
    // Bumping a disagreement would produce a different disagreement. Refuse and say so.
    console.error("the version does not currently agree across the three files — fix that first:\n");
    console.error(error.stdout ?? "");
    console.error(error.stderr ?? "");
    process.exit(1);
  }
}

const from = current();
const [major, minor, patch] = from.split("-")[0].split(".").map(Number);

const next = {
  major: `${major + 1}.0.0`,
  minor: `${major}.${minor + 1}.0`,
  patch: `${major}.${minor}.${patch + 1}`,
}[requested] ?? requested;

if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(next)) {
  console.error(`${next} is not a semantic version, and the release tag is built from it.`);
  process.exit(1);
}
if (next === from) {
  console.error(`already ${from} — the release workflow publishes only when the version changes.`);
  process.exit(1);
}

/**
 * Replace the version in one file, matching only the field that carries it.
 *
 * Anchored per file rather than a global search-and-replace: `0.1.0` appears in `Cargo.lock` for every
 * one of our own crates, in documentation, and in the odd dependency, and rewriting those would be
 * both wrong and hard to notice.
 */
function replace(file, pattern, build) {
  const path = join(root, file);
  const text = readFileSync(path, "utf8");
  const match = text.match(pattern);
  if (!match) {
    console.error(`could not find the version field in ${file}`);
    process.exit(1);
  }
  writeFileSync(path, text.replace(pattern, build(match)));
  console.log(`  ${file}`);
}

console.log(`${from} -> ${next}`);

replace("package.json", /"version":\s*"[^"]+"/, () => `"version": "${next}"`);
replace("src-tauri/tauri.conf.json", /"version":\s*"[^"]+"/, () => `"version": "${next}"`);

// The workspace version, which every crate inherits. Matched inside `[workspace.package]` so the
// dependency versions further down the file are untouched.
replace(
  "src-tauri/Cargo.toml",
  /(\[workspace\.package\][\s\S]*?\nversion = ")[^"]+(")/,
  (match) => `${match[1]}${next}${match[2]}`,
);

// Cargo.lock records our own crates' versions, and a stale lockfile fails `--locked` builds.
console.log("\nupdating Cargo.lock");
execFileSync("cargo", ["update", "--workspace", "--offline"], {
  cwd: join(root, "src-tauri"),
  stdio: "inherit",
});

// The point of the script: prove the three now agree before anyone commits.
const after = execFileSync("node", [join(root, "scripts/check-version.mjs")], { encoding: "utf8" });
process.stdout.write(`\n${after}`);
console.log(`\nNext: review the diff, commit, and push to master to publish v${next}.`);
