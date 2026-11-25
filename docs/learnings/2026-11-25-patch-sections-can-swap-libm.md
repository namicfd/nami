# A `[patch]` section can replace `libm` unnoticed

- Status: Open
- Found in: the guardrail review of 2026-11-25, while tightening `runtime-dependency`

## What happened

Tidy's `runtime-dependency` rule checks the dependency tables of each crate and rejects a
`package = ".."` rename. It does not look at `[patch]` or `[replace]` in the root manifest.
A patch could point `libm` at a path or a fork, and every crate would still depend on a crate
called `libm`. `cargo deny` allows only the crates.io registry as a source, which covers a
git patch. Whether it covers a path patch is not verified.

## Proposed guardrail

Tidy, in `xtask/src/tidy/manifest.rs`: report any `[patch.*]` or `[replace]` section in the
root `Cargo.toml`. One fixture pair. Check first what `cargo deny check sources` says about a
path patch, so that the rule does not repeat it.
