# Packaging

Task 0.10 (`FND-9`). Four formats, with an honest note about what each can actually do.

| Format | Built by | Helper installed | Privileged actions |
| --- | --- | --- | --- |
| `.deb` | `tauri build --bundles deb` | `/usr/libexec/nix/nix-helper` | yes |
| `.rpm` | `tauri build --bundles rpm` | `/usr/libexec/nix/nix-helper` | yes |
| AppImage | `tauri build --bundles appimage` | bundled, not installed | **no** — see below |
| Flatpak | `flatpak-builder` with `flatpak/com.tlc.nix.yml` | not installed | **no** — see below |

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
```

The helper is under `libexec` rather than `bin` because it is not meant to be run by hand; its
`--serve` mode is the only useful entry point and it says so when invoked without it.

## Verifying a build locally

```sh
make build                 # release bundle for this host
NIX_HELPER_PATH=src-tauri/target/debug/nix-helper make dev
```

`NIX_HELPER_PATH` exists so a development build can find an uninstalled helper. In a packaged
install the default `/usr/libexec/nix/nix-helper` applies.
