# Plan: 1D linear advection

The first equation system, end to end. It is the cheapest complete test of the guardrails:

- an exact solution exists for every time;
- the convergence rates are textbook results;
- every piece of later systems is already here: configuration, `init`, ghost cells, fluxes, conservative update, time step, time integrator and references.

## Problem

ū is the cell average of u. The problem is

  ∂u/∂t + a ∂u/∂x = 0,   x ∈ [0, 1) periodic,   a ∈ ℝ constant (either sign, a ≠ 0).

The exact solution is u(x, t) = u₀(x − a t), periodic. The domain has N uniform cells: Δx = 1/N, and cell i covers [i Δx, (i+1) Δx).

### Discretisation

The update is in conservative form (LeVeque, *Finite Volume Methods for Hyperbolic Problems*, §4.1):

  ū_i ← ū_i − (Δt/Δx) (F_{i+½} − F_{i−½}).

There is exactly one flux per interface, shared by both neighbours.

- **Upwind flux.** It is the exact Riemann solution for linear advection (LeVeque §4.10): F(u_L, u_R) = a u_L if a > 0, else a u_R.
- **First order:** ū at both sides of each interface, with forward Euler.
- **Second order (MUSCL):**
  - One slope σ_i per cell: u_{i+½}^L = ū_i + σ_i/2 and u_{i−½}^R = ū_i − σ_i/2.
  - σ_i is built from the one-sided differences Δ⁻ = ū_i − ū_{i−1} and Δ⁺ = ū_{i+1} − ū_i (LeVeque §6.8–6.12):
    - **unlimited (Fromm):** σ = (Δ⁻ + Δ⁺)/2;
    - **minmod:** σ = minmod(Δ⁻, Δ⁺);
    - **monotonised central (MC):** σ = minmod((Δ⁻ + Δ⁺)/2, 2Δ⁻, 2Δ⁺).

    Here minmod is zero unless all its arguments have the same sign. If they do, it is the argument of smallest magnitude.
  - Time integration is the SSP-RK2 (Heun) method of Gottlieb, Shu and Tadmor (2001): u⁽¹⁾ = uⁿ + Δt L(uⁿ), then uⁿ⁺¹ = ½ uⁿ + ½ (u⁽¹⁾ + Δt L(u⁽¹⁾)). Ghost cells are refilled before each stage.
- **Time step.** Δt = ν Δx / |a|, where ν is the configured Courant number. The last step is shortened so that the run ends exactly at the requested time, and no step is ever longer than ν Δx / |a|.
- **Ghost cells.** Two per side, filled periodically before every stage. That is enough for the MUSCL stencil: a cell's slope needs its neighbours, and an interface needs the slopes of both its cells.

### Data layout

- Each field is a `Box<[f64]>` of N + 4 values, allocated in `init`.
- Kernels see the field through `array_windows`/`chunks_exact` (ADR-0003). No kernel indexes a slice.
- The flux buffer holds N + 1 interface values.
- Everything after `init` is allocation-free (ADR-0003). Task `advection1d-no-allocation` proves it.

### Rules that apply

- ADR-0002: counts are `CellCount`.
- ADR-0003: kernels are `no_std`; data access as above.
- ADR-0004: assertions.
- ADR-0005: determinism. Sums use `sum_fixed`/`sum_compensated`, with no `mul_add`.
- ADR-0007: references.
- `TIGER_STYLE.md`, "Numerical code":
  - conservative form;
  - no silent floors;
  - NaN is a bug;
  - configuration spelled out in full, with no `Default`.

## How the references plug in

The reference task defines a contract that any 1D advection scheme can implement, together with a suite of generic checks. The sketch below is where the reference task starts; the reference task owns the final API.

