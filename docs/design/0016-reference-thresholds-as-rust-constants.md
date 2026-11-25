# ADR-0016: Reference thresholds as Rust constants

- Status: Proposed
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0007 (point 4, "Thresholds")

## Context

ADR-0007 puts every reference threshold in one human-owned file, `reference/thresholds.toml`:
- the allowed order shortfall δ;
- the absolute error ceilings;
- the conservation tolerances.

The first plan that needs thresholds is `advection1d` in `docs/exec-plans/active/advection-1d/`. Reading TOML from the `reference` crate needs one of two things:

- **A TOML crate.** That would be a new dependency, and ADR-0001 allows a new crate only through an ADR.
- **A hand-written parser.** It would be one more thing to test and keep correct, written to read about twenty numbers.

What ADR-0007 actually wants from the file is that thresholds sit in one place, change only with human approval, and are visibly separate from the tests that use them.

## Decision

1. **Thresholds are `pub const` items** in `crates/reference/src/thresholds.rs`, grouped by equation system.
2. **Each constant's doc comment** gives its derivation or its source, for example: "modified-equation estimate for first-order upwind, LeVeque §8.6, ×2 margin". A threshold without a derivation is a review failure.
3. **The tests read the constants by name.** Nobody may pass a tolerance as a literal in a reference check.
4. **CODEOWNERS** already covers `crates/reference/`, so a changed threshold always needs the maintainer.

## Enforcement

- CODEOWNERS on `/crates/reference/`, as before.
- Review: `numerics-verifier` checks every changed threshold against its documented derivation. `tigerstyle-reviewer` blocks a loosened threshold that has no ADR or explanation.
- A later tidy rule may reject float literals inside `crates/reference/src/*/checks` outside `thresholds.rs`. It is not needed for the first plan.

## Consequences

- No parser and no dependency. Thresholds are typed and checked at compile time, and an unused or misspelled threshold is a compile error.
- Changing a threshold means recompiling. That is fine, because changing one is a reviewed event anyway.
- It deviates from the letter of ADR-0007, point 4. This ADR amends it.

## Alternatives considered

- **Keep `thresholds.toml` with a TOML crate (`toml`, `basic-toml`).** Rejected. It is a new dependency tree for a test-only file, against the spirit of ADR-0001.
- **Keep `thresholds.toml` with a hand-written subset parser.** Rejected. It adds code to maintain and test, and buys nothing over constants.
- **Inline thresholds in each test.** Rejected. They would be scattered, and an agent editing a test could loosen them in the same diff.

## Evidence

- ADR-0007, point 4, and ADR-0001 (dependency policy).
- Trixi.jl and Athena++ keep their regression tolerances next to the test definitions in code (ADR-0007 evidence). This ADR keeps them in code, but centralised.
