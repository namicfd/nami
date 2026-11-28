# Plan M0: linear advection, the walking skeleton

## Goal

The first vertical slice of nami (ADR-0035, milestone M0). It solves 1D linear advection on the
real architecture, so that every later milestone extends this code instead of rewriting it. It
also puts the task, claim and review process through its first full run.

M0 is **thin on methods and complete in structure:**

- **Methods:** donor cell reconstruction, the LLF Riemann solver, forward Euler, periodic
  boundaries. PLM, the limiters, HLLE and the Runge–Kutta integrators come in a later plan (M0b).
- **Structure:**
  - the 3D data model, with one block that is 1D along x, y or z;
  - structure of arrays and row kernels (ADR-0043);
  - the extension-point traits (ADR-0037);
  - the configuration, output with version metadata, checkpoint and restart (ADR-0041);
  - diagnostics;
  - the reference suite chosen by declared properties (ADR-0042).

Read `docs/ARCHITECTURE.md` first. It gives the whole design, and this plan only cuts it into
tasks.

## Problem

ū is the cell average of u. The problem is

  ∂u/∂t + **a**·∇u = 0,   **a** = (a_x, a_y, a_z) constant,

on the unit cube with periodic boundaries. In M0 every problem is 1D: the block has N cells along
one axis (x, y or z) and one cell along each of the other two, and **a** points along that axis
(a ≠ 0, either sign). The exact solution is u(x, t) = u₀(x − a t), periodic. Cell i along the
axis covers [i Δx, (i + 1) Δx), with Δx = 1/N.

### Discretisation

- **Conservative update** in area/volume form (ADR-0035):
  ū_i ← ū_i − (Δt / V_i) (A_{i+½} F_{i+½} − A_{i−½} F_{i−½}).
  It has one flux per face, shared by both neighbours. On a Cartesian grid, A = Δy Δz and
  V = Δx Δy Δz.
- **Donor cell:** the face states are the adjacent cell averages. The ghost width is 1.
- **LLF flux:** F(u_L, u_R) = ½ (a u_L + a u_R) − ½ s (u_R − u_L), where s = |a_n| is the
  wave-speed bound of linear advection along the face normal. For linear advection this equals
  the upwind flux, which is the exact Riemann solution (LeVeque, *Finite Volume Methods for
  Hyperbolic Problems*, §4.10), up to round-off.
- **Forward Euler,** as a one-stage table in the low-storage 3S* form (ADR-0037), so that M0b only
  adds tables.
- **Time step:** Δt = ν Δx / |a|, where ν is the configured Courant number. The last step is
  shortened so that the run ends exactly at the requested time, and no step is longer than
  ν Δx / |a|. Upwind with forward Euler is stable and monotone for ν ≤ 1 (LeVeque §4.4), and the
  configuration rejects ν > 1.

### Performance estimate (TIGER_STYLE.md, Performance)

Per cell per step: read ū and two face fluxes, write one flux and ū. That is about 40 bytes and
6 flops: memory-bound, around 10⁹ cell updates per second per core. Advection is never the
benchmark. M0 sets no speed target; the Athena++ comparison starts with Euler (M4, ADR-0034).

## Rules that apply

- ADR-0002: indices and counts are `u32` newtypes.
- ADR-0003, as amended by ADR-0043: `kernels` is `no_std` without `alloc`; data access through row
  stencils, named state structs and typed gathers; no raw indexing.
- ADR-0004 and ADR-0043: assertions. Per-face and per-cell checks inside hot loops are
  `debug_assert!`, and the per-block backstop is the release-build guarantee. The layout prototype
  (`docs/research/layout-prototype.md`) measured checks that stay on in release, deferred per row;
  if an ADR adopts them, the task that implements that ADR changes the M0 kernels. Until then the
  kernel tasks follow the accepted rule.
- ADR-0005: determinism.
- ADR-0007 and ADR-0042: references.
- ADR-0037: extension points; ADR-0038: blocks, pools, phases; ADR-0041: configuration, output,
  restart.
- TIGER_STYLE.md, "Numerical code".

## How the references plug in

`reference` defines a **contract** that any scheme for a 1D conservation law can implement, and
**checks chosen by declared properties** (ADR-0042). A sketch to start from; the `reference-suite`
task owns the final API:

