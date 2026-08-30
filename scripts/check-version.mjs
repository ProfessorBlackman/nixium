// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * The three places a version is written must agree.
 *
 * `package.json`, `src-tauri/tauri.conf.json` and the Cargo workspace each carry one, and nothing
 * compared them. They are used for different things — the tag and release title come from one, the
 * binary's `--version` from another, the package metadata in the `.deb` from a third — so a
 * disagreement does not fail a build. It ships a release whose tag says one thing and whose contents
 * say another, and the first person to notice is a user filing a bug against the wrong version.
 *
 * Prints the agreed version on success, so the release workflow can read it from here rather than
 * picking one of the three and hoping.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const sources = [
  {
    file: "package.json",
    version: JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version,
  },
  {
    file: "src-tauri/tauri.conf.json",
    version: JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8")).version,
  },
  {
    file: "src-tauri/Cargo.toml",
    // The workspace version, which every crate inherits with `version.workspace = true`.
    version: readFileSync(join(root, "src-tauri/Cargo.toml"), "utf8")
      .split("[workspace.package]")[1]
      ?.match(/^version = "([^"]+)"/m)?.[1],
  },
];

const missing = sources.filter((s) => !s.version);
if (missing.length > 0) {
  for (const s of missing) console.error(`FAIL no version found in ${s.file}`);
  process.exit(1);
}

const distinct = [...new Set(sources.map((s) => s.version))];
if (distinct.length > 1) {
  console.error("FAIL the version is not the same everywhere:");
  for (const s of sources) console.error(`  ${s.version.padEnd(12)} ${s.file}`);
  console.error("\nA release built from these would carry a tag that disagrees with its contents.");
  process.exit(1);
}

const version = distinct[0];
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`FAIL ${version} is not a semantic version, and the release tag is built from it.`);
  process.exit(1);
}

// Bare, on stdout, so a workflow can capture it.
if (process.argv.includes("--quiet")) {
  console.log(version);
} else {
  console.log(`version: ${version}, agreed across ${sources.length} files`);
}
