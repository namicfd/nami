# ADR-0004: Assertions, overflow checks and build profiles

- Status: Accepted (amended by ADR-0014)
- Date: 2026-11-24
- Deciders: maintainer
- Summary: `assert!` at function boundaries, before loops and once per step. `debug_assert!` only for per-cell checks in kernels. Overflow checks stay on in release.

## Context

TigerStyle treats assertions as a force multiplier. It asks for at least two per function, paired assertions on both sides of a boundary, and assertions that stay on in production. TigerBeetle runs with them enabled in production, and its Rust client recommends `overflow-checks = true` in release builds.

Measurements on nami-style kernels show the cost depends on **where** an assertion sits:

- Length and range asserts **before** a loop are free or better than free, because they let LLVM drop the per-element bounds checks.
- Asserts **inside** a hot per-cell loop are expensive.
- Overflow checks cost nothing measurable in float kernels, but they stop integer reductions from being vectorised.

## Decision

### Where each kind of assertion goes

| Placement | Macro | Active in |
|---|---|---|
| Function pre- and postconditions, argument validation | `assert!` | all builds |
| Length and range checks before loops | `assert!` | all builds |
| Per-step invariants (dt > 0, CFL, conservation totals, finite totals) | `assert!` | all builds |
| Per-pencil backstop (e.g. "all finite and density > 0" over the pencil, written as a vectorisable reduction) | `assert!` | all builds |
| Per-cell checks inside hot loops in `kernels` | `debug_assert!` | `dev`, `test`, `release-checked` |
| Compile-time relations between constants and sizes | `const _: () = assert!(..);` | compile time |

### Further rules

- **Messages are required.** Every assertion carries a message (`missing_assert_message`).
- **Pair assertions across boundaries.** Assert the same property where data is produced and again where it is consumed, for example a Riemann solver's outputs and the update kernel's inputs.
- **No unchecked hints.** `core::hint::assert_unchecked`, `unreachable_unchecked` and `get_unchecked` are unsafe and so already excluded by `unsafe_code = "forbid"`. If the compiler needs a fact, a real `assert!` provides it.
- **No overflow escape hatches.** Integer overflow is a bug. `wrapping_*` is used only where wrapping is the intended semantics, with a comment saying why.

### Build profiles (`Cargo.toml`)

```toml
[profile.release]
overflow-checks = true
debug-assertions = false

[profile.release-checked]
inherits = "release"
debug-assertions = true
```

`release-checked` runs in CI for convergence tests, the seeded simulation and fuzzing, so that expensive checks run at optimised speed.

## Enforcement

- Clippy: `missing_assert_message = "warn"` (denied in CI through `-D warnings`). `unsafe_code = "forbid"`.
- Tidy (ADR-0006):
  - `debug_assert!` is rejected outside `kernels`.
  - Assertion density: the average per non-trivial function in each crate must be ≥ 2, and any function longer than 10 lines with zero assertions is rejected. These thresholds are initial values, to be tuned when the tidy check is implemented.
- CI builds and tests both `release` and `release-checked`.

## Consequences

- TigerStyle's "assertions in production" holds everywhere except for the per-cell checks, which the per-pencil backstop still covers in production, just more coarsely.
- Agents have to classify each assertion. The tidy rule limits the damage from a wrong classification.

## Alternatives considered

- **`assert!` everywhere, including per-cell in hot loops.** Rejected. It cost about 25% in a MUSCL kernel, and the slowdown would tempt agents to delete assertions "for performance".
- **Conventional Rust (`debug_assert!` almost everywhere).** Rejected. It drops TigerStyle's central idea, and blow-ups in production runs would go unnoticed.

## Evidence

Local benchmarks, rustc 1.100, Apple Silicon:

| Kernel | Variant | Time |
|---|---|---|
| 3-point stencil | no asserts | 0.24 ms |
| 3-point stencil | asserts before the loop | 0.16 ms |
| MUSCL/minmod | no per-cell asserts | 1.43 ms |
| MUSCL/minmod | 2 per-cell asserts | 1.80 ms |

Turning overflow checks on left the timings above unchanged.

- Compiler Explorer, rustc 1.100: a `u32` sum over a slice vectorises without overflow checks and does not vectorise with them.
- TigerBeetle production asserts: [docs](https://docs.tigerbeetle.com/about/production-ready). The Rust client recommends `overflow-checks = true` in release builds.