```rust
/// What the checks need from a scheme (in `reference::contract`).
pub trait Scheme {
    /// A periodic run on [0, 1) along `axis`, with `cells` cells, speed `speed` and Courant
    /// number `courant`, starting from `initial`.
    fn new(axis: Axis, cells: CellCount, speed: f64, courant: f64, initial: &Initial) -> Self;
    /// Advances to exactly `time` (at least the current time).
    fn advance_to(&mut self, time: f64);
    /// The current cell averages along the axis, `cells` values.
    fn cell_averages(&self) -> &[f64];
    /// Time steps taken so far.
    fn steps(&self) -> u32;
    /// The largest signal speed the scheme used in its last step (its wave-speed bound).
    fn max_signal_speed(&self) -> f64;
}

/// What a scheme claims. The checks follow from the claims; the implementer does not pick them.
pub struct Properties {
    pub formal_order: u8,
    pub monotone: bool,
    pub boundaries: &'static [Boundary],
}
```

`Initial` carries only cell averages. The profile it came from stays private to `reference`, so a
scheme cannot evaluate the exact solution. The wiring task adapts nami's solver to `Scheme` and
calls the suite once per combination, with one line each.

**The parts of the closed PR #4 that carry over** (`git fetch origin pull/4/head`, file
`docs/exec-plans/active/advection-1d/plan.md`) are the checks and their reasoning:

- **Run conditions.** Every check runs with both signs of a, on at least two resolutions, none
  coarser than N = 32.
- **Finite first.** Every check first requires each value it computes to be finite, and writes
  comparisons so that NaN fails.
- **ν = 0.45.** It is below every scheme's limit, and it makes the step count non-integer at
  every tested N for t = 0.25 and 0.3.
- **Conservation,** on the smooth profile and on the pulse: |M_end − M_0| ≤ C_mass · ε · scale ·
  steps, with compensated sums, ε = `f64::EPSILON` and scale = Σ|ū⁰| Δx.
- **Moved correctly.** The pulse on [0.3, 0.45) advanced by 0.3. The L1 distance to the exact
  solution must be at most half the distance from the initial data, and below an absolute L1
  ceiling.
- **Order.** The smooth profile, t = 0.25, N = 32, 64, 128: p ≥ p_formal − δ with δ = 0.1 on the
  finest pair, plus an error ceiling derived from theory. For upwind, the modified equation
  gives the diffusion D = |a| Δx (1 − ν)/2, so the leading error is t D u₀″ shifted by a t, and
  the ceiling is 1.5 × t D ‖u₀″‖₁.
- **Maximum principle** for monotone schemes, on the pulse.
- **Mirror symmetry,** bitwise.
- **Determinism,** bitwise.
- **Step count.** At least ⌈(t|a| / (νΔx)) (1 − 4ε)⌉ steps.
- **The exact scheme is exempt from two checks,** as in PR #4: from the order check, because its
  error is exactly 0 and log₂(0/0) is NaN, and from bitwise mirror symmetry, because the exact
  averages of the mirrored problem agree only to rounding (3.6e-15 on the pulse at t = 0.3,
  N = 32). The known-order and shift schemes below are the positive controls for those two checks.

**One change from PR #4: the smooth profile is a polynomial, not a sine.** It is
u₀(x) = 16 x² (1 − x)² on [0, 1), extended periodically. It is C² (u₀, u₀′ and u₀″ match at the
wrap), which is ample for first and second order. Its exact cell averages and ‖u₀″‖₁ are
polynomial expressions, so `reference` computes them without any transcendental function. That
keeps `libm` out of `reference` (tidy's `direct-libm` ban allows it only in `kernels::math`, and
`reference` must not depend on `kernels`), with no rule change. Exact cell averages of the
square pulse are piecewise linear in its end points.

New in M0:

- **Rotation, bitwise:** the same problem along x, y and z gives identical cell averages
  (ADR-0042, § 7.4).
- **Wave-speed bound:** the scheme's `max_signal_speed` is at least |a|, which `reference`
  computes from the problem it set up, not from the scheme's system. This replaces the old
  invariant test `wave_speed_bounds_the_maximum_eigenvalue`.
- **Periodic boundaries:** a pulse that straddles x = 0, advanced by exactly one period
  (t = 1/|a|), must come back within the moved-correctly ratio of its initial data. Every
  unwritten ghost cell is NaN in checked builds (`m0-block`), so a scheme that reads one fails
  "finite first". Together these replace the old invariant test
  `ghost_cells_are_written_before_they_are_read`.
