# Common tasks. `make check` is what CI runs; run it before pushing.
SHELL := /bin/bash
CARGO_DIR := src-tauri

.PHONY: help check fmt fmt-check clippy test typecheck a11y i18n version bump icons notices bindings perf helper dev build hooks \
	install-helper uninstall-helper helper-smoke timer-status timer-run

help:
	@grep -E '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t22

check: fmt-check clippy test typecheck a11y i18n version ## Everything CI checks

fmt: ## Format Rust sources
	cd $(CARGO_DIR) && cargo fmt --all

fmt-check: ## Fail if Rust sources are unformatted
	cd $(CARGO_DIR) && cargo fmt --all --check

clippy: ## Lint the whole workspace, warnings are errors
	cd $(CARGO_DIR) && cargo clippy --workspace --all-targets -- -D warnings

# The test suite must be safe to run on a machine where the helper is actually installed.
#
# It was not. A unit test escalated through polkit — silently, because auth_admin_keep had cached an
# earlier authorisation — and removed a real kernel. `Elevation` no longer has a `Default`, so no test
# can ask for escalation by accident, and no fixture names a package that exists. This is the third
# layer: point the client at a path that cannot exist, so even a test that *did* ask to escalate finds
# nothing to launch.
#
# It is belt and braces, not the mechanism. If this line is what saves you, two guards have already
# failed.
TEST_ENV := NIX_HELPER_PATH=/nonexistent/nix-helper-must-not-be-found

test: helper ## Run the Rust test suite
	cd $(CARGO_DIR) && $(TEST_ENV) cargo test --workspace
	# nix-core again, alone, *without* `dbus` — the configuration the privileged helper links.
	#
	# `--workspace` builds nix-app, which enables `nix-core/dbus`, and the resolver unifies that into
	# every copy of nix-core in the invocation. So the run above tests the feature-on build only, and
	# nothing else here compiles nix-core feature-off except `make helper`. Without this line, code
	# that needs zbus but isn't behind `#[cfg(feature = "dbus")]` passes CI and breaks the helper (D10).
	cd $(CARGO_DIR) && $(TEST_ENV) cargo test -p nix-core

helper: ## Build the helper binary (the client's integration tests spawn it)
	cd $(CARGO_DIR) && cargo build -p nix-helper

# ---- Exercising the privileged paths -------------------------------------------------------------
#
# These are the two paths the test suite cannot reach, because both need real root and one of them
# installs a daily job. Everything here is reversible with `uninstall-helper`.
#
# polkit authorises by **absolute path**: the action's exec.path annotation names
# /usr/libexec/nix/nix-helper, so the helper has to actually be there for the real prompt — the one
# with nix's own wording and `auth_admin_keep`'s one-prompt-per-session — to be what you see. Pointing
# NIX_HELPER_PATH at a build directory still works, but falls back to polkit's generic action.

HELPER_DIR := /usr/libexec/nix
POLICY_DIR := /usr/share/polkit-1/actions

install-helper: helper ## Install the helper and its polkit policy (needs sudo). Reversible.
	@echo "Installing the helper to $(HELPER_DIR) and its policy to $(POLICY_DIR)."
	@echo "Both are removed again by 'make uninstall-helper'."
	sudo install -d -m 755 $(HELPER_DIR)
	sudo install -m 755 -o root -g root $(CARGO_DIR)/target/debug/nix-helper $(HELPER_DIR)/nix-helper
	sudo install -m 644 -o root -g root packaging/polkit/com.tlc.nix.policy $(POLICY_DIR)/com.tlc.nix.policy
	@echo
	@echo "Installed. The helper's protocol version must match this build, so re-run this after"
	@echo "changing the helper. Now try: make helper-smoke"

uninstall-helper: ## Remove the helper and its polkit policy
	sudo rm -f $(HELPER_DIR)/nix-helper $(POLICY_DIR)/com.tlc.nix.policy
	sudo rmdir --ignore-fail-on-non-empty $(HELPER_DIR) 2>/dev/null || true
	@echo "Removed."

helper-smoke: ## Prove the pkexec path end to end: authenticate, handshake, one read, one refusal
	@echo "This asks for your password once. It starts the helper under pkexec, completes the version"
	@echo "handshake, reads an allow-listed file, and checks /etc/shadow is refused."
	@echo
	$(CARGO_DIR)/target/debug/nix helper-probe

