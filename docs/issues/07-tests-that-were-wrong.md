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
