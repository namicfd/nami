# ADR-0013: Lint table refinements from the first scaffold

- Status: Proposed
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0003, ADR-0006

## Context

ADR-0003 and ADR-0006 proposed a lint table before any code existed. Compiling the style-guide examples and the first crates under that table on Rust 1.100 / clippy 0.1.100 showed four lints that fire constantly without catching anything that another mechanism does not already catch. It also showed one configuration that is needed for a planned rule to work at all.

## Decision

1. **Drop `arithmetic_side_effects`.**
   - It fires on every integer operation, including `state.len() + 1` inside an assertion.
   - Integer overflow is already a crash in every build profile, because `overflow-checks = true` is set in `dev`, `test`, `release` and `release-checked` (ADR-0004).
   - The lint therefore adds noise and no safety. This amends the enforcement section of ADR-0003.
2. **Allow `missing_panics_doc`.**
   - Under TigerStyle every function asserts, so this pedantic lint asks for a `# Panics` section on nearly every function.
   - Assertion failures are programmer errors, not behaviour that callers handle, so documenting each one is noise.
3. **Drop `default_numeric_fallback`.**
   - It flags float literals even where the type is fully determined, for example `value >= 0.0` with `value: f64`. In numerical code that is nearly every comparison.
   - Integer width is enforced instead by `as_conversions`, the cast lints and typed indices (ADR-0002).
4. **Do not ban `Vec::push`, `Extend::extend` or `vec!` with `disallowed-*` lists.**
   - Clippy cannot scope a ban to code that runs after init, and the init code in `nami-solver` legitimately allocates.
   - Allocation after init is enforced instead by:
     - the compiler: `nami-kernels` and `nami-core` are `no_std` without `alloc`;
     - the counting-allocator test (ADR-0003);
     - the `Box<[T]>` storage rule (`TIGER_STYLE.md`).

   This amends the enforcement section of ADR-0003.
5. **Set `avoid-breaking-exported-api = false` in `clippy.toml`.**
   - Without it, clippy's `upper_case_acronyms` skips public items, so `pub struct HLLCSolver` would pass.
   - nami is unpublished, so there is no external API to protect.
6. **Use `use_small_heuristics = "Max"` in `rustfmt.toml`.**
   - The default heuristics wrap calls at 60 characters, well short of the 100-column limit.
   - TigerStyle says to use the full width: "Use it up. Never go beyond."

## Enforcement

The workspace `[workspace.lints]` table in `Cargo.toml`, plus `clippy.toml` and `rustfmt.toml`. All three are code-owned (ADR-0008).

## Consequences

- The lint table is quiet enough that agents can treat every warning as a real problem.
- Integer overflow safety now rests on the runtime overflow checks rather than a lint, so it is caught in tests and seeded simulation, not at compile time.

## Alternatives considered

- **Keep `arithmetic_side_effects` and require `checked_*` everywhere.** Rejected. It is verbose, duplicates the overflow checks, and invites agents to scatter `#[expect]`.
- **Keep the `Vec` bans with per-crate `clippy.toml` files.** Rejected. The `no_std` crates already make the ban a compile error, and duplicating `clippy.toml` per crate drifts.

## Evidence

Local clippy 0.1.100 runs on the scaffold and on the `TIGER_STYLE.md` examples:

- `arithmetic_side_effects` fired on `state.len() + 1`;
- `missing_panics_doc` fired on every asserting public function;
- `default_numeric_fallback` fired on `value >= 0.0`;
- `upper_case_acronyms` ignored `pub struct HLLCSolver` until `avoid-breaking-exported-api = false` was set.
