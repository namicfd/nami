# Plan M0: linear advection, the walking skeleton

## Goal

The first vertical slice of nami (ADR-0023, milestone M0). It solves 1D linear advection on the
real architecture, so that every later milestone extends this code instead of rewriting it. It
also puts the task, claim and review process through its first full run.

M0 is **thin on methods and complete in structure:**

- **Methods:** donor cell reconstruction, the LLF Riemann solver, forward Euler, periodic
  boundaries. PLM, the limiters, HLLE and the Runge–Kutta integrators come in a later plan (M0b).
- **Structure:**
  - the 3D data model, with one block that is 1D along x, y or z;
  - structure of arrays and row kernels (ADR-0031);
  - the extension-point traits (ADR-0025);
  - the configuration, output with version metadata, checkpoint and restart (ADR-0029);
  - diagnostics;
  - the reference suite chosen by declared properties (ADR-0030).

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

- **Conservative update** in area/volume form (ADR-0023):
  ū_i ← ū_i − (Δt / V_i) (A_{i+½} F_{i+½} − A_{i−½} F_{i−½}).
  It has one flux per face, shared by both neighbours. On a Cartesian grid, A = Δy Δz and
  V = Δx Δy Δz.
- **Donor cell:** the face states are the adjacent cell averages. The ghost width is 1.
- **LLF flux:** F(u_L, u_R) = ½ (a u_L + a u_R) − ½ s (u_R − u_L), where s = |a_n| is the
  wave-speed bound of linear advection along the face normal. For linear advection this equals
  the upwind flux, which is the exact Riemann solution (LeVeque, *Finite Volume Methods for
  Hyperbolic Problems*, §4.10), up to round-off.
- **Forward Euler,** as a one-stage table in the low-storage 3S* form (ADR-0025), so that M0b only
  adds tables.
- **Time step:** Δt = ν Δx / |a|, where ν is the configured Courant number. The last step is
  shortened so that the run ends exactly at the requested time, and no step is longer than
  ν Δx / |a|. Upwind with forward Euler is stable and monotone for ν ≤ 1 (LeVeque §4.4), and the
  configuration rejects ν > 1.

### Performance estimate (TIGER_STYLE.md, Performance)

Per cell per step: read ū and two face fluxes, write one flux and ū. That is about 40 bytes and
6 flops: memory-bound, around 10⁹ cell updates per second per core. Advection is never the
benchmark. M0 sets no speed target; the Athena++ comparison starts with Euler (M4, ADR-0022).

## Rules that apply

- ADR-0002: indices and counts are `u32` newtypes.
- ADR-0003, as amended by ADR-0031: `kernels` is `no_std` without `alloc`; data access through row
  stencils, named state structs and typed gathers; no raw indexing.
- ADR-0004: assertions. Checks inside per-face loops follow the ADR on hot-loop checks (ADR-0032,
  being written: checks stay on in release builds, accumulated without branching and asserted
  per row, with an explicit cargo feature to opt out).
- ADR-0005: determinism.
- ADR-0007 and ADR-0030: references.
- ADR-0025: extension points; ADR-0026: blocks, pools, phases; ADR-0029: configuration, output,
  restart.
- TIGER_STYLE.md, "Numerical code".

## How the references plug in

`reference` defines a **contract** that any scheme for a 1D conservation law can implement, and
**checks chosen by declared properties** (ADR-0030). A sketch to start from; the `reference-suite`
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

- **Finite first.** Every check first requires each value it computes to be finite, and writes
  comparisons so that NaN fails.
- **ν = 0.45.** It is below every scheme's limit, and it makes the step count non-integer at
  every tested N for t = 0.25 and 0.3.
- **Conservation.** |M_end − M_0| ≤ C_mass · ε · scale · steps, with compensated sums.
- **Moved correctly.** The pulse on [0.3, 0.45) advanced by 0.3. The L1 distance to the exact
  solution must be at most half the distance from the initial data.
- **Order.** Sine with k = 1, t = 0.25, N = 32, 64, 128: p ≥ p_formal − δ with δ = 0.1, plus an
  error ceiling derived from theory. For upwind, the modified equation gives the diffusion
  |a| Δx (1 − ν)/2, and the ceiling is at most 1.5× the resulting amplitude error.
- **Maximum principle** for monotone schemes.
- **Mirror symmetry,** bitwise.
- **Determinism,** bitwise.
- **Step count.** At least ⌈(t|a| / (νΔx)) (1 − 4ε)⌉ steps.

New in M0:

- **Rotation, bitwise:** the same problem along x, y and z gives identical cell averages
  (ADR-0030, § 7.4).
