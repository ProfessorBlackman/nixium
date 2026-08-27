# The Rust ↔ TypeScript boundary

Types cross this boundary through `ts-rs`, which generates TypeScript from Rust. Six defects came
through here and **every one of them compiled and typechecked**. This is the least trustworthy
interface in the project, and each entry below now has a test.

---

## 1. `EntryId` serialised as a number but declared as a string

**M2** · **Serious** · **Found by** reading the generated `.ts` rather than trusting it compiled

`EntryId` wraps a `u64` and was `#[serde(transparent)]`, so it serialised as a JSON number. The
generated TypeScript declared it a `string`. Worse, the same id appeared as a *string* where it was a
map key and a *number* where it was a value, in the same payload — because JSON object keys are always
strings.

Nothing failed. TypeScript believed the declaration, `serde` produced the reality, and any code
comparing an id from one position against an id from the other would silently never match.

**Resolved** by making `EntryId` a **hex string in every position**, with hand-written `Serialize` and
`Deserialize` implementations rather than `transparent`. Sixteen characters, lower case, no prefix.

**Guard.** A round-trip test through `serde_json`, and the module documentation states the reason so
nobody "simplifies" it back to `transparent`.

---

## 2. `#[ts(type = "Record<string, SpaceEntry>")]` emitted a type name with no import

**M2** · **Moderate** · **Found by** the frontend typecheck, after the Rust side was green

`#[ts(type = "…")]` takes a raw string and emits it **verbatim** into the generated file. ts-rs does
not parse it, so it cannot know that `SpaceEntry` is a type needing an import. The generated module
referenced a name it never imported, which broke the module and produced twelve implicit-`any` errors
downstream.

**Resolved** with `#[ts(as = "HashMap<String, SpaceEntry>")]`, which describes the shape as a *Rust*
type. ts-rs then understands the parts, resolves `SpaceEntry` and writes the import.

**Guard.** CI runs `tsc --noEmit` over the generated bindings, so an unimported name fails the build.
The distinction is recorded in a comment at the attribute, because `type` and `as` look
interchangeable and are not.

---

## 3. Bindings were generated to two different directories

**M2** · **Serious** · **Found by** noticing a stale type in a diff (fixed in commit `4b2b56c`)

`TS_RS_EXPORT_DIR` was set in `src-tauri/.cargo/config.toml`. Cargo discovers configuration by
walking up from the **current directory**, not from the manifest — so `cargo test --manifest-path
src-tauri/…` run from the repository root never saw that file. ts-rs fell back to its default of
`./bindings`, and a second complete copy of every type accumulated there, silently going stale.

Which copy you got depended on which directory you had happened to run cargo from.

**Resolved** by moving the config to the **repository root** with `relative = true`, so it applies
whichever directory cargo is invoked from, and gitignoring the fallback location. The config file
carries a comment explaining the discovery rule, because the failure is invisible.

**Guard.** The generated files are committed, and CI regenerates and diffs them — so a Rust type
change not reflected in the bindings fails the build instead of drifting.

---

## 4. Two Rust types named `Snapshot` overwrote each other's binding

**M5 / STO-17** · **Serious** · **Found by** looking for a binding that should have existed

`caps::Snapshot` and `cow::Snapshot` both carried `#[ts(export)]`. ts-rs names the output file after
the type, so both wrote `Snapshot.ts` and one silently clobbered the other. `cow::Snapshot` ended up
with no binding at all, and nothing anywhere reported a problem.

**Resolved** by renaming `caps::Snapshot` to `caps::Capabilities`, which is the better name
regardless — it describes what the type holds rather than that it was taken at a moment.

**Guard.** A test in `lib.rs`, `no_two_exported_types_share_a_name`, walks the crate source for
`#[ts(export)]` type names and fails on a duplicate, naming both modules. **Verified to fire** by
reintroducing the collision and watching it fail.

This is the entry that motivated the others being guarded too. Three separate silent failures through
one interface is a property of the interface, not bad luck.

---

## 5. Rust methods are not serialised fields

**M2, M4** · **Moderate** · **Found by** the frontend reading `undefined`

Three times — `coverage_note`, then `reclaimed_count`, `skipped_count` and `failed_count` — a value
the frontend needed was implemented as a Rust **method**. Methods are not serialised. The generated
type did not declare them, the frontend read `undefined`, and because the surrounding types were
correct the mistake looked like a data problem rather than a shape problem.

**Resolved** by making each a real field, computed once at construction. `Report::new()` derives its
counts as it builds rather than offering accessors.

**Guard.** Partial and honestly so: `tsc` catches an access to a property the generated type does not
declare, which is what closes the loop *if the frontend reads it*. There is no guard against a
method that nothing reads yet. The convention is written down instead — anything the UI needs is a
field.

---

## 6. A build warning nobody could act on, on every single build

**Phase 0, fixed in M5** · **Friction** · **Found by** the user running `pnpm tauri dev` and reading the output

`OperationId` and `Ticket` are `u64` newtypes that cross the boundary as bare numbers, spelled with
`#[serde(transparent)]` and `#[ts(export, type = "number")]`. ts-rs's attribute parser does not
understand `transparent`, so every build printed:

```
warning: failed to parse serde attribute
  | transparent
  = note: ts-rs failed to parse this attribute. It will be ignored.
```

Twice, on every compile, for months. The behaviour was correct — the wire type is stated explicitly
right underneath — and a comment said as much, so it was noise.

Which is the problem. I had been filtering it out of my own command output with `grep -E "^error"` for
the whole project, and that habit hides real warnings too. It took someone else reading a build log to
raise it.

**Resolved** by deleting the attribute, which turned out to be **redundant**: a single-field newtype
already serialises as its inner value in JSON. Verified before and after rather than assumed —
`OperationId(7)` encodes to `7` and `{"id":42}` nested, identically both ways — and the generated
TypeScript is byte-for-byte unchanged.

**Guard.** `an_operation_id_is_a_bare_number_on_the_wire` and `a_ticket_is_a_bare_number_on_the_wire`
assert the encoding, because "redundant" is exactly the sort of claim that should not live only in a
comment when the cost of being wrong is every id on the wire changing shape.

After this the workspace builds with **zero warnings**, which is the state that makes the next one
worth reading.

---

## 7. `u64` crosses to TypeScript as `bigint`

**Phase 0** · **Friction** · **Found by** the first generated binding

ts-rs maps `u64` to `bigint`, which is correct and unusable: `bigint` does not mix with `number` in
arithmetic, does not survive `JSON.stringify`, and every byte count in this project is a `u64`.

**Resolved** with `#[ts(type = "number")]` on byte counts, accepting the loss of precision above
2^53. Nine petabytes is not a size this tool needs to report exactly.

**Guard.** None, deliberately. The trade-off is recorded in a comment at each site naming the 2^53
limit, so the choice is visible to anyone who later needs a genuinely large integer.
