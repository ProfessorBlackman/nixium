// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

/**
 * Facts that are written in more than one place must agree.
 *
 * # The version
 *
 * `package.json`, `src-tauri/tauri.conf.json` and the Cargo workspace each carry one, and nothing
 * compared them. They are used for different things — the tag and release title come from one, the
 * binary's `--version` from another, the package metadata in the `.deb` from a third — so a
 * disagreement does not fail a build. It ships a release whose tag says one thing and whose contents
 * say another, and the first person to notice is a user filing a bug against the wrong version.
 *
 * Prints the agreed version on success, so the release workflow can read it from here rather than
 * picking one of the three and hoping.
 *
 * # The repository URL
 *
 * The same failure, one fact over, and it had already happened: `[workspace.package]` named a
 * repository this project does not live in, while the About view held its own copy of the real one.
 * Nothing compared them because nothing looked at either.
 *
 * The view no longer carries a URL at all — `commands::versions` hands it Cargo's value at runtime —
 * which leaves `site/index.html` as the last duplicate. A static page cannot read Cargo, so it is
 * checked here instead: every GitHub repository it links to must be the one the manifest names.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const cargo = readFileSync(join(root, "src-tauri/Cargo.toml"), "utf8");
/** The `[workspace.package]` block, which every crate inherits from. */
const workspace = cargo.split("[workspace.package]")[1] ?? "";

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
    version: workspace.match(/^version = "([^"]+)"/m)?.[1],
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

const repository = workspace.match(/^repository = "([^"]+)"/m)?.[1];
if (!repository) {
  console.error("FAIL no repository in [workspace.package] of src-tauri/Cargo.toml");
  console.error(
    "  The About view builds its 'Raise an issue' link from it, and an absent one removes the link\n" +
      "  silently rather than failing anything — see commands::versions.",
  );
  process.exit(1);
}

/*
 * Every GitHub repository the landing page links to.
 *
 * Matched down to `owner/repo`, so the deeper links — `/releases`, `/issues`,
 * `/blob/master/docs/...` — all reduce to the repository they are under and are compared once. If a
 * third-party repository is ever linked from that page, this is where it will show up, and it will
 * need an allow-list rather than a looser pattern: a check that cannot tell "our repo moved" from
 * "someone linked elsewhere" is not checking anything.
 */
const page = readFileSync(join(root, "site/index.html"), "utf8");
const linked = [
  ...new Set(
    [...page.matchAll(/https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+/g)].map((m) => m[0]),
  ),
];
const disagree = linked.filter((url) => url !== repository);
if (disagree.length > 0) {
  console.error(`FAIL site/index.html links a repository the manifest does not name:`);
  for (const url of disagree) console.error(`  ${url}`);
  console.error(`  the manifest says: ${repository}`);
  console.error("\nA landing page that points somewhere the project does not live sends every");
  console.error("reader, and every bug report, to the wrong place.");
  process.exit(1);
}

// Bare, on stdout, so a workflow can capture it.
if (process.argv.includes("--quiet")) {
  console.log(version);
} else {
  console.log(`version: ${version}, agreed across ${sources.length} files`);
  console.log(`repository: ${repository}, agreed by ${linked.length} link(s) in site/index.html`);
}