- **Bad-scheme self-tests in `reference`:** a no-op scheme, a scheme that produces NaN, a
  non-conservative one, a mirror-asymmetric one, an axis-dependent one, one at half speed, one
  reversed, and one with twice the diffusion must each fail the suite, and the exact scheme must
  pass it.

## Sessions

`thresholds-file`, `reference-suite` and `m0-reference-advection` are references (ADR-0007). They
are written by sessions that claim no other task of this plan, with no shared context with an
implementer, and merge only with the maintainer's approval. That is checked in review: the claim
records each session.

## Changes to the guardrails plan

Six tasks of `guardrails` overlap with M0. This plan changes them as follows:

- `thresholds-file` and `reference-suite` move here and are narrowed to code in
  `crates/reference/`. `thresholds-file` writes Rust constants (ADR-0030), not a TOML file.
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
m0-scaffold ─┬─ thresholds-file ── reference-suite ── m0-reference-advection ─┐
             ├─ m0-rows ─┬─ m0-reconstruct ─┐                                 │
             │           ├─ m0-update ──────┤                                 │
             │           ├─ m0-integrator ──┤                                 │
             │           └─ m0-block ───────┤                                 │
             ├─ m0-system ── m0-riemann ────┼─ m0-time-loop ─┬───────────────┴─ m0-wire-references
             ├─ m0-config ──────────────────┘                ├─ m0-cli ── m0-restart
             ├─ m0-output ── (m0-cli)                        │
             └─ m0-diagnostics ── (m0-cli)                   └─ alloc-after-init (guardrails)
glossary (guardrails) ── tier-tag ── alloc-after-init ── determinism-hash ── m0-style-guide
```

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
  `mesh`; `solver` dev-dependencies → `reference`, `alloc-guard`; `reference` → `base`, `libm`.
- A test over the crate graph, `xtask/tests/crate_graph.rs`, using `cargo metadata`:
  - `kernels` depends on nothing but `base` and `libm`;
  - `reference` does not depend on `kernels`, `mesh` or `solver`, so that it shares no code with
    what it checks.

  The compiler does not enforce either (ADR-0026).

The manifests and `Cargo.lock` are protected: put them in a commit of their own.

### thresholds-file

`crates/reference/src/thresholds.rs`: the one place for test tolerances (ADR-0030, replacing
ADR-0007's `thresholds.toml`).

- Typed `pub const`s. Each doc comment derives the value or cites its source.
- The values M0 needs:
  - the conservation constant C_mass, with its derivation (a few roundings per cell per step);
  - δ = 0.1 for the observed order (ADR-0007);
  - the error-ceiling factor of 1.5 for first-order upwind (see "How the references plug in").
- A test that every threshold is finite and positive.
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
- one check per declared boundary type. For periodic boundaries: a pulse that crosses the
  boundary arrives whole.

Prove the suite with bad schemes written inside `reference`'s tests, and prove that the exact
scheme passes. Each check prints one `ERROR` line on failure (ADR-0007). Written by a reference
session.

### m0-reference-advection

`reference::advection`: exact cell averages, computed analytically, for the sine sin(2πk x) and
the square pulse, shifted by a t, periodic. It also adds the advection checks:

- moved correctly;
- observed order with its error ceiling;
- `check_advection(properties)`, which runs everything the properties imply.

Use `libm` directly, not `kernels::math`, so the reference shares no code with the solver. The
self-tests: half speed, reversed direction and double diffusion fail; the exact scheme passes.
Written by a reference session.

### m0-rows

`base::rows`: typed row access into a structure-of-arrays block (ADR-0031).

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
  - `Direction` and the face-frame permutation, which is exact (ADR-0025).
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

`solver::config`: the strict TOML subset (ADR-0029). It supports tables, strings, integers,
floats and booleans.

- No defaults. Missing keys, unknown keys and wrong types are errors whose message names the file,
  the line and the fix.
- `--set key=value` overrides.
- The resolved configuration can be serialised back, for output headers.

### m0-output

- `solver::output`: nami's binary format (ADR-0029). The header holds:
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
  and the combination type that names the extension points (ADR-0025).
- `solver::time_loop`:
  - `Solver::init`, which validates, allocates every pool and register and sets the initial
    conditions;
  - `step`, which runs the integrator table over the phases of ADR-0026: ghost fill, fluxes per
    active direction, update, then the per-block backstop;
  - the CFL time step, with the last step shortened to end exactly.

### m0-cli

`solver::cli`: `run <config>` and `check <config>`, with `--set`. Outputs and diagnostics are
written at configured intervals. It includes an example problem binary,
`crates/solver/examples/advection.rs` (sine and pulse), and tests that drive the commands in
process.

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
kernel of `m0-update` (ADR-0031), and remove `update_pencil`. This task sits on the chain that
owns the style guide, after `determinism-hash`.
