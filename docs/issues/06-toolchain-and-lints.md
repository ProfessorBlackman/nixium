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

---

## 7. `gen_blocking` names the type without the suffix

**Phase 4** · **Friction** · **Found by** the compiler refusing what was written

`units.rs` declares its systemd interface with zbus's proxy macro, configured for the blocking API
because nix-core is std threads throughout (§D10):

```rust
#[zbus::proxy(interface = "org.freedesktop.systemd1.Manager", gen_blocking = true, gen_async = false)]
```

Every call site referred to `ManagerProxyBlocking`, by analogy with how zbus names the blocking variant
when it generates both. It does not exist. With `gen_async = false` there is only one proxy type, so it
takes the plain name — `ManagerProxy` — and the `Blocking` suffix appears only when the async type has
already claimed it.

**Resolved** by using the plain names. No behaviour was at stake; the cost was reading the macro's
expansion instead of guessing from the feature name.

**Guard.** The compiler, which is the right one — a missing type cannot ship.

---

## 8. Clippy's complex-type threshold, and what it was right about

**Phase 4** · **Friction** · **Found by** a gate in the toolchain

Three `-D warnings` errors on the SVC work. One was a `%` test that `is_multiple_of` now expresses
(available since 1.87, which the MSRV move for zbus had just made reachable — a small dividend). The
other two were `ListUnits`'s return type, which is D-Bus signature `a(ssssssouso)` and therefore a
ten-field tuple, written inline in both the proxy trait and the call sites.

The lint was making a better point than "this is long". A ten-wide positional tuple is read by counting,
and reordering two same-typed fields shifts every value by one with nothing to catch it. Extracted as
`UnitRow` with the field order named in its doc comment, and a `Changes` alias for the
`(type, file, destination)` triples that enable and disable report back:

```rust
/// `(name, description, load, active, sub, following, path, job id, job type, job path)` — the
/// signature `a(ssssssouso)` that `ListUnits` returns, one entry per unit.
type UnitRow = (String, String, String, String, String, String, OwnedObjectPath, u32, String, OwnedObjectPath);
```

**Guard.** Clippy, unchanged. Recorded because the lint's stated reason (verbosity) was not the reason
the change was worth making, and dismissing it on the stated reason would have been easy.

---

## 9. `--workspace` unifies features, so CI tested the helper's configuration nowhere

**Phase 4** · **Friction** · **Found by** reading generated output rather than trusting it compiled

The suspicion was the opposite of the finding, which is why it is written down.

Counting `make check`'s output after the SVC work: 728 tests, the same total as before, though
`units.rs` and `journal.rs` had added 29. The apparent explanation was that `cargo test --workspace`
does not build nix-core with `dbus` — the feature only `nix-app` enables — so every
`#[cfg(feature = "dbus")]` test was invisible and the whole systemd half untested. A `cargo test -p
nix-core --features dbus` line went into the `Makefile` and CI, with a confident comment.

Then the check that should have come first:

```
$ cargo test --workspace the_inventory_meets_its_budget
test units::tests::the_inventory_meets_its_budget ... ok

$ cargo build --workspace --message-format=json | ...
nix_core ['lib'] features= ['dbus', 'default']
```

One nix-core lib artifact, feature on. The resolver *does* unify — nix-app asks for `dbus`, so the
single shared rlib has it, and the gated tests had been running all along. The 29 new tests were
already in the 721; the total had moved and I had misread which number was which.

The real gap was the mirror image. Nothing in CI built nix-core **without** the feature, which is the
configuration `nix-helper` links. So zbus code that escaped its `#[cfg(feature = "dbus")]` gate would
compile and test clean under `--workspace`, and break only when someone built the helper on its own.

**Resolved** by adding the narrow, feature-off run instead of the feature-on one:

```make
	cd $(CARGO_DIR) && $(TEST_ENV) cargo test -p nix-core
```

