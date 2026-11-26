# Playbook: add a guardrail

Use this when a check should have caught something and did not: a bug, a review comment that no
check enforces, an error message that did not say how to fix the problem (ADR-0020).

If the guardrail is not needed for your task, do not build it now. File an issue instead
(`cargo xtask issue file --kind guardrail-gap`, see `docs/exec-plans/README.md`, Issues) and
carry on. Another session claims the issue and builds the check (ADR-0021).

## 1. Pick the rung

Use the first rung that can catch the problem.

| Ask | Rung | Where |
|---|---|---|
| Can a type make the mistake impossible to write? | A type, or the compiler | the code itself |
| Is it "never call this function, never use this type or macro", for a path that clippy can resolve? | clippy ban | `clippy.toml` |
| Does clippy already have a lint for it? | clippy lint | `[workspace.lints.clippy]` in `Cargo.toml` |
| Is it "this path or this macro only in that file or directory"? | tidy ban | `PATH_BANS` or `MACRO_BANS` in `xtask/src/tidy/registry.rs` |
| Is it about the shape of the source: length, attributes, literals, signatures, file names? | tidy rule | `xtask/src/tidy/` |
| Does it need types, trait resolution or the call graph? | dylint lint | `lints/src/` |
| Is it about the numbers the code produces? | reference or invariant test | `crates/reference`, by another session (ADR-0007) |
| None of these? | prose, tagged `[review]` | `docs/TIGER_STYLE.md` |

Tidy sees text and syntax, never types. If the rule needs to know that an expression is an
`f64`, it is a dylint lint.

## 2. Write the failing case first

Write the smallest code that breaks the rule, and the nearest code that is correct. They become
the rule's fixtures. If you cannot write the correct version, the rule is wrong.

## 3. Add the check

### A clippy ban

Add an entry to `disallowed-methods`, `disallowed-types` or `disallowed-macros` in `clippy.toml`.
The `reason` is the error message, so it names the replacement:

```toml
{ path = "f64::sqrt", reason = "use kernels::math::sqrt (libm, deterministic; ADR-0005)" },
```

### A tidy ban

Add one row to `PATH_BANS` or `MACRO_BANS` in `xtask/src/tidy/registry.rs`, sorted by id. There
is no other code to write. Then add the two fixtures (below).

### A tidy rule

1. Add a row to `RULES` in `xtask/src/tidy/registry.rs`: `source(..)` for a rule that one file
   decides, `workspace(..)` for a rule that needs the whole tree. Give the id, a one-line summary
   and the ADR or style-guide section that holds the reason.
2. Write the check where its input is parsed: `rust_source.rs` (syntax tree), `tokens.rs`
   (tokens, including macro arguments), `text.rs` (lines), `manifest.rs` (`Cargo.toml`) or
   `docs.rs` (Markdown).
3. Report through `Violation::new(path, line, "<id>", message)`. It panics on an id that is not
   registered.

### Fixtures for a tidy ban or rule

Add `xtask/fixtures/tidy/<id>.fires.fixture` and `<id>.passes.fixture`. The first line names the
virtual path that decides which rules apply:

```rust
// path: crates/kernels/src/example.rs
```

Use `# path: ..` in a manifest and `<!-- path: .. -->` in Markdown. The firing fixture must
produce your rule and nothing else. The passing fixture must produce nothing. A workspace rule
has no fixture files: make it fire in `broken_workspace()` in `xtask/src/tidy.rs`.

### A dylint lint

1. Add a module in `lints/src/`, declare the lint in `lints/src/lib.rs` at `Deny`, and register
   its pass in `register_lints`. `just dylint` does not deny warnings, so a `Warn` lint gates
   nothing.
2. Add `lints/ui/<name>.rs` with the cases that must fire and the ones that must not, and its
   `.stderr`. Read every line of the expected output before you accept it.
3. Run `cargo test --locked` inside `lints/`, then `just dylint` from the root.

The library is tied to the nightly in `lints/rust-toolchain.toml`, and compiler internals move
between nightlies. The existing lints show which APIs exist.

## 4. Write the message for the next agent

`path:line: error[<id>]: <what is wrong>; <how to fix it> (<where the reason is>)`

The reader is an agent in the middle of a task. The message is all the help it gets, so name the
replacement, not only the offence.

## 5. Make the documents agree

Tidy fails until all of these are done.

- Tag the rule in `docs/TIGER_STYLE.md`: `[tidy:<id>]`, `[clippy:<lint>]`, `[rustc:<lint>]` or
  `[dylint:<lint>]`. If the rule is new, add its paragraph there: the rule, its reason and an
  example, never what the check sees. Tidy's `doc-tool-name` rejects rule prose that names a tool.
- Write what the check sees and misses, its exemptions and its known gaps, in the `detail` of the
  rule's row in the registry, or in the `### What it does` section of the lint's doc comment.
  Tightening a rule later then changes only these, not the style guide.
- Run `cargo xtask rules --write` to regenerate `docs/RULES.md`.
- If the mechanism was planned: delete its row from `PLANNED` in the registry, and replace its
  `[planned:<id>]` tags. Leave `passes` in `tasks.json` alone.
- If it came from an issue: write `Fixes #<n>` in the PR description, so that the merge closes it.

## 6. Check that it does not fire on correct code

Run `just check-full`. If the new rule fires on existing code, fix that code in the same PR.
Never narrow the rule to make it pass. If the code is right and the rule is wrong, the rule is
not ready: file an issue and leave it out.

## 7. The pull request

- Put the guardrail in a commit of its own (ADR-0015).
- Under `Rule changes`, say that it tightens, and name the rule.
- Under `Protected files`, list what `cargo xtask protected` prints.
- No ADR is needed for a check of a rule that is already written down. A new constraint needs
  its paragraph in the style guide. Reversing or narrowing a decision needs an ADR with status
  `Proposed`.

CI's base gate then shows the code owner what your rule finds in `main`'s tree.

If the PR changes only rule paths (ADR-0022: the rule tables and source-rule modules of tidy,
`lints/src/`, the fixtures, the UI cases and `docs/RULES.md`), it needs no code owner. The
required `ratchet` check runs `main`'s `cargo xtask ratchet` on your tree instead. It fails when
a case that `main` writes down no longer holds through your tools, when a mutant of your changed
lines survives your tests, or when the reviewer app has not reviewed your head commit or left a thread
open. Only the reviewer app's review counts, not the agents' bot. A new ban row or another
pure data change yields no mutant, so it still needs the code owner. A finding names the fixture
or UI case and the line. Fix the rule, never the case: `main`'s
cases count, whatever your branch did to its own copies. Run it yourself first, inside a checkout
of `main`: `cargo xtask ratchet --base . --head <your tree> --no-review`.
