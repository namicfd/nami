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
- **Courant limits.** Each scheme has a largest ν for which it is stable, and for the limited schemes also TVD:

  | Scheme | Limit | Why |
  |---|---|---|
  | upwind with forward Euler | ν ≤ 1 | LeVeque §4.4 |
  | Fromm with SSP-RK2 | ν ≤ 1 | von Neumann analysis |
  | minmod with SSP-RK2 | ν ≤ 2/3 | Harten's incremental form: C = ν(1 + ψ(θ_i)/2 − ψ(θ_{i−1})/(2θ_{i−1})), with ψ and ψ(θ)/θ both in [0, 1]. Forward Euler is TVD when 0 ≤ C ≤ 1, which holds for ν(1 + ψ_max/2) ≤ 1. SSP-RK2 has SSP coefficient 1, so it keeps the forward-Euler limit. |
  | MC with SSP-RK2 | ν ≤ 1/2 | the same argument, with ψ and ψ(θ)/θ both in [0, 2] |

  These are Harten's sufficient conditions for TVD, not necessarily the largest TVD ν. `Config` rejects a Courant number above its scheme's limit.
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
    /// `courant`, starting from `initial`.
    fn new(cells: CellCount, speed: f64, courant: f64, initial: &Initial) -> Self;
    /// Advances to exactly `time` (≥ the current time).
    fn advance_to(&mut self, time: f64);
    /// The current cell averages, `cells` values.
    fn cell_averages(&self) -> &[f64];
    /// Time steps taken so far.
    fn steps(&self) -> u32;
}

/// Initial data. Schemes may read only `cell_averages()`. The profile it was built from is
/// private to the reference crate, so that only the reference's own exact scheme can use it,
/// and a solver cannot cheat by evaluating the exact solution.
pub struct Initial { /* cell averages, and a crate-private profile */ }

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
- **Checks** each return `Result<(), String>`, and `check_scheme` panics with the first error. Every check uses both signs of a, at least two resolutions (none coarser than N = 32), and **ν = 0.45** unless noted.
  - **Finite first.** Every check first requires each cell average, error, mass and observed order it computes to be finite, and fails otherwise. Each comparison is written so that NaN fails, never `!(x < bound)`, which NaN passes. A solver that produces NaN must fail every check.
  - **Why ν = 0.45:** it is below every scheme's limit in the table above. It also makes t|a|/(νΔx) a non-integer at every tested N for t = 0.25 and t = 0.3. With a dyadic ν such as 0.5, the step counts come out as whole numbers, and a solver that never shortens its last step would pass.
  - **Why ν < 1:** at ν = 1, upwind is an exact shift, and its observed order and error ceiling both collapse to zero.
  - **Conservation.**
    - The mass is M = Σ ū Δx, and the scale is Σ|ū⁰| Δx.
    - Required: |M_end − M_0| ≤ C_mass · ε · scale · steps, where ε is `f64::EPSILON`. Each step can change M by a few roundings per cell, so the bound is linear in the number of steps.
    - C_mass is derived and documented in the thresholds.
    - A tolerance in ULP of M itself is wrong: on zero-mean data such as the sine, M is about 1e-18, and correct schemes drift by about 1e-16.
    - Use compensated sums (`kernels::reduce::sum_compensated`, or an equivalent written in this crate). Test on both the sine and the pulse.
  - **Moved correctly.**
    - Setup: the square pulse on [0.3, 0.45), advanced by |a| t = 0.3. The shift is not a multiple of the period, and it is twice the pulse width.
    - Required: the L1 distance to the exact solution is at most half the L1 distance from the initial data to the exact solution, and under an absolute ceiling.
    - With this width, correct schemes stay at or below a ratio of 0.36, while the no-op, the reversed scheme and ±50% speed errors reach 0.78 or more (measured in the review prototype). A narrower pulse fails correct schemes at coarse N; a pulse as wide as the shift lets speed errors through.
  - **Order of accuracy** (smooth data only, for every kind).
    - Setup: sine with k = 1, t = 0.25 (not a full period, so a no-op fails), N = 32, 64 and 128.
    - The observed L1 order on the finest pair must be at least:
      - first order: p ≥ 1 − δ;
      - second order unlimited: p ≥ 2 − δ, with δ = 0.1 (ADR-0007);
      - second order TVD: p ≥ 1.5. Limiters clip the smooth extrema, which costs some accuracy but not first order.

      That lower bound for TVD is what separates a working minmod or MC from a slope that is always zero, or one used without its factor of ½. For reference, the review prototype measured these orders on this data at ν = 0.45:
      - minmod 1.87 and MC 2.02;
      - flat-slope MUSCL with SSP-RK2 0.97;
      - full-slope ū ± σ: minmod 1.00, MC 0.61, Fromm 1.03.

      Record the bound and its reasoning in the thresholds.
    - The finest error must also be under a ceiling, one per kind.
  - **Maximum principle** (first order and TVD only). From square-pulse data, every cell average stays within [min ū⁰, max ū⁰], up to round-off.
  - **Mirror symmetry.** Run with a and initial data ū_i, then with −a and the mirrored data ū_{N−1−i}. The results must be mirrors of each other, **bitwise** (ADR-0007, symmetry tier). The review prototype matched bitwise in all 648 cases it tried, across every scheme.
  - **Determinism.** Two identical runs give bitwise-identical results.
  - **Step count.** `steps()` must be at least ⌈(t |a| / (ν Δx)) · (1 − 4ε)⌉. The factor keeps a correct solver from failing when the exact quotient is an integer. This bounds the number of steps, so a scheme that covers the run in fewer, longer steps fails. It does not bound the longest single step.
