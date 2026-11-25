# A changed lint does not run on unchanged crates

- Status: Codified in `cargo xtask dylint` (ADR-0016)
- Found in: the guardrail review of 2026-11-25, while merging `nami_float_reduction`

## What happened

After `lints/src/` changed, `cargo dylint --all -- --workspace --all-targets --locked -v`
printed `Fresh` for every crate whose own sources had not changed, and finished in 0.2 s. cargo
does not know that the lint library is an input of the check, so the new lint never ran on
those crates. CI has the same hole whenever `target/` is restored from a cache. A PR that adds
a lint could therefore pass while existing code breaks it.

## Proposed guardrail

A wrapper, `cargo xtask dylint`, that hashes `lints/` and puts the hash into
`DYLINT_RUSTFLAGS`, which the dylint driver asks cargo to track. `just dylint` calls the
wrapper. Checked by hand: six crates are checked, none on the second run, and six again after
a comment is added to a lint source.
