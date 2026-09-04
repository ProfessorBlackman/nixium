# Tests that were wrong

Three tests that passed, or failed, for reasons unrelated to what they claimed to check. Kept separate
because a test asserting the wrong thing is worse than no test: it reads as coverage.

---

## 1. A test that scanned its own list of banned words

**M4** · **Moderate** · **Found by** the test passing when it should not have

`packages.rs` must detect package managers by **capability** — is `apt-get` on `PATH`? — and never by
distribution, because Stacer probed `PATH` in a fixed order and so reported the wrong manager on any
system with two installed.

To hold that line, a test reads the module's own source with `include_str!` and fails if the
implementation mentions `ubuntu`, `fedora`, `debian`, `arch linux`, `opensuse` or `os-release`.

The test read the **whole file** — including its own banned-word list, and including documentation
that legitimately discusses which distributions ship which manager. So it had to be written to pass in
the presence of those words, which meant it could not fail on a real occurrence either.

**Resolved** by narrowing the scan to the implementation and excluding comments:

```rust
let implementation = source.split("#[cfg(test)]").next().unwrap_or(source);
for line in implementation.lines().filter(|l| {
    let t = l.trim_start();
    !t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!")
}) { … }
```

A test that reads source has to exclude itself, and the fact that it needs to is a hint that source
inspection is a blunt instrument. It is kept because the property it protects is a real one that no
type can express.

---

## 2. A category that sorted correctly in production and not in tests

**M4** · **Moderate** · **Found by** a test failing, then being wrong about why

`candidates_are_ordered_largest_first` failed for `PackageCacheCategory`. The obvious reading was a
broken comparator.

The comparator was fine. The category sorted on the *real* cache path's size and the test supplied a
different path, so under test it sorted on a size that was always zero. A genuine bug in the category —
it was reaching for a hardcoded location rather than the one it had been given — surfaced by a test
that appeared to be about ordering.

**Resolved** by making the category sort what it actually produces. The test then passed for the right
reason.

Recorded because the first instinct was to fix the test, which would have hidden a real defect. A test
failing for a reason you did not predict deserves the code to be read before the test is changed.

---

## 3. A registry test asserting a stale category list, three times

**M4, M5 ×2** · **Friction** · **Found by** the test failing on every category addition

`the_default_registry_holds_every_implemented_category` asserts the exact ordered list of registered
category ids. Adding a category fails it until the list is updated — which happened three times, and is
the test doing its job: the registration order is deliberate (trash first, because it is the one
category whose consequences a user has already accepted), and a silent reordering would be a real
change.

Recorded as friction rather than a defect, and left as it is. The alternative — asserting only a count,
or only membership — would not catch a reordering, and the ordering is the part that matters.

---

## 4. A field whose check could never fail

**Phase 5** · **Moderate** · **Found by** reading the code back after writing it

`PKG-2`'s criterion is that the preview matches the actual outcome, so `RemovalOutcome` carries three
sets: what went, what survived, and `unexpected` — packages that disappeared without being previewed.
That last one is the important one. It is the field that catches the operation diverging from what the
user approved.

The comparison was written like this:

```rust
pub fn compare(preview: &RemovalPreview, still_installed: &[String]) -> Self {
    // ...
    Self {
        removed,
        remaining,
        // Anything gone that the preview did not list. Computable only against a before-set, so the
        // caller supplies it by passing a preview whose `removing` is the full expected set.
        unexpected: Vec::new(),
        expected_freed_bytes: preview.freed_bytes,
    }
}
```

`unexpected` is hard-coded empty, with a comment explaining that the caller supplies it — and no caller
can, because the function does not take the before-set. Every removal would have reported "matched the
preview", forever, no matter what happened. The comment is the tell: it explains why the field is empty
rather than noticing that it therefore does nothing.

**Resolved** by giving `compare` both sets, which is what computing a difference requires:

```rust
pub fn compare(preview: &RemovalPreview, before: &[String], after: &[String]) -> Self
```

**Guard.** `something_removed_that_the_preview_never_mentioned_is_reported`, which is exactly the case
the original could not express.

Worth putting next to
[04-measurement-accuracy.md §8](04-measurement-accuracy.md), the timer properties that all read
"never" because a failed read went through `.ok()`. Same shape: a value that can only come out one way
is not a reading, and a check that can only pass is not a check. The timers took a live machine to
notice. This one was caught before it ran, by asking of a newly written function what it would take for
it to fail — which is cheaper, and is the question worth asking of anything whose job is to detect a
problem.

---