- **Self-tests in `reference`.** Each check has a bad scheme that fails it and a scheme that
  passes it, so a check that does nothing or always fails is caught:

  | Scheme | Fails | Passes |
  |---|---|---|
  | no-op | moved correctly | |
  | produces NaN | finite first | |
  | non-conservative | conservation | |
  | mirror-asymmetric | mirror symmetry | |
  | axis-dependent | rotation | |
  | nondeterministic | determinism | |
  | large-step (half the required steps) | step count | |
  | non-monotone (overshoots at the pulse) | maximum principle | |
  | wrong wrap (zero-gradient ghosts) | periodic boundaries | |
  | underestimated speed (reports \|a\|/2) | wave-speed bound | |
  | half speed, reversed | moved correctly | |
  | twice the diffusion | error ceiling | |
  | known order p: exact + c Δx^p g | order, for p below 1 − δ | order, for p = 1 |
  | shift (upwind at ν = 1, exact on whole cells) | | mirror symmetry, rotation, determinism, conservation, maximum principle |
  | exact | | every check except the two exemptions |

## Sessions

`thresholds-file`, `reference-suite` and `m0-reference-advection` are references (ADR-0007). They
are written by sessions that claim no other task of this plan, with no shared context with an
implementer, and merge only with the maintainer's approval. That is checked in review: the claim
records each session. Today it cannot, because nothing sets `NAMI_SESSION_ID` and every claim
reads `session: -`, so `claim-session-id` comes first: the session-start hook exports the Claude
Code session id (through `CLAUDE_ENV_FILE`), and `cargo xtask task claim` records it. The
reference tasks depend on it.

## Changes to the guardrails plan

Six tasks of `guardrails` overlap with M0. This plan changes them as follows:

- `thresholds-file` and `reference-suite` move here and are narrowed to code in
  `crates/reference/`. `thresholds-file` writes Rust constants (ADR-0042), not a TOML file.
  The two invariant tests of the old `reference-suite`, `wave_speed_bounds_the_maximum_eigenvalue`
  and `ghost_cells_are_written_before_they_are_read`, stay in `reference`, as the wave-speed-bound
  and periodic-boundary checks with their bad schemes (see "How the references plug in"), so
  `tier-tag` retags the style guide to checks that exist.
- `tier-tag` stays in `guardrails`. It now depends on `glossary` (the end of that chain) and on
  this plan's reference tasks, and it also retags `[planned:thresholds-file]` and
  `[planned:reference-suite]` in the style guide.
- `alloc-after-init` and `determinism-hash` stay in `guardrails`, with dependencies on this plan's
  solver tasks.
- `floor-activation-count` waits until the end of M0. Its first real target is the first-order
  flux correction in M2.

This keeps every edit of `registry.rs`, `TIGER_STYLE.md` and `RULES.md` on the one chain, while
the code of M0 runs in parallel beside it.

## Order

```
miri (strict-rust) ── m0-scaffold
m0-scaffold ─┬─ claim-session-id ── thresholds-file ── reference-suite ── m0-reference-advection
             ├─ m0-rows ─┬─ m0-reconstruct ─┐                                      │
             │           ├─ m0-update ──────┤                                      │
             │           ├─ m0-integrator ──┤                                      │
             │           └─ m0-block ───────┤                                      │
             ├─ m0-system ── m0-riemann ────┼─ m0-time-loop (also after m0-reference-advection)
             └─ m0-config ──────────────────┘
m0-time-loop ─┬─ m0-wire-references (also after m0-reference-advection)
              ├─ m0-cli (also after m0-output, m0-diagnostics) ── m0-restart
              └─ alloc-after-init (guardrails)
m0-config ── m0-output;   m0-scaffold ── m0-diagnostics
glossary (guardrails) ── tier-tag ── alloc-after-init ── determinism-hash ── m0-style-guide
```

`m0-scaffold` waits for `miri`, the end of the `strict-rust` chain, so that M0's code is written
under that plan's lints and no two ready tasks own the same files. `m0-time-loop` waits for
`m0-reference-advection`, so that the references land before the solver they test (ADR-0007).

## Tasks

Acceptance tests are named `<crate>::<module or test file>::<test>`.

### m0-scaffold

Create the crate structure, so that later tasks own disjoint files.

- New crate `crates/mesh` (`std`, allocates only at init), with empty modules `block`, `pool`,
  `ghost`.
- Empty modules, each with a doc comment saying what will live there and which ADR governs it:
  - `base::rows`;
  - `kernels::{system, advection, riemann, reconstruct, coordinates, integrator}`;
  - `solver::{config, output, build_info, diagnostics, problem, time_loop, cli, restart}`;
  - `reference::{thresholds, contract, checks, advection}`.
- Dependencies: `kernels` → `base`, `libm`; `mesh` → `base`; `solver` → `base`, `kernels`,
  `mesh`; `solver` dev-dependencies → `reference`, `alloc-guard`; `reference` → `base` only (it
  needs no `libm`: see "How the references plug in").