- **Thresholds** (see the open question above). Derive every error ceiling from theory, never from a solver's output, and document the derivation next to the constant.
  - For first-order upwind, the modified equation gives the numerical diffusion |a| Δx (1 − ν)/2 (LeVeque §8.6). The ceiling is **at most 1.5 times** the resulting amplitude error, so a scheme with twice the diffusion fails. At ν = 0.45, N = 128 and t = 0.25, the review prototype predicted (2/π)(1 − exp(−4π² · |a|Δx(1 − ν)/2 · t)) = 1.3357e-2 and measured 1.3373e-2.
  - For the other schemes, use the leading truncation-error term, or a published measurement with its citation.
  - Margins are generous enough to tolerate round-off and tight enough that a factor-of-two error in the scheme fails.
- **Self-tests**, which are this task's acceptance tests:
  - An *exact scheme* reads the private profile in `Initial` and returns `exact_cell_averages` at the current time. It reports `steps()` = ⌈t|a|/(νΔx)⌉, as if it stepped at the CFL limit.
    - It passes conservation, moved-correctly, the maximum principle, determinism and the step count, for every kind.
    - It is exempt from two checks:
      - **Mirror symmetry:** exact cell averages of the mirrored problem agree only up to rounding, not bitwise.
      - **Order of accuracy:** its error is exactly zero, so the observed order is log₂(0/0).
  - The order check gets its own self-tests. A *known-order scheme* returns the exact cell averages plus c·Δx^p·g, for a fixed smooth zero-mean g. Its error is c·Δx^p·‖g‖₁, so its observed order is p up to rounding.
    - With p = 2, it passes the order check of every kind.
    - With p = 1, it passes first order and fails both second-order kinds.
    - A *NaN scheme*, whose averages are all NaN, fails every check.
  - The mirror check gets its own two self-tests:
    - a *shift scheme*, upwind at ν = 1, passes it. Every full step moves each average by exactly one cell. A shortened last step is no longer a pure shift, but it is still mirror-symmetric;
    - a *one-sided scheme*, upwind that always takes the left state whatever the sign of a, fails it.
  - A *no-op scheme* fails the moved-correctly and order checks.
  - A *reversed scheme* (one that advects with −a) fails the moved-correctly check.
  - A *flat-slope scheme* (MUSCL with σ ≡ 0) fails the second-order TVD order check.
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
  - `Config` validation returns an error for each of:
    - zero cells;
    - a non-finite or zero speed;
    - a Courant number outside (0, limit], where the limit comes from the table in "Discretisation";
    - an unsupported scheme.
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
  - the slope on integer-valued linear data, which equals the exact difference for all three slopes and is compared bitwise;
  - the minmod sign cases.

### advection1d-no-allocation

Prove that nothing allocates after `init` (ADR-0003, ADR-0014).

- `crates/solver/tests/advection1d_no_allocation.rs`:
  - installs `alloc_guard::CountingAllocator` as the global allocator;
  - builds a solver for each scheme;
  - wraps `advance_to` over many steps in `alloc_guard::assert_no_allocations`.
- Add `alloc-guard` as a dev-dependency of `solver`.
- This task does not touch `crates/solver/src/`. If the test finds an allocation, record it in this plan and stop. Do not fix `src/` here, because that belongs to the task that owns it.