## 5. Reading the wrong directory, and a test that was happy about it

**Phase 5** · **Serious** · **Found by** printing the numbers a passing test had not looked at

`PKG-4` reads two autostart directories: `/etc/xdg/autostart` and the user's own. `user_dir` was built
on the obvious helper:

```rust
paths::config_dir().map(|dir| dir.join("autostart"))
```

`config_dir()` is `$XDG_CONFIG_HOME/nix` — **nix's own settings directory**. So it looked for autostart
entries in `~/.config/nix/autostart`, which does not exist, and found none. The user's two real entries
were never read.

The test that should have caught it:

```rust
assert!(!entries.is_empty(), "a desktop machine has autostart entries");
assert!(entries.iter().any(|e| e.origin == Origin::System), "…");
```

Both pass. There are 42 system entries, so the list is not empty and the system half works; the user
half being empty is indistinguishable from a machine that has no user entries. The test asserted that
listing works at all, and it did assert that — it simply had nothing to say about the half that was
broken.

What found it was printing the breakdown instead of the total:

```
PROBE total=42 enabled=42 system=42 user=0 no_display=40 not_in_session=3 shadowed=0
```

`user=0`, on a machine with `slack.desktop` and `jetbrains-toolbox.desktop` sitting in
`~/.config/autostart`. After the fix: `total=44 … user=2`.

**Resolved** by adding `paths::config_home()` — the XDG base directory with no application
subdirectory — and using that. Not `config_dir().parent()`, which is fragile and would have produced
the right answer for the wrong reason on a machine where `XDG_CONFIG_HOME` is set.

**Guard.** Two, because there are two ways to get this wrong. In `paths.rs`,
`the_base_config_directory_is_not_nixs_own` asserts the two helpers differ and that the base one does
not end in the application name. In `autostart.rs`, the machine test now counts `.desktop` files in the
user directory itself and requires at least one user entry **if there are any files there** — a
conditional property, since a machine legitimately might have none.

That second shape is the transferable part. The original assertion was about the *result*; the
replacement is about the result agreeing with the input. A test that reads a directory and asserts
"something came back" cannot tell you it read the right directory.

---

## 6. Two tests fighting over one process's nice value

**Phase 6** · **Moderate** · **Found by** a full `make check` after a version bump

```
test signal::tests::raising_niceness_on_our_own_process_works ... FAILED
  anyone may lower their own priority: AppError { code: AuthDenied,
    message: "Not allowed to set process 2163426 to niceness 1." }
```

Setting a nice value *up* to 1 is allowed for anyone, always — unless the value is already above 1, in
which case it is a decrease and needs privilege. So something had already moved it.

Two tests had. Both reniced `std::process::id()`:

```rust
fn lowering_niceness_on_our_own_process_reports_the_real_reason() {
    match renice(me, -5) { … Ok(()) => { renice(me, 0).ok(); } }
}

fn raising_niceness_on_our_own_process_works() {
    renice(me, 1).expect("anyone may lower their own priority");
    assert!(renice(me, 0).is_err() || renice(me, 0).is_ok());
}
```

A nice value set through `setpriority(PRIO_PROCESS)` belongs to the **process**, not to the test that
set it, and `cargo test` runs a crate's tests in parallel threads of one process. So the two were
writing one value and reading each other's writes. Neither restored it. It passes in isolation, passed
the next several runs, and failed once — which is the worst frequency for a test to fail at.

And the second one's cleanup asserted nothing whatsoever: `x.is_err() || x.is_ok()` is true of every
`Result` that exists. Written to express "either outcome is fine here", it expressed nothing, and would
have gone on passing if `renice` had started returning the wrong error entirely.

**Resolved** by giving each test a **child process it owns** — `sleep 30`, killed and reaped on drop.
State no other test can reach, and a better test of the real thing: renicing *another* process is what
the process table actually does. The tautology is replaced with a match asserting `AuthDenied`
specifically, with the root case handled explicitly rather than swept into an always-true expression.

**Guard.** The isolation is the guard: there is no longer shared state to interleave. The reaping
matters too — a zombie left behind would be read by the process-table tests, which walk the real
`/proc`.

This is the third defect in this project from **process-wide state in a parallel test harness**, after
the idle-CPU budget that measured 196% of one core and the memory budget that measured 317 MiB. The
pattern is worth stating plainly: `cargo test` gives each test a thread, not a process, so anything
reached through a pid — CPU time, resident memory, nice value, `/proc/self/*` — is shared, and a test
that writes one is writing to every other test at once.
