# Tidy's syntax rules do not look inside macro arguments

- Status: Open
- Found in: the guardrail review of 2026-11-25

## What happened

`syn` does not parse the arguments of a macro invocation, so the rules that walk the syntax
tree skip them. A `loop`, a `Vec` field or a `usize` parameter written inside a `macro_rules!`
body, or passed to a macro, is not reported. The ban tables and `magic-tolerance` work on
tokens and do look there.

## Proposed guardrail

Tidy: either move `unbounded-loop` to the token walk in `xtask/src/tidy/tokens.rs` (the
keywords `loop` and `while` are easy to find in tokens), or ban `macro_rules!` in nami crates
outside an allow-list with a row in `MACRO_BANS`. The second is one row and settles the
question for every syntax rule at once.
