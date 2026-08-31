#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Methuselah Nwodobeh
#
# Declare the shared libraries the binaries actually need, so a package that cannot run refuses to
# install. `PLT-5`.
#
# # Why this is not Tauri's job and has to be done here
#
# Tauri's deb bundler writes whatever `bundle.linux.deb.depends` says in `tauri.conf.json`, plus the
# GTK and webkit packages it knows it linked. It does **not** run `dpkg-shlibdeps`, so the package
# declares no `libc6` dependency at all — and glibc is the one dependency that matters most, because a
# binary built against a newer glibc than the target has cannot run and there is no way to fix that
# after installation.
#
# The symptom without this: `apt install` succeeds, the launcher does nothing, and the terminal says
#
#     nix: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found (required by nix)
#
# apt had no reason to refuse, because the package never said it needed 2.39.
#
# `dpkg-shlibdeps` resolves each binary's dynamic symbols against the *build machine's* library
# packages, which is exactly the right answer: it produces the minimum versions that satisfy what was
# actually linked. It is also why the build base matters — see the `bundle` job's runner.
#
# Usage: scripts/add-deb-depends.sh path/to/package.deb

set -euo pipefail

deb="${1:-}"
if [[ -z "$deb" || ! -f "$deb" ]]; then
  echo "usage: $0 <path to .deb>" >&2
  exit 2
fi

for tool in dpkg-deb dpkg-shlibdeps; do
  if ! command -v "$tool" >/dev/null; then
    echo "$0: $tool is not installed (apt install dpkg-dev)" >&2
    exit 1
  fi
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# `-R` keeps DEBIAN/ alongside the payload, which is what lets the control file be rewritten and the
# whole thing repacked.
dpkg-deb -R "$deb" "$work/pkg"

# dpkg-shlibdeps insists on a debian/control naming the package it is analysing.
mkdir -p "$work/pkg/debian"
{
  echo "Source: nix"
  echo
  echo "Package: nix"
  echo "Architecture: amd64"
  echo 'Depends: ${shlibs:Depends}'
  echo "Description: placeholder for dpkg-shlibdeps"
} > "$work/pkg/debian/control"

# Every ELF the package installs, not just the app: the helper is a separate binary that runs as root,
# and a helper that cannot start is a set of features that fail with no explanation.
mapfile -t binaries < <(
  find "$work/pkg/usr" -type f -perm -u+x -exec sh -c 'file -b "$1" | grep -q "^ELF" && echo "$1"' _ {} \;
)
if (( ${#binaries[@]} == 0 )); then
  echo "$0: found no ELF binaries in the package, which cannot be right" >&2
  exit 1
fi
echo "analysing ${#binaries[@]} binaries: $(basename -a "${binaries[@]}" | tr '\n' ' ')"

computed="$(cd "$work/pkg" && dpkg-shlibdeps -O --ignore-missing-info "${binaries[@]}" 2>/dev/null | sed 's/^shlibs:Depends=//')"
if [[ -z "$computed" ]]; then
  echo "$0: dpkg-shlibdeps produced nothing, so no dependencies would be declared" >&2
  exit 1
fi

# glibc is the whole point. If it is absent the analysis did not work, and shipping the package would
# reintroduce exactly the failure this script exists to prevent.
if [[ "$computed" != *libc6* ]]; then
  echo "$0: the computed dependencies name no libc6, which means the check did not work:" >&2
  echo "  $computed" >&2
  exit 1
fi

existing="$(grep -oP '^Depends: \K.*' "$work/pkg/DEBIAN/control" || true)"
if [[ -n "$existing" ]]; then
  merged="$existing, $computed"
else
  merged="$computed"
fi

# Rewrite in place. Kept as one line, which is what dpkg expects.
python3 - "$work/pkg/DEBIAN/control" "$merged" <<'PY'
import sys

path, depends = sys.argv[1], sys.argv[2]
lines = open(path).read().split("\n")

# Deduplicate while preserving order: Tauri's own list and dpkg-shlibdeps overlap on gtk and webkit,
# and the *versioned* one is the more useful of the two, so a later entry wins.
seen = {}
for item in (d.strip() for d in depends.split(",")):
    if not item:
        continue
    name = item.split()[0]
    seen[name] = item

out = []
replaced = False
for line in lines:
    if line.startswith("Depends:"):
        out.append("Depends: " + ", ".join(seen.values()))
        replaced = True
    else:
        out.append(line)
if not replaced:
    # No Depends field at all: insert one after Architecture, where dpkg conventionally puts it.
    for index, line in enumerate(out):
        if line.startswith("Architecture:"):
            out.insert(index + 1, "Depends: " + ", ".join(seen.values()))
            break

open(path, "w").write("\n".join(out))
print("Depends: " + ", ".join(seen.values()))
PY

dpkg-deb -b "$work/pkg" "$deb" >/dev/null
echo "rewrote $(basename "$deb")"
