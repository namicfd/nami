# `just check` does not format-check the lint library

- Status: Open
- Found in: the guardrail review of 2026-11-25, while changing `lints/`

## What happened

`lints/` is its own cargo workspace, so `cargo fmt --all --check` at the root does not cover
it. The edit hook formats a lint source when an agent edits it with the edit tool, but an edit
through the shell is never formatted, and no gate notices. The agent that rewrote the lints
ran `rustfmt --edition 2024 --check` on `lints/src/*.rs` and `lints/ui/*.rs` by hand.

## Proposed guardrail

`justfile`: make `fmt-check` and `fmt` also run `cargo fmt --check` inside `lints/`. It needs
the nightly's rustfmt component, so check that `lints/rust-toolchain.toml` installs it.
