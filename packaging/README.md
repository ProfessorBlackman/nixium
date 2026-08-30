# Packaging

Task 0.10 (`FND-9`). Four formats, with an honest note about what each can actually do.

| Format | Built by | Helper installed | Privileged actions |
| --- | --- | --- | --- |
| `.deb` | `tauri build --bundles deb` | `/usr/libexec/nix/nix-helper` | yes |
| `.rpm` | `tauri build --bundles rpm` | `/usr/libexec/nix/nix-helper` | yes |
| AppImage | `tauri build --bundles appimage` | bundled, not installed | **no** — see below |
| Flatpak | `flatpak-builder` with `flatpak/com.tlc.nix.yml` | not installed | **no** — see below |

## Building a deb locally

```sh
cd src-tauri && cargo build -p nix-helper --release && cd ..
pnpm tauri build --bundles deb
./scripts/check-bundle.sh src-tauri/target/release/bundle/deb/*.deb
```

**The helper must be built first.** `tauri.conf.json` copies it out of `target/release`, so without
that step the bundle fails — after a full release compile — with `"target/release/nix-helper" does not
exist`. CI has a separate step for it; locally it is easy to forget, which is how this note came to be
written.

`check-bundle.sh` is the check that matters. It opens the built package and asserts, among other
things, that **the path the polkit policy annotates is a path the package installs**. Those two facts
live in different files, nothing else compares them, and if they drift the symptom is every privileged
action failing authorisation at run time on a user's machine, with no build error anywhere.

## Why AppImage and Flatpak are read-only for now

Both are relocatable, and polkit authorises by **absolute executable path**: the policy file in
`polkit/com.tlc.nix.policy` annotates `/usr/libexec/nix/nix-helper`. An AppImage mounts itself at a
different path on every run, and a Flatpak's helper lives inside the sandbox — so neither can
satisfy that annotation.

This is a real limitation, not an oversight, and the app degrades honestly: without a helper the
capability probe reports `pkexec` unavailable and privileged features are unavailable with a stated
reason, rather than failing silently at the point of use.

Resolving it properly means shipping the helper and its policy as a separate host-installed
package. That belongs with M9 (PLT-5), not Phase 0.

## What the deb and rpm install

```
/usr/bin/nix                                    the application
/usr/libexec/nix/nix-helper                     the privileged helper
/usr/share/polkit-1/actions/com.tlc.nix.policy  the polkit action
/usr/share/applications/com.tlc.nix.desktop     the desktop entry
/usr/share/icons/hicolor/*/apps/nix.png         icons
/usr/share/doc/nix/copyright                    the licence  (deb)
/usr/share/licenses/nix/LICENSE                 the licence  (rpm)
```

Tauri has no licence field of its own, so the copyright file is placed through the same `files` map
that installs the helper — at the path each packaging convention expects.

## Outstanding for a public release

**Dependency attribution — done.** nix is GPL-3.0-or-later and its dependencies are permissive, which
is compatible in that direction, but Apache-2.0 §4(d) requires that any `NOTICE` file a dependency
ships be reproduced in distributions.

`scripts/collect-notices.py` reads `Cargo.lock`, finds each crate in the local registry and collects
its `NOTICE` and licence files into `THIRD-PARTY-NOTICES.md`, which the deb and rpm install to
`/usr/share/doc/nix/`. **504 third-party crates, and none ships a `NOTICE`** — so §4(d) imposes no
reproduction requirement here.

That was checked rather than assumed, which is the point of the script existing instead of a sentence
saying "probably empty". And the check was itself verified: planting a `NOTICE` in a crate's source
makes the collector reproduce it in full, and removing it makes the file report none again. Confirmed
independently with `find`: 821 licence files across the registry, zero notices.

Regenerate before a release — the script says so in the file when a crate is missing from the local
registry, since an unfetched crate is one whose notice was not checked.

The helper is under `libexec` rather than `bin` because it is not meant to be run by hand; its
`--serve` mode is the only useful entry point and it says so when invoked without it.

## Releasing

`.github/workflows/release.yml`, on every push to `master`. A release is only *published* when the
version has changed: the workflow reads it from the tree and skips if `v<version>` is already tagged.

That direction matters. Releasing on every push would produce a release per commit; requiring a
hand-pushed tag would let the tag and the version disagree. Deciding from the version in the tree is
the only arrangement in which the tag cannot lie — and `scripts/check-version.mjs` enforces that the
three files carrying a version agree before anything is built.

To release: bump the version in `package.json`, `src-tauri/tauri.conf.json` and
`src-tauri/Cargo.toml`, run `make notices` if the lockfile moved, and push to `master`.

Before publishing, the workflow re-runs the full gate on those exact sources, builds the three
bundles, opens the `.deb` with `scripts/check-bundle.sh`, installs it on a clean runner and asks the
installed binary for its version. A build that succeeds and a package that works are different claims.

## Verifying a build locally

```sh
make build                 # release bundle for this host
NIX_HELPER_PATH=src-tauri/target/debug/nix-helper make dev
```

`NIX_HELPER_PATH` exists so a development build can find an uninstalled helper. In a packaged
install the default `/usr/libexec/nix/nix-helper` applies.