- A test over the crate graph, `xtask/tests/crate_graph.rs`, using `cargo metadata`:
  - `kernels` depends on nothing but `base` and `libm`;
  - `reference` depends on nothing but `base`, so that it shares no code with what it checks
    beyond the index types.

  The compiler does not enforce either (ADR-0038).

The manifests and `Cargo.lock` are protected: put them in a commit of their own.

### claim-session-id

Make a claim record which session holds it, so that review can check "Sessions".

- The session-start hook (`cargo xtask hook`, ADR-0019) reads `session_id` from its payload and
  appends `export NAMI_SESSION_ID=<id>` to the file named by `CLAUDE_ENV_FILE`, so that every
  later command of the session sees it. Without `CLAUDE_ENV_FILE` it does nothing and says so.
- `cargo xtask task claim` refuses to claim without `NAMI_SESSION_ID` when it runs under Claude
  Code (`CLAUDECODE` is set), instead of writing `session: -`.

`xtask/` is protected: the PR waits for the code owner.

### thresholds-file

`crates/reference/src/thresholds.rs`: the one place for test tolerances (ADR-0042, replacing
ADR-0007's `thresholds.toml`).

- Typed `pub const`s. Each doc comment derives the value or cites its source.
- The values M0 needs:
  - the conservation constant C_mass, with its derivation (a few roundings per cell per step);
  - δ = 0.1 for the observed order (ADR-0007);
  - the error-ceiling factor of 1.5 for first-order upwind (see "How the references plug in");
  - the Courant number ν = 0.45 of the reference runs;
  - the moved-correctly ratio ½ and its absolute L1 ceiling;
  - the step-count round-off factor 4ε;
  - the maximum-principle round-off margin, derived like C_mass.
- A test that every threshold is finite and positive, and a test that names each constant above
  and checks its value against its derivation, so that an empty file fails.
- `reference-suite` and `m0-reference-advection` also own this file, in sequence, for any
  constant they find they need; tolerances live nowhere else (ADR-0042).
- Does **not** edit `TIGER_STYLE.md` or the registry: `tier-tag` retags the style guide.

Written by a reference session (see "Sessions").

### reference-suite

`reference::contract` and `reference::checks`: the contract, the declared properties, the
mapping from properties to checks, and the generic checks:

- finite first;
- conservation;
- step count;
- determinism;
- mirror symmetry;
- rotation across axes;
- the maximum principle;
- the wave-speed bound;
- one check per declared boundary type. For periodic boundaries: a pulse straddling x = 0,
  advanced by one whole period, returns within the moved-correctly ratio of its initial data.

Prove the suite with the bad schemes of "How the references plug in", written inside
`reference`'s tests, each failing the check it targets, and with the shift scheme as the positive
control (one whole period needs no exact solution beyond the initial data). Each check prints one
`ERROR` line on failure (ADR-0007). Written by a reference session.

### m0-reference-advection

`reference::advection`: exact cell averages, computed analytically, for the smooth polynomial
profile u₀(x) = 16 x² (1 − x)² and the square pulse, shifted by a t, periodic. It also adds the
advection checks:

- moved correctly;
- observed order with its error ceiling 1.5 × t D ‖u₀″‖₁;
- `check_advection(properties)`, which runs everything the properties imply.

No transcendental function is needed, so `reference` uses neither `libm` nor `kernels::math`. The
self-tests: half speed, reversed direction and double diffusion fail; the known-order scheme
passes the order check at p = 1 and fails it below 1 − δ; the exact scheme passes every check
except its two exemptions. Written by a reference session.

### m0-rows

`base::rows`: typed row access into a structure-of-arrays block (ADR-0043).

- A block shape (cells and ghost cells per axis).
- The row of a variable at (j, k), contiguous in i.
- The stencil of rows around (j, k) along y or z.
- Lengths asserted once per row.

This is the only place where the offsets of the `[var][k][j][i]` layout are computed.

### m0-system

- `kernels::system`:
  - the `EquationSystem` trait: associated `Conserved`, `Primitive` and `Parameters` types; the
    conversions between them; the physical flux along the face normal;
  - the `WaveSpeedBounds` trait;
  - `Direction` and the face-frame permutation, which is exact (ADR-0037).
- `kernels::advection`: linear advection. One scalar state, with the velocity in `Parameters`,
  permuted with the face.

### m0-riemann

`kernels::riemann`: the `RiemannSolver<E>` trait, and LLF for every system with `WaveSpeedBounds`.
The solver works on one face in the face frame and is called in a loop over a row. It asserts
that the wave speed it uses is at least the system's bound (TIGER_STYLE.md, Physical
admissibility).

