---
title: Comment style
nav_order: 12
---

# Comment style

A comment is the last resort. Work down this ladder, and write prose only
when all four rungs fail.

1. **Extract it.** Logic that a comment explains belongs in a function with
   a domain name. Arithmetic spelled out in a comment belongs in named
   constants or a compile-time check. A coercion that is explained twice
   belongs in one named helper.

   ```rust
   // Before
   // Kernel struct v4l2_buffer: 88 bytes
   // The umask strips the group bits from the create mode.   (twice)

   // After
   const _: () = assert!(std::mem::size_of::<v4l2_buffer>() == 88);
   undo_umask(&file)?;
   ```

2. **Name it.** A comment that labels a block (`// Liveness: prune, compare,
   push`) means the block wants to be a function or a type (`Liveness::observe`).
   Sentinels get names too: `ARRAY_OF_TABLES`, not `"\0" // no keys here`.

3. **Pin it with a test pair.** A comment that states a business rule is a
   rule nothing enforces. Write one test inside the boundary and one
   outside, named after the rule (`accepts_frame_at_minimum_mean` /
   `rejects_frame_below_minimum_mean`), instead of writing `// exactly at the
   gate is fine`. The pair is the point: it fails the day someone moves the
   limit. In a test, a comment that labels the next few asserts is the same
   smell. Split those asserts into their own test, named after what the
   comment said.

4. **Pin cross-boundary rules with an end-to-end test.** What `deploy.sh`
   installs and patches, the PAM entry point's exit codes, and what the
   helper's verbs accept belong in CI's `deploy-script` job or a test, not in
   a paragraph.

5. **Only then, prose.** Hidden constraints (a kernel ABI, a group-writable
   directory, a set-group-ID caller), upstream bugs, protocol requirements.
   State the constraint, not its biography: "the umask strips the group bits"
   is enough without the story of how it was found. Measured claims carry a
   date where one is known.

Security reasoning in the auth path (why a gate exists, what it stops) is a
hidden constraint and stays, as short as it can be. Pin it with a test when
one can state it.

## Linking the test from the code

When rung 3 or 4 replaced a comment, the code it governs says which test
pins the rule, always with the same phrase:

```rust
/// Under the mean gate is too dark; at it is fine (pinned by
/// `tests::accepts_frame_at_minimum_mean`,
/// `tests::rejects_frame_below_minimum_mean`).
```

rustdoc can't link to `#[test]` functions (they are compiled out even
under `--cfg test`), so these are code spans, not intra-doc links.
`crates/face-auth-core/tests/doc_links.rs` (CI job "Doc Links") checks every
crate's `src/`: each `` `tests::name` `` or `` `module::tests::name` `` after
"(pinned by" must be a `#[test]` fn in that file or module, and a comment
naming a test without the phrase fails too. The phrase is one constant there.

## Exempt

- `///` and `//!` docs on items, which say what an item is and how to call
  it. They follow the same "constraint, not biography" rule, and shouldn't
  restate the signature, but aren't replaced by code.
- `// SAFETY:` comments on `unsafe`, `#[allow]` reasons, and coverage
  pragmas.
