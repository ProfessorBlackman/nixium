# Common tasks. `make check` is what CI runs; run it before pushing.
SHELL := /bin/bash
CARGO_DIR := src-tauri

.PHONY: help check fmt fmt-check clippy test typecheck bindings perf helper dev build hooks \
	install-helper uninstall-helper helper-smoke timer-status timer-run

help:
	@grep -E '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t22

check: fmt-check clippy test typecheck ## Everything CI checks

fmt: ## Format Rust sources
	cd $(CARGO_DIR) && cargo fmt --all

fmt-check: ## Fail if Rust sources are unformatted
	cd $(CARGO_DIR) && cargo fmt --all --check

clippy: ## Lint the whole workspace, warnings are errors
	cd $(CARGO_DIR) && cargo clippy --workspace --all-targets -- -D warnings

test: helper ## Run the Rust test suite
	cd $(CARGO_DIR) && cargo test --workspace

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

typecheck: ## Type-check the frontend
	pnpm tsc --noEmit

dev: ## Run the app in development
	pnpm tauri dev

build: ## Build a release bundle
	pnpm tauri build

hooks: ## Point git at the repo's hooks
	git config core.hooksPath .githooks
	@echo "hooks installed: $$(ls .githooks | tr '\n' ' ')"