### m0-reconstruct

`kernels::reconstruct`: the `Reconstruction` trait, with the ghost width as an associated
constant, and donor cell, as a row kernel from cell rows to left and right face-state rows.

### m0-update

- `kernels::coordinates`: the `Coordinates` trait (face areas, cell volumes, geometric source
  terms) and `Cartesian`, whose areas and volumes are constants that fold away.
- `kernels::update`: the row update in area/volume form.

Add the new update **beside** `update_pencil`. `m0-style-guide` removes `update_pencil` once the
style guide's template points at the new kernel, so the guide never names code that does not
exist.

### m0-integrator

`kernels::integrator`: stage tables in the low-storage 3S* form (Ketcheson 2010) and the row kernel
that combines registers for one stage. M0 has one table, forward Euler. The driver that runs a
table over blocks lives in `solver::time_loop`.

### m0-block

- `mesh::block`: a block's shape, its geometry (origin and spacing), and its state in the layout
  of `base::rows`.
- `mesh::pool`: blocks allocated once from a memory budget. Exhaustion is an operating error
  (`Result`). New storage is filled with NaN in `dev`, `test` and `release-checked` builds
  (TIGER_STYLE.md, Ghost cells).
- `mesh::ghost`: periodic ghost fill along each active axis. Every ghost cell is written before
  the step reads it.

There is no tree in M0. The Morton-key octree arrives with many blocks (M4).

### m0-config

`solver::config`: the strict TOML subset (ADR-0041). It supports tables, strings, integers,
floats and booleans.

- No defaults. Missing keys, unknown keys and wrong types are errors whose message names the file,
  the line and the fix.
- `--set key=value` overrides.
- Rejects a Courant number outside (0, 1], a speed that is zero or not finite, and zero cells,
  each with a message that names the key and the allowed range.
- The resolved configuration can be serialised back, for output headers.

### m0-output

- `solver::output`: nami's binary format (ADR-0041). The header holds:
  - `format_version` and `results_version`;
  - the build fields;
  - the resolved configuration;
  - the block layout.

  The data is raw little-endian `f64`, each block at an offset computed in advance and written
  with `write_at`. It includes a reader for the round-trip tests.
- `solver::build_info` and `crates/solver/build.rs` (`std` only): the commit and dirty flag, the
  `rustc` version, the `Cargo.lock` hash, the target and its CPU flags.

The numpy reader in `tools/` is a separate later task.

### m0-diagnostics

`solver::diagnostics`: per-step JSON Lines and the one-line `ERROR` format on stderr. The buffers
are allocated at init, and writing a step does not allocate.

### m0-time-loop

- `solver::problem`: the `Problem` trait (initial conditions; user boundaries and sources later),
  and the combination type that names the extension points (ADR-0037).
- `solver::time_loop`:
  - `Solver::init`, which validates, allocates every pool and register and sets the initial
    conditions;
  - `step`, which runs the integrator table over the phases of ADR-0038: ghost fill, fluxes per
    active direction, update, then the per-block backstop;
  - the CFL time step, with the last step shortened to end exactly.

### m0-cli

`solver::cli`: `run <config>` and `check <config>`, with `--set`. Outputs and diagnostics are
written at configured intervals. It includes an example problem binary,
`crates/solver/examples/advection.rs` (the smooth polynomial profile and the pulse), and tests
that drive the commands in process.

### m0-restart

`restart <checkpoint>`. A checkpoint uses the output format. Restart compares the checkpoint's
build fields with the running build and refuses on any difference that affects bits.
`--allow-different-build` overrides, and the output records it. Test: a run that stops at a
checkpoint and restarts gives the same bits as one that does not stop.

### m0-wire-references

`crates/solver/tests/advection_reference.rs`: an adapter from the solver to `reference::Scheme`,
and one line per combination. M0 has one combination: advection, donor cell, LLF, forward Euler,
Cartesian, periodic. Its declared properties are formal order 1, monotone, and periodic
boundaries.

### m0-style-guide

Point `TIGER_STYLE.md`'s kernel template and its section "Data access in kernels" at the row
kernel of `m0-update` (ADR-0043), and remove `update_pencil`. This task sits on the chain that
owns the style guide, after `determinism-hash`. Its own test, in
`xtask/tests/style_guide_kernel.rs`, fails while `crates/kernels` defines `update_pencil` or
while the guide's kernel template does not call the row kernel, so a no-op does not pass.
