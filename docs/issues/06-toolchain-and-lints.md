# Toolchain and lints

The gates catching things, and the friction of being caught. Every entry here is the cheap kind of
defect — found at the keystroke, before any reasoning was needed.

---

## 1. `unsafe_code = "deny"` rejected a test that set an environment variable

**Phase 0** · **Friction** · **Found by** the lint, on its first day

`paths.rs` resolves XDG directories, so the obvious test sets `XDG_CACHE_HOME` and checks the result.
In edition 2024 `std::env::set_var` is `unsafe` — it is not thread-safe, and `cargo test` is threaded —
and the workspace denies `unsafe_code`.

The easy response would have been an `#[allow]` on the test.

**Resolved** by extracting the rules into a pure function instead:

```rust
fn resolve(var: Option<&OsStr>, home: Option<&Path>, fallback: &str) -> Option<PathBuf>
```

Six focused tests where there had been one, no process state mutated, and the XDG rule that a
non-absolute value must be ignored became directly testable — which it had not been before, because
setting a relative value in a shared environment is exactly the racy case.

Worth recording because the lint produced a better design on day one rather than merely blocking a
line. That is the argument for keeping it.

---

## 2. `clippy::result_large_err` — `AppError` was too big to return by value

**Phase 0** · **Friction** · **Found by** clippy

`AppError` carries a code, message, remedy, cause, context and path, reaching about 168 bytes. Clippy
warns above 128, because every `Result<T, AppError>` in the crate pays that cost on the success path
too.

**Resolved** by boxing the `cause` field, which is the largest and least often present.

**Guard.** `app_error_stays_small_enough_to_return_by_value` asserts `size_of::<AppError>() <= 128`,
and its failure message says what to do:

> AppError grew to {size} bytes; clippy::result_large_err warns above 128. Box the new field rather
> than raising the ceiling.

A size assertion without that instruction invites raising the number, which defeats the point.

---

## 3. `unreachable_pub` on Tauri command and state items

**Phase 0** · **Friction** · **Found by** the lint

Tauri's macros want `pub` items, but command handlers are not part of the library's API and
`unreachable_pub` says so.

**Resolved** with `pub(crate)`, which the macros accept.

---

## 4. The helper protocol version guard caught a stale binary three times

**M4, M5 ×2** · **Friction** · **Found by** the guard, working as designed

The helper is a separate binary. `cargo test` does not rebuild it, so after a protocol change the
tests run the *old* helper and the handshake version check fails. This happened at v1→v2, v2→v3 and
v3→v4.

Correct behaviour — a version mismatch is exactly what the check exists to catch, and it is far better
than a stale helper silently accepting a request it does not understand. But the first occurrence
presented as a panic on an `unwrap` with no indication of what to do.

**Resolved** by improving the failure message to say:

> run `make test`, `cargo test` does not rebuild the helper

The check itself was left alone.

**Guard.** `make test` builds the helper before running tests, so the documented path never hits it. It
is the guard for the times someone reaches for `cargo test` directly, which is often.

---

## 5. The pre-commit hook failed every documentation-only commit

**Documentation phase** · **Moderate** · **Found by** running the hook before committing this log

The hook is `set -euo pipefail`, and the licence check was written as one pipeline:

```bash
missing=$(staged | grep -E '\.(rs|ts|tsx|css)$' | grep -v '^src/bindings/' | while read -r f; do …)
```

If a commit stages no source files, the first `grep` matches nothing and exits 1. Under `pipefail`
that fails the pipeline, and `set -e` aborts the script — so the hook exited 1 with **no output at
all**, rejecting the commit without saying why.

Every commit so far has touched at least one `.rs` file, so it had never fired. The first
documentation-only commit was the one adding this log, which is a satisfying way to find it.

**Resolved** by tolerating an empty match and iterating over the captured list:

```bash
sources=$(staged | grep -E '\.(rs|ts|tsx|css)$' | grep -v '^src/bindings/' || true)
missing=$(printf '%s\n' "$sources" | while read -r f; do
    [ -n "$f" ] || continue
    …
```

The `|| true` is load-bearing and now says so in a comment, since it looks like the kind of thing
someone would tidy away.

**Guard.** Verified both directions rather than just the fix: the hook passes with only documentation
staged, and still rejects a deliberately header-less `.rs` file naming it in the output. The
equivalent CI job does not share the bug — it uses `git ls-files`, which always matches, and GitHub
Actions does not set `pipefail` by default.

Worth noting against [09-patterns.md §10](09-patterns.md): a gate that fails closed with no
explanation is only marginally better than no gate. The message matters as much as the check.

---

## 6. CI YAML, twice in one file

**Phase 0** · **Friction** · **Found by** CI refusing to parse the file

First a `steps` key missing its colon. Then, less obviously:

```yaml
run: cargo test -p nix-core --release --lib -- budget:: --nocapture
```

The `:: ` sequence — colon followed by space — is how YAML separates a key from a value, so the scanner
treated the middle of the command as a mapping and failed.

**Resolved** by quoting the value:

```yaml
run: "cargo test -p nix-core --release --lib -- budget:: --nocapture"
```

**Guard.** None beyond CI itself, which is the appropriate one: a YAML error cannot reach `master`
because the job that would run is the job that fails to parse.
