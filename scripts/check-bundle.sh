#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Methuselah Nwodobeh
#
# Verify a built package actually contains what it is supposed to. `PLT-5`.
#
# `PLT-5`'s acceptance criterion is that each artefact *installs and runs*, and until now CI built the
# bundles and uploaded them without ever opening one. A build that succeeds and produces a package
# missing its polkit policy is a build that succeeds.
#
# The check that matters most is the last one: polkit authorises by **absolute executable path**, so
# the path annotated in the policy and the path the package installs the helper to must agree. They
# are declared in two different files — `packaging/polkit/com.tlc.nix.policy` and the bundle config in
# `tauri.conf.json` — and nothing else would notice them drifting apart. The symptom would be every
# privileged action failing authorisation at run time, on a user's machine, with no build error
# anywhere.
#
# Usage: scripts/check-bundle.sh path/to/package.deb

set -euo pipefail

deb="${1:-}"
if [[ -z "$deb" || ! -f "$deb" ]]; then
  echo "usage: $0 <path to .deb>" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

dpkg-deb --extract "$deb" "$work"
dpkg-deb --info "$deb" > "$work/.control"

fail=0
check() {
  local path="$1" what="$2"
  if [[ -e "$work$path" ]]; then
    echo "  ok   $path — $what"
  else
    echo "  FAIL $path missing — $what" >&2
    fail=1
  fi
}

echo "contents of $(basename "$deb"):"
check /usr/bin/nix "the application"
check /usr/libexec/nix/nix-helper "the privileged helper"
check /usr/share/polkit-1/actions/com.tlc.nix.policy "the polkit policy"
check /usr/share/applications/com.tlc.nix.desktop "the desktop entry"
check /usr/share/doc/nix/THIRD-PARTY-NOTICES.md "third-party attribution (Apache-2.0 §4(d))"

# Modes. The helper is launched through pkexec, so it must not be setuid — that would be a binary
# authorising itself rather than being authorised.
helper="$work/usr/libexec/nix/nix-helper"
if [[ -e "$helper" ]]; then
  mode="$(stat -c '%a' "$helper")"
  if [[ "$mode" == "755" ]]; then
    echo "  ok   helper mode 755"
  else
    echo "  FAIL helper mode is $mode, expected 755" >&2
    fail=1
  fi
  if [[ -u "$helper" ]]; then
    echo "  FAIL the helper is setuid — pkexec performs the authorisation, not the binary" >&2
    fail=1
  else
    echo "  ok   helper is not setuid"
  fi
fi

# The desktop entry must be valid, or the launcher silently does not appear.
desktop="$work/usr/share/applications/com.tlc.nix.desktop"
if [[ -e "$desktop" ]] && command -v desktop-file-validate >/dev/null; then
  if desktop-file-validate "$desktop"; then
    echo "  ok   desktop entry validates"
  else
    echo "  FAIL desktop entry does not validate" >&2
    fail=1
  fi
fi

# polkit depends on this dependency being declared: without it the helper cannot be authorised and
# every privileged feature reports itself unavailable.
if grep -qiE '^ Depends:.*polkit' "$work/.control"; then
  echo "  ok   declares a polkit dependency"
else
  echo "  FAIL no polkit dependency declared" >&2
  fail=1
fi

# The one that would otherwise fail only at run time, on a user's machine.
policy="$work/usr/share/polkit-1/actions/com.tlc.nix.policy"
if [[ -e "$policy" ]]; then
  annotated="$(grep -oP '(?<=<annotate key="org.freedesktop.policykit.exec.path">)[^<]+' "$policy" || true)"
  if [[ -z "$annotated" ]]; then
    echo "  FAIL the policy annotates no executable path" >&2
    fail=1
  elif [[ -e "$work$annotated" ]]; then
    echo "  ok   policy annotates $annotated, which the package installs"
  else
    echo "  FAIL the policy annotates $annotated, which this package does not install" >&2
    echo "       polkit authorises by absolute path, so every privileged action would be refused" >&2
    fail=1
  fi
fi

if (( fail )); then
  echo >&2
  echo "the package is missing something it needs to work once installed" >&2
  exit 1
fi
echo "bundle: every expected file is present, with the right mode, and the policy path agrees"