717 tests feature-off, 721 feature-on — the difference is exactly the four gated tests, which is the
confirmation the first hypothesis never got. §D10 in `SPEC.md` was corrected at the same time: it had
credited the helper's zero `zbus` symbols to the resolver not unifying, when in a whole-workspace build
they are the linker discarding unreached code. The shipped helper is still isolated by construction —
`cargo build -p nix-helper` puts zbus nowhere in its graph at all — but a `--workspace` build cannot
demonstrate that, and the measurement had been taken there.

**It paid for itself immediately.** The first `cargo build -p nix-core` after adding it reported three
things the unified build cannot see: an unconditional `use crate::error::Cause` and two `pub(crate)`
functions, all three used only by the D-Bus code and therefore dead in exactly the configuration the
helper links. Warnings rather than errors, and invisible for as long as nothing compiled that
configuration — which is the shape of every defect in this file that took hours instead of minutes.

**Guard.** The feature-off run, in both `Makefile` and CI. Against
[09-patterns.md §12](09-patterns.md), the entry that matters here is a different one: the fix was
verified in the direction that would have caught the original mistake, by asking for one gated test
**by name** and by reading the build's own feature list, rather than by re-reading a total. A test count
that does not change is not evidence about which tests ran.

---

## 10. A generated file that depended on what had been built

**Phase 6** · **Moderate** · **Found by** the release workflow refusing to publish

`THIRD-PARTY-NOTICES.md` is generated from `Cargo.lock`, and the release workflow regenerates it and
refuses to publish if the result differs from what is committed — a release must ship attribution
built from the lockfile it was built with. On the first run it refused:

```
Error: THIRD-PARTY-NOTICES.md is out of date.
+> Some crates were not present in the local registry when this was generated…
+> - block2 0.6.2
+> - core-graphics 0.25.0
+> - embed_plist 1.2.2
-397 crates ship their licence text in-tree.
+334 crates ship their licence text in-tree.
```

The collector read `~/.cargo/registry/src`, which is where cargo **unpacks** a crate — and it unpacks
lazily, only what a build actually compiled. So the output was a function of what had been built on
that machine, not of the lockfile. This machine had all 504 crates extracted from months of building;
a clean CI runner had 371, the missing 133 being macOS and Android crates a Linux build never touches.

**What made it worse is that I had "verified" determinism.** The check I ran was:

```
$ python3 scripts/collect-notices.py > n1.md
$ python3 scripts/collect-notices.py > n2.md
$ diff -q n1.md n2.md && echo "notices: deterministic"
```

That proves the script is *repeatable on one machine with a warm cache*. It says nothing about
*reproducible across machines*, which is the property a committed generated file actually needs, and
the two are easy to conflate because the first sounds like it implies the second.

**Resolved** by reading the published `.crate` archives in `~/.cargo/registry/cache` instead. Those
exist for every entry in the lockfile, are populated by `cargo fetch` regardless of target, and are
content-addressed — so the output depends on the lockfile and nothing else. It also found **419**
crates carrying licence files rather than 397: the archive is the canonical published content, and the
extracted tree was missing files the glob had been looking for.

A crate in neither place is now a **hard error that writes nothing**. The previous version noted it and
carried on, producing a plausible-looking file quietly missing attribution — the worst of the three
available behaviours, and the one that would have shipped.

**Guard.** Verified the way the original should have been: by simulating the other machine rather than
re-running on this one. A `CARGO_HOME` pointing at a directory with the real cache symlinked in and an
**empty** `src` — exactly the CI runner's shape — produces byte-identical output. An empty cache exits
1 and writes zero bytes.

Worth putting beside [09-patterns.md §12](09-patterns.md). "Verify the guard fires" is not enough on
its own; the question is *which* property the verification establishes. Running a thing twice in the
same place tests repeatability. Reproducibility needs a different place, and constructing a fake one
took two commands.
