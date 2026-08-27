# Concurrency and performance

---

## 1. Scanner cancellation took 37 seconds

**M2** · **Serious** · **Found by** a test with a latency budget

Cancelling a scan should be near-instant; the budget is 300 ms. It took **37 seconds**. Three rounds of
diagnosis, two of which were wrong, and the order matters because each wrong answer was plausible.

**First guess: debug builds are slow.** Measured instead of assumed — a release scan of 93,620 files
takes 233 ms. Slowness was not the explanation for a 37-second cancellation.

**Second guess: this specific test is heavy.** Partly true and worth fixing, but not the cause.
Sibling tests were each building 93,000-file `Spec::perf()` fixtures, and several running in parallel
produced an I/O storm that made every timing in the suite unreliable. Right-sized to what each test
actually needs.

**The real cause: architecture.** The walker created a `rayon::scope` **per directory**. A scope blocks
its caller until its children finish. Nesting scopes therefore fills the thread pool with threads
waiting on other threads, and on a deep tree the pool deadlocks into near-serial progress — so
cancellation could not be observed until the structure unwound.

**Resolved** by replacing nested scopes with `par_iter().map().reduce()`, which is built on
`rayon::join` and nests correctly by design: work is stealable at every level and no thread blocks on a
child it could be running itself.

Result: **447,687 files/sec** in release, 2.23 µs per file against a 30 µs budget.

The test was also made deterministic. It had been racing — cancelling after a sleep and hoping work was
still in flight. It now cancels **from inside the progress callback**, which by construction only fires
while work is happening.

**Guard.** `SCAN_PER_FILE` and `CANCEL_LATENCY` are budgets in `budget.rs`, asserted by a CI job that
runs the release build:

```
cargo test -p nix-core --release --lib -- budget:: --nocapture
```

A regression fails the build rather than being noticed by someone waiting.

---

## 2. Test fixtures caused an I/O storm

**M2** · **Friction** · **Found by** the diagnosis above

Several tests used `Spec::perf()` — 93,000 files — when they needed a few dozen. Under `cargo test`
these run in parallel, and building them concurrently made every timing measurement in the suite
noise.

**Resolved** by right-sizing each fixture to what its test actually asserts. `Spec::perf()` is now used
only by the performance tests that need that scale.

**Guard.** None mechanical. Convention, recorded here: a fixture's size is part of the test's cost, and
a correctness test does not need a performance fixture.

---

## 3. Fixture temporary directories collided between parallel tests

**Phase 0** · **Moderate** · **Found by** intermittent test failures

Fixture directories were named from the process id and the spec's seed. Two tests using the same seed
in the same process — the normal case, since the seed is chosen for reproducibility — got the **same
path**, and deleted each other's trees mid-run.

The symptom was intermittent failures in whichever test lost the race, with an error about a missing
file that had definitely been created.

**Resolved** with a process-wide `AtomicU64` counter appended to the name:

```rust
static COUNTER: AtomicU64 = AtomicU64::new(0);
let n = COUNTER.fetch_add(1, Ordering::Relaxed);
let root = std::env::temp_dir().join(format!("nix-fixture-{}-{}-{n}", process::id(), spec.seed));
```

**Guard.** The counter is inside `Fixture::create`, so every caller gets uniqueness without having to
know about the problem. The comment above it explains what went wrong, since the code's purpose is
otherwise unguessable.

The same mistake recurred in STO-12's flatpak tests, which used a bare `process::id()`. Fixed the same
way, which suggests the right long-term answer is one shared temp-directory helper rather than the
convention being re-derived per module.
