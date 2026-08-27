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