```rust
/// What the checks need from a scheme (defined in `reference::advection1d`).
pub trait Scheme {
    /// A periodic run on [0, 1) with `cells` cells, wave speed `speed` and Courant number
    /// `courant`, starting from the given cell averages.
    fn new(cells: CellCount, speed: f64, courant: f64, initial: &[f64]) -> Self;
    /// Advances to exactly `time` (≥ the current time).
    fn advance_to(&mut self, time: f64);
    /// The current cell averages, `cells` values.
    fn cell_averages(&self) -> &[f64];
    /// Time steps taken so far.
    fn steps(&self) -> u32;
}

/// Runs every check that applies to a scheme of the given kind; panics on the first failure,
/// with one `ERROR …` line.
pub fn check_scheme<S: Scheme>(kind: Kind);
```

Each solver task wires its scheme into the suite with one adapter and one call per scheme, in `crates/solver/tests/advection1d.rs`. The wiring cannot pick and choose checks. Which checks apply depends only on the declared `Kind` (first order, second order unlimited, or second order TVD), and the reference crate maps each `Kind` to its checks.

## Open question for the maintainer

- **Thresholds.** ADR-0007 puts thresholds in `reference/thresholds.toml`. Parsing TOML needs either a dependency (ADR-0001) or a hand-written parser. ADR-0016 (Proposed, in this PR) proposes Rust constants in `crates/reference/src/thresholds.rs` instead. The file is just as code-owned, and the constants are typed and documented with their derivations. The reference task follows whichever you accept, and assumes ADR-0016 unless told otherwise.

## Tasks

Acceptance tests are named `<crate>::<module or test file>::<test>` in `tasks.json`. Each task uses exactly those names, so that CI can find them.

### advection1d-reference

Write `reference::advection1d`: exact solutions, the `Scheme` contract and the check suite. This task is written by a session that will not implement the solver (ADR-0007).

- **Exact cell averages at time t** (`exact_cell_averages(profile, speed, time, cells) -> Box<[f64]>`), computed analytically:
  - **sine** sin(2πk x), with integer k ≥ 1. Its cell average over [x_l, x_r] is (cos 2πk x_l − cos 2πk x_r) / (2πk Δx), evaluated at the shifted positions x − a t taken modulo 1.
  - **square pulse** that is 1 on [p, q) and 0 elsewhere, periodic. Its cell average is the overlap length of the cell with the shifted pulse, divided by Δx.
  - Use `libm` directly for sin/cos, not the solver's `kernels::math`, so that the reference shares no numerical code with what it checks.
- **Checks** each return `Result<(), String>`, and `check_scheme` panics with the first error. Every check uses at least two resolutions and both signs of a, unless noted.
  - **Conservation.** Σ ū Δx after a run equals the initial value, within a few ULP of the total. Compare with `kernels::reduce::sum_compensated`, or an equivalent compensated sum written in this crate.
  - **Moved correctly.**
    - Setup: a square pulse advanced by a shift that is not a multiple of the period (for example a t = 0.3).
    - The L1 distance to the exact solution must be at most half the L1 distance from the initial data to the exact solution. A scheme that does nothing therefore fails.
    - The distance must also be under an absolute ceiling.
  - **Order of accuracy** (smooth data only).
    - Setup: sine with k = 1, t = 0.25 (not a full period, so a no-op fails), N = 32, 64 and 128.
    - The observed L1 order on the finest pair must be at least p − δ, with δ = 0.1 (ADR-0007): p = 1 for first order, p = 2 for unlimited second order.
    - The finest error must be under a ceiling.
    - Only for first order and for second order unlimited. Limiters clip smooth extrema, so the limited schemes get the maximum-principle and moved-correctly checks instead.
  - **Maximum principle** (first order and TVD only). From square-pulse data, every cell average stays within [min ū⁰, max ū⁰], up to round-off.
  - **Mirror symmetry.** Run with a and initial data ū_i, and again with −a and the mirrored data ū_{N−1−i}. The results are mirrors of each other, **bitwise** (ADR-0007, symmetry tier).
  - **Determinism.** Two identical runs give bitwise-identical results.
  - **Step count.** `steps()` is at least ⌈t |a| / (ν Δx)⌉ for t, ν and Δx as configured. A scheme cannot take steps longer than the CFL limit and still pass.
