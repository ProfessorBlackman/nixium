#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Methuselah Nwodobeh
"""
Collect third-party attribution from the crates actually in the lockfile. `PLT-5`.

Apache-2.0 §4(d) requires that any NOTICE file a dependency ships be reproduced in distributions.
Most Rust crates carry none, so the honest way to find out is to look rather than to assume — which
is the whole point of this script existing instead of a sentence saying "probably empty".

Reads `Cargo.lock` and inspects each crate's **published `.crate` archive** in the registry cache,
collecting its NOTICE and licence files. Emits Markdown on stdout.

# Why the archives and not the extracted sources

The first version read `~/.cargo/registry/src`, which is where cargo *unpacks* a crate — and it
unpacks lazily, only what a build actually compiled. The output was therefore a function of what had
been built on that machine rather than of the lockfile. On a warm development machine all 504 crates
were present; on a clean CI runner 133 were not, most of them platform-specific crates for macOS and
Android that a Linux build never touches. The release refused to publish over the difference, which is
the check working — but the file it compares against should never have been machine-dependent.

`~/.cargo/registry/cache` holds the `.crate` archive for every entry in the lockfile, is populated by
`cargo fetch` regardless of target, and is content-addressed — so it is identical everywhere.

A crate found in neither place is now a **hard error**. The previous version noted it and carried on,
producing a plausible-looking file that was quietly missing attribution, which is the worst of the
three available behaviours.

Run: `make notices` (which fetches first), or:
`cd src-tauri && cargo fetch && cd .. && python3 scripts/collect-notices.py > THIRD-PARTY-NOTICES.md`
"""
import os
import re
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "src-tauri" / "Cargo.lock"

# Our own crates are not third-party.
OURS = {"nix-app", "nix-core", "nix-helper"}

NOTICE_NAMES = ("NOTICE", "NOTICE.md", "NOTICE.txt", "NOTICE-APACHE")


def cache_dirs():
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    cache = home / "registry" / "cache"
    return sorted(cache.glob("*")) if cache.exists() else []


def packages():
    if not LOCK.exists():
        sys.exit(f"no lockfile at {LOCK}")
    text = LOCK.read_text()
    found = re.findall(
        r'\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"(?:\nsource = "([^"]*)")?',
        text,
    )
    # Only packages from a registry: a path dependency is one of ours, and a git dependency would
    # need its own handling rather than being silently skipped.
    return [(n, v, s) for (n, v, s) in found if n not in OURS]


def find_archive(name, version):
    for base in cache_dirs():
        candidate = base / f"{name}-{version}.crate"
        if candidate.is_file():
            return candidate
    return None


def main():
    found_notices = []
    missing_source = []
    licences = {}
    total = 0

    for name, version, source in packages():
        total += 1
        if not source:
            # A path dependency that is not ours would be a vendored crate; there are none.
            continue
        archive = find_archive(name, version)
        if archive is None:
            missing_source.append(f"{name} {version}")
            continue

        # A `.crate` is a gzipped tar whose single top-level directory is `<name>-<version>`, so an
        # attribution file at the crate root is exactly two path components deep. Anything deeper is
        # source, not attribution.
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                if not member.isfile():
                    continue
                parts = member.name.split("/")
                if len(parts) != 2:
                    continue
                filename = parts[1]

                if filename in NOTICE_NAMES:
                    handle = tar.extractfile(member)
                    if handle is None:
                        continue
                    body = handle.read().decode("utf-8", errors="replace").strip()
                    if body:
                        found_notices.append((name, version, filename, body))

                upper = filename.upper()
                if upper.startswith(("LICENSE", "LICENCE", "COPYING")):
                    licences.setdefault(name, []).append(filename)

    if missing_source:
        # Refused rather than noted. A file that silently omits a crate's attribution is worse than no
        # file, because it looks like the question was asked and answered.
        print(
            f"{len(missing_source)} crates are not in the registry cache, so their notices could not "
            "be checked:",
            file=sys.stderr,
        )
        for entry in missing_source[:20]:
            print(f"  {entry}", file=sys.stderr)
        if len(missing_source) > 20:
            print(f"  …and {len(missing_source) - 20} more", file=sys.stderr)
        print("\nRun `cargo fetch` in src-tauri/ and try again.", file=sys.stderr)
        sys.exit(1)

    out = []
    out.append("<!-- SPDX-License-Identifier: GPL-3.0-or-later -->")
    out.append("<!-- Generated by scripts/collect-notices.py — do not edit by hand. -->")
    out.append("")
    out.append("# Third-party notices")
    out.append("")
    out.append(
        "nix is GPL-3.0-or-later. Its dependencies are permissive, which is compatible in that "
        "direction — but Apache-2.0 §4(d) requires that any `NOTICE` file a dependency ships be "
        "reproduced in distributions, so this file exists to satisfy that rather than to assume it "
        "empty."
    )
    out.append("")
    out.append(f"Generated from `src-tauri/Cargo.lock`: **{total} third-party crates**.")
    out.append("")


    out.append("## NOTICE files")
    out.append("")
    if found_notices:
        out.append(
            f"{len(found_notices)} of the {total} crates ship a `NOTICE`. Reproduced in full below, "
            "as §4(d) requires."
        )
        out.append("")
        for name, version, filename, body in sorted(found_notices):
            out.append(f"### {name} {version} — `{filename}`")
            out.append("")
            out.append("```")
            out.append(body)
            out.append("```")
            out.append("")
    else:
        out.append(
            "**None.** No crate in the dependency tree ships a `NOTICE` file, so §4(d) imposes no "
            "reproduction requirement. This was checked, not assumed — the check is "
            "`scripts/collect-notices.py`, and re-running it is how the claim stays true."
        )
        out.append("")

    out.append("## Licences")
    out.append("")
    out.append(
        f"{len(licences)} crates ship their licence text in-tree. The licences themselves are the "
        "standard MIT, Apache-2.0, BSD and Unicode texts; the copyright lines they carry are the part "
        "that varies, and are preserved in each crate's own source as distributed."
    )
    out.append("")
    for name in sorted(licences):
        out.append(f"- `{name}`: {', '.join(sorted(set(licences[name])))}")

    print("\n".join(out))


if __name__ == "__main__":
    main()