timer-status: ## What the growth-history timer is doing, if installed
	@systemctl --user status nix-snapshot.timer --no-pager 2>&1 | head -12 || true
	@echo "--- next run ---"
	@systemctl --user list-timers nix-snapshot.timer --no-pager 2>&1 | head -4 || true

timer-run: ## Run the snapshot job now, as systemd would, without waiting for tomorrow
	systemctl --user start nix-snapshot.service
	@systemctl --user status nix-snapshot.service --no-pager 2>&1 | head -12 || true

bindings: ## Regenerate the TypeScript types from the Rust definitions
	cd $(CARGO_DIR) && cargo test -p nix-core --lib export_bindings
	@echo "bindings written to src/bindings/"

perf: ## Measure the performance budgets (release mode)
	cd $(CARGO_DIR) && NIX_PERF=1 cargo test -p nix-core --release --lib -- budget:: --nocapture

a11y: ## Accessibility checks: WCAG AA contrast, and every control named (PLT-2)
	node scripts/check-contrast.mjs
	node scripts/check-labels.mjs

i18n: ## Translatable-string ratchet: the count must not go up (PLT-1)
	node scripts/check-i18n.mjs

version: ## Facts written twice must agree: the version in three files, the repository URL in two
	node scripts/check-version.mjs

icons: ## Regenerate the application icons and the sidebar logo from assets/nix-mascot.png
	# One recipe, one shell, so the temporary directory is created and removed here rather than on
	# every `make` invocation — which is what a top-level mktemp variable would do, since make expands
	# those whatever target you asked for.
	#
	# The black square becomes transparent first: an opaque square reads as a black box on a light
	# panel, so the artwork's framing becomes a defect once it is 32 pixels wide. A flood fill from a
	# corner, not a colour threshold — the sunglasses are rgb(18,13,10) and the background is
	# rgb(16,15,14), so any threshold dark enough to remove one removes the other. See assets/README.md.
	#
	# The sidebar logo comes out of the same masked source rather than being filled again: the sidebar
	# and the launcher showing subtly different artwork would be a bug nobody would think to look for.
	# 128px for a mark drawn at 28 — enough for a 2x display and 29 kB on disk.
	@set -eu; \
	scratch="$$(mktemp -d)"; \
	trap 'rm -rf "$$scratch"' EXIT; \
	convert assets/nix-mascot.png -alpha set -fuzz 12% -fill none \
		-draw "color 0,0 floodfill" -resize 1024x1024 \
		-background none -gravity center -extent 1024x1024 "PNG32:$$scratch/icon-source.png"; \
	convert "$$scratch/icon-source.png" -resize 128x128 "PNG32:public/nix-logo.png"; \
	npx tauri icon "$$scratch/icon-source.png"; \
	rm -rf src-tauri/icons/android src-tauri/icons/ios
	@echo "Regenerated, and the mobile icon sets removed — nix is Linux-only."
	@echo "Review with: git diff --stat src-tauri/icons/ public/nix-logo.png"

bump: ## Bump the version everywhere: make bump BUMP=patch|minor|major, or BUMP=1.2.3
	@test -n "$(BUMP)" || { echo "usage: make bump BUMP=patch|minor|major|x.y.z"; exit 2; }
	node scripts/bump-version.mjs $(BUMP)

notices: ## Regenerate third-party attribution from the lockfile (PLT-5)
	# Fetch first: the collector reads the `.crate` archives, and a crate the cache has never seen is a
	# crate whose notice cannot be checked — which it treats as an error rather than an omission.
	cd $(CARGO_DIR) && cargo fetch
	python3 scripts/collect-notices.py > THIRD-PARTY-NOTICES.md
	@echo "THIRD-PARTY-NOTICES.md regenerated — commit it if it changed."

typecheck: ## Type-check the frontend
	pnpm tsc --noEmit

dev: ## Run the app in development
	pnpm tauri dev

build: ## Build a release bundle
	pnpm tauri build

hooks: ## Point git at the repo's hooks
	git config core.hooksPath .githooks
	@echo "hooks installed: $$(ls .githooks | tr '\n' ' ')"
