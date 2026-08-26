# Common tasks. `make check` is what CI runs; run it before pushing.
SHELL := /bin/bash
CARGO_DIR := src-tauri

.PHONY: help check fmt fmt-check clippy test typecheck bindings perf helper dev build hooks

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

bindings: ## Regenerate the TypeScript types from the Rust definitions
	cd $(CARGO_DIR) && cargo test -p nix-core --lib
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
