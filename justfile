# Entry points for humans and agents. Each recipe is a one-liner; logic lives in `cargo xtask`.
# `just check` mirrors the cheap part of the blocking CI gate (ADR-0009).

set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list

# Fast gate, run before every commit (target: under 60 s).
check: fmt-check tidy clippy test

# Full gate: the fast gate plus supply-chain checks, compiler-level lints and release-checked tests.
check-full: check deny dylint test-checked

# Format all code.
fmt:
    cargo fmt --all

# Fail if any file is not formatted.
fmt-check:
    cargo fmt --all --check

# Style rules that clippy cannot express (ADR-0006).
tidy:
    cargo xtask tidy

# All lints, warnings denied (ADR-0006).
clippy:
    cargo clippy --workspace --all-targets --locked --quiet -- -D warnings

# Unit and property tests, then doctests (nextest skips doctests).
test:
    cargo nextest run --workspace --locked --no-tests=pass
    @out=$(cargo test --workspace --doc --locked --quiet 2>&1) || { echo "$out"; exit 1; }

# Tests with optimisations and debug assertions on (ADR-0004).
test-checked:
    cargo nextest run --workspace --locked --no-tests=pass --cargo-profile release-checked

# Lints that need the compiler: no recursion (ADR-0006). Uses the nightly pinned in lints/.
dylint:
    cargo dylint --all -- --workspace --all-targets --locked

# Licenses, advisories, bans and sources (ADR-0001).
deny:
    cargo deny --locked check
