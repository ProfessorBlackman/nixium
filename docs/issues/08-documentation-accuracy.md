# Documentation accuracy

The Stacer analysis in `docs/stacer/` is reverse-engineered from source, so its claims are only as
good as the checking. Three in the first draft were wrong, each found by going back to the source
rather than trusting the note made while reading it. A fourth entry here concerns the published
artifacts rather than a claim about Stacer.

---

## 1. Line-number citations were off

**Documentation phase** · **Moderate** · **Found by** re-checking every citation with grep

The per-feature documents cite Stacer source locations as `file.cpp:123`. Roughly twenty citations were
off by a few lines — accumulated drift from reading a file, making a note, and the note ageing against
a scroll position.

A citation that is nearly right is worse than none: it sends a reader to plausible-looking code that is
not the code being discussed, and they may not notice.

**Resolved** by verifying each with grep and correcting twenty references.

**Guard.** None mechanical, and there cannot easily be one — Stacer is a separate read-only tree with
no build step of ours. The mitigation is that citations name a symbol as well as a line where possible,
so a reader can find the right place even if the number has drifted.

---

## 2. A claim about a context menu that did not exist

**Documentation phase** · **Moderate** · **Found by** checking the `.ui` file instead of assuming

The Processes documentation stated that the context menu opens on the table body — the normal
arrangement, and what a reader would expect.

`processes_page.ui` sets no `contextMenuPolicy` at all. The claim was inferred from what such a page
usually does, not observed.

**Resolved** by reading the `.ui` file and correcting the claim.

The general point: in reverse-engineered documentation the *plausible* claim is the dangerous one,
because nothing prompts you to check it. The implausible claim gets verified automatically by
disbelief.

---

## 3. An icon path typo

**Documentation phase** · **Friction** · **Found by** proofreading

`{16,32,...}x256` in the packaging documentation, where the sizes should vary in both dimensions.
Corrected.

---

## 4. Native CSS nesting in a published artifact

**Documentation phase** · **Friction** · **Found by** the artifact skill's explicit instruction

The specification and plan were also published as artifacts. The first version used native CSS nesting
— an `@media` block inside a selector — which the skill's instructions explicitly disallow, for
compatibility.

**Resolved** by flattening the rules.

---

## 5. §D10 credited the helper's isolation to the wrong mechanism

**Phase 4** · **Moderate** · **Found by** verifying a documented claim against the source

The decision record for using D-Bus closes on the security question — zbus must not end up linked into
the binary that runs as root — and said so with a measurement:

> a whole-workspace build produces **zero `zbus` symbols in the helper** and 47,547 in the app. Edition
> 2024's resolver does not unify the feature across the two binaries.

The measurement is real and was taken. The sentence after it is wrong, and it is the sentence carrying
the claim:

```
$ cargo build --workspace --message-format=json | ...
nix_core ['lib'] features= ['dbus', 'default']      # one artifact, feature on
```

The resolver unifies. In a whole-workspace build there is a single nix-core rlib with `dbus` enabled and
the helper links *that*, so its zero symbols are the linker discarding code nothing reaches — not a
feature gate holding. Same observation, materially weaker guarantee: dead-code elimination depends on no
helper code path reaching zbus, whereas the claim as written implied the helper could not compile against
it even if a path did.

The shipped artifact is fine, and better than the entry claimed for the build it measured. `cargo build
-p nix-helper`, which is how the helper is built and packaged, resolves without zbus anywhere in the
graph:

```
$ cargo tree -p nix-helper | grep -c zbus
0
```

**Resolved** by rewriting the section to separate the two builds and state which guarantee each gives,
and by adding the feature-off CI run that makes the strong claim testable —
[06-toolchain-and-lints.md §9](06-toolchain-and-lints.md) has that half.

**Guard.** The feature-off run. The wider point is why this file exists: the error survived because a
correct number sat next to the explanation and lent it credibility. Four of the five entries here are
claims about Stacer that went unchecked; this one is a claim about nix's own build, which is worse,
because it was one command away the whole time.