- **Thresholds** (see the open question above). Derive every error ceiling from theory, never from a solver's output, and document the derivation next to the constant.
  - For first-order upwind, the modified equation gives the numerical diffusion a Δx (1 − ν)/2 (LeVeque §8.6). The ceiling is about twice the resulting amplitude error.
  - For the other schemes, use the leading truncation-error term, or a published measurement with its citation.
  - Margins are generous enough to tolerate round-off and tight enough that a factor-of-two error in the scheme fails.
- **Self-tests**, which are this task's acceptance tests:
  - An *exact scheme* (it returns `exact_cell_averages` at the current time) passes every check of every kind.
  - A *no-op scheme* fails the moved-correctly and order checks.
  - A *reversed scheme* (one that advects with −a) fails the moved-correctly check.
  - A *leaky scheme* (it multiplies every average by 1 − 10⁻⁶ each step) fails conservation.
  - A *large-step scheme* (it takes one step to the end) fails the step-count check.
- `crates/reference/Cargo.toml` may depend on `base`, `kernels` and `libm`. That is within ADR-0001, but it changes the lockfile.

### advection1d-upwind

First-order upwind with forward Euler, as the first vertical slice through `kernels` and `solver`.

- **`kernels::advection`:**
  - the upwind flux of one interface;
  - the flux sweep over a field with ghosts, writing N + 1 interface fluxes through `array_windows`;
  - the conservative update from the fluxes.

  No allocation, no indexing.
- **`solver::advection1d`:**
  - `Config` holds the cell count, speed, Courant number and scheme, all required, with no `Default`.
  - `Config` validation returns an error for each of: zero cells, a non-finite or zero speed, a Courant number outside (0, 1], and an unsupported scheme.
  - `Solver::init` allocates every buffer.
  - The solver provides periodic ghost filling, the time step that lands exactly on the target time, `advance_to`, `cell_averages` and `steps`.
- **Wiring.**
  - Add `crates/solver/tests/advection1d.rs` with the reference adapter and `check_scheme::<Upwind>(Kind::FirstOrder)`.
  - Add `reference` as a dev-dependency of `solver`.
- **Unit tests** in `kernels` for the flux (both signs of a, and equal states) and for the update (a constant state stays constant, and the mass change equals the boundary fluxes).

### advection1d-muscl

Second-order MUSCL with SSP-RK2, with the three slopes from the discretisation section above.

- `kernels::reconstruction`: the three slopes, and the interface states from the slopes. Limiters must not divide.
- `solver::advection1d`: SSP-RK2 for the second-order schemes, with ghost cells refilled before each stage. The `Scheme` choice becomes upwind, Fromm, minmod or MC.
- Wiring: three more calls in `crates/solver/tests/advection1d.rs`, one per slope:
  - `Kind::SecondOrderUnlimited` for Fromm;
  - `Kind::SecondOrderTvd` for minmod and MC.
- Unit tests in `kernels` for each slope:
  - the zero slope at an extremum;
  - the slope on linear data (for MC and Fromm, equal to the exact difference);
  - the minmod sign cases.

### advection1d-no-allocation

Prove that nothing allocates after `init` (ADR-0003, ADR-0014).

- `crates/solver/tests/advection1d_no_allocation.rs`:
  - installs `alloc_guard::CountingAllocator` as the global allocator;
  - builds a solver for each scheme;
  - wraps `advance_to` over many steps in `alloc_guard::assert_no_allocations`.
- Add `alloc-guard` as a dev-dependency of `solver`.
- This task does not touch `crates/solver/src/`. If the test finds an allocation, record it in this plan and stop. Do not fix `src/` here, because that belongs to the task that owns it.
