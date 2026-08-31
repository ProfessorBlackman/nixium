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

## Which base the packages are built on, and why it matters

**The `.deb` and `.rpm` are built on Ubuntu 22.04, deliberately, and that is not an accident of
whichever runner was handy.**

glibc is forward-compatible only. A binary linked against 24.04's glibc 2.39 runs on 2.39 and newer and
on nothing older — and `SPEC.md` §7.1 makes **Ubuntu 22.04+** a Tier-1 target. A package built on 24.04
therefore installs cleanly on 22.04 and then fails at every launch:

```
nix: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found (required by nix)
```

Which is exactly what happened once. Building on the oldest supported target is the only arrangement
that works for all of them.

`libwebkit2gtk-4.1-dev` is available on jammy (2.50.4), so nothing is given up.

**The Rust cache is keyed to the distro release**, and has to be. `Swatinem/rust-cache` keys on
`runner.os`, which is `Linux` for both 22.04 and 24.04 — so without this the 24.04 jobs' cache restores
into the 22.04 one, and `target/` holds *executables*: a dependency's build script linked against glibc
2.39 then fails to run, producing the same error one layer down from where it was fixed.

## Why the deb's dependencies are computed after bundling

Tauri's bundler writes whatever `bundle.linux.deb.depends` says, plus the GTK and webkit packages it
knows it linked. It does **not** run `dpkg-shlibdeps` — so the package declares **no `libc6`
dependency at all**, which is why apt had no reason to refuse the broken package above.

`scripts/add-deb-depends.sh` runs after `tauri build`: it unpacks the deb, runs `dpkg-shlibdeps` over
every ELF binary it installs — the app *and* the helper, since a helper that cannot start is a set of
features that fail with no explanation — merges the result with the hand-declared `policykit-1 |
polkit`, and repacks. Versioned entries win over unversioned ones on the same package name, and nothing
already declared is dropped.

`check-bundle.sh` then asserts the result contains a versioned `libc6`, so a package built without that
step cannot be published. The rpm side needs no equivalent: rpmbuild generates ELF dependencies itself.

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
