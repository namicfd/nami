# Layout prototype: 3D Euler in three layouts, against Athena++

Measured input for `docs/ARCHITECTURE.md` § 4(a) and the ADR that will amend ADR-0003. Neither
is on `main` yet: both are in PR #45, and the section numbers of `ARCHITECTURE.md` cited here
are that PR's. The code is on the branch `research/layout-prototype` (commits `2f971e5`,
`f25084c` and `7d99172`, directory `research/layout-prototype/`, with a README that says how to
reproduce the campaigns). That branch is not merged.

## Result in short

- **Structure of arrays with row stencils is confirmed.** It is the fastest layout in every
  configuration except A and D, the two with libm's `arch` sqrt, where the hybrid escapes the
  SSE/AVX transition penalty by accident (finding 2). It is the only layout whose kernels LLVM
  vectorises in all three sweep directions.
  Once the Riemann loop can vectorise, SoA runs at 14.1 Mzc/s at 64³ and 12.3 Mzc/s at 128³: 1.9×
  Athena++'s fastest build (which uses `-ffast-math`), with nami's determinism rules intact. AoS and
  the hybrid reach 1.1× to 1.2×.
- **The layouts give bitwise-identical output,** and the L1 errors after one wave period match
  Athena++'s to all seven printed digits, at 64³ and at 128³.
- **With a copy of `kernels::math::sqrt`'s body, nami fails the target:** 0.10× (SoA) and
  0.07× (AoS) of Athena++ in the main campaign, or 0.9× in a rebuild that differs only in
  inlining. Either way it is below 1.0×, and which of the two you get is not under control. The
  copy has today's backend and asserts, but it is marked `#[inline(always)]` and lives in the
  benchmark's own crate; today's function has no inline attribute and sits in another crate,
  without LTO. Inlining moves these numbers by 9× (see "Side finding"), so the speed of the real
  cross-crate function is not measured here. Two properties of `sqrt`, not the layout, cause the
  shortfall:
  1. `libm::sqrt` with the `arch` feature is an inline-asm legacy-SSE `sqrtsd`. In a function that
     also runs AVX code, every call pays an SSE/AVX transition penalty (about 10× here). It is
     also opaque to LLVM, so it can never vectorise.
  2. The wrapper's `assert!`s on every call give each loop that calls `sqrt` a panic exit, so
     the loop vectoriser gives up on the HLLC loop.

  Replacing the backend with `f64::sqrt` (correctly rounded, same bits) gives 0.90×. Moving the
  per-call asserts to `debug_assert!`, as ADR-0004 already prescribes for per-cell checks, gives
  1.9×.
- **1.0× Athena++ is reachable,** with room to spare, but only after a decision on `sqrt`
  (below). The determinism rules themselves cost 13% of the `algebraic_*` build's speed (the CA
  data point: 1 − C/CA), which matches ADR-0005's estimate of 5 to 15%.
- **Per-face checks can stay on in release builds** and keep the loop vectorised, if they are
  deferred. Each face accumulates its checks into a flag without branching, and the flag is
  asserted once per row. That costs 4 to 5% against no release checks (SoA), and gives the same
  bits. See "Checks that stay on in release".
- **Block sizes known at compile time buy nothing.** SoA with const-generic dimensions
  (`Soa<Fixed<64>>`) and SoA with the same sizes given at runtime produce the same machine code,
  within one or two instructions per function, and run at the same speed: median ratio 0.99 to
  1.01 in every configuration and size.

## Setup

**Problem.** Athena++'s `linear_wave` problem (adiabatic hydro, `wave_flag = 0`, the sound wave,
amplitude 10⁻⁶) on the unit cube with periodic boundaries. With `ang_2` and `ang_3` unset,
Athena++ puts the wave vector along the grid diagonal; the wavelength is 1/√3 and one period
lasts t = 0.57735. γ = 5/3. One block of 64³ or 128³ cells, 2 ghost cells per side, one core.

**Scheme.** PLM on primitive variables with the van Leer limiter, HLLC, SSP-RK2 (Heun, Athena++'s
`integrator = rk2`), CFL 0.3, dt from the largest signal speed, the last step cut at `tlim`.
`physics.rs` ports Athena++ v24.0's `plm.cpp` (uniform-grid branch) and `hllc.cpp` operation by
operation, so the two codes do the same arithmetic per face. Both codes take 124 cycles for one
period at 64³ and 247 at 128³. nami's code has no density or pressure floors; Athena++'s floors
never trigger on this problem.

**Layouts.** All call the same per-face functions (`Primitive`/`Conserved` named-field structs,
`plm`, `hllc`) with the same loop structure: conserved to primitive (with the per-block backstop),
ghost fill, then one sweep per direction (PLM into face-state rows, HLLC over a row of faces, the
flux difference into a divergence array), then the Heun update.

| Variant | Layout | Transverse sweep |
|---|---|---|
| `soa` | `[var][k][j][i]` | three rows j−1, j, j+1 (or k−1, k, k+1), each contiguous in i |
| `aos` | `[k][j][i][var]`, one `[f64; 5]` per cell | the same three rows, of cells |
| `hybrid4`, `hybrid8` | `[k][j][i/W][var][i%W]` (AoSoA), W = 4 or 8 | the same rows, group by group; the x-sweep unpacks a row into plain rows and packs the result back |

**Constraints kept.**

- No raw slice indexing: clippy's `indexing_slicing` is denied, and clippy passes with the nami
  lint table. Rows come from `chunks_exact().nth()` and `get()`, cells from `array_windows` and
  zips of rows.
- `libm` only through a copy of `kernels::math` (`assert!` on the argument and the result), no
  `algebraic_*` and no `mul_add` (configurations A and B; C to G change exactly one rule each, as
  listed below). Unlike today's `kernels::math`, the copy marks `sqrt` and its backend call
  `#[inline(always)]` and is in the same crate as the kernels.
- `assert!` on lengths before every loop, `debug_assert!` per cell, and a per-block backstop on
  every stage (density and pressure positive and finite, as a branch-free `&` reduction).
- The workspace release profile (`overflow-checks = true`) and `-C target-cpu=x86-64-v3`.
- No allocation in the time loop: a counting global allocator asserts zero allocations on every
  run. Scratch arrays start as NaN, so that their pages are touched at init and any read of an
  unwritten cell would trip the backstop.

**Configurations.**

| Name | `math::sqrt` backend | Asserts in `math::sqrt` | Arithmetic |
|---|---|---|---|
| A (faithful body, `#[inline(always)]`) | `libm::sqrt` (`arch`: inline-asm `sqrtsd`) | `assert!` | IEEE |
| B | `f64::sqrt` (LLVM intrinsic, VEX `vsqrtsd`/`vsqrtpd`) | `assert!` | IEEE |
| C | `f64::sqrt` | `debug_assert!` | IEEE |
| D | `libm::sqrt` (`arch`) | `debug_assert!` | IEEE |
| G | `libm::sqrt` without `arch` (pure-Rust generic) | `debug_assert!` | IEEE |
| CA (data point) | `f64::sqrt` | `debug_assert!` | `algebraic_*` in PLM and HLLC |

A, B, C, D and G produce **identical bits** (same state hash after one period in every layout).
All three backends are correctly rounded IEEE square roots, so this is expected.

**Athena++.** v24.0, commit `209a9258ee158d1965557f1a5c4193101b704dd9`, configured with
`--prob=linear_wave --flux=hllc --eos=adiabatic`, no MPI, no OpenMP. Two builds:

- `--cxx=g++`: `-O3 -std=c++11`;
- `--cxx=g++-simd`: `-O3 -std=c++11 -fopenmp-simd -fwhole-program -flto -ffast-math
  -march=native -fprefetch-loop-arrays`.

The SIMD build is the faster one and is the baseline for every ratio below. Note that it uses
`-ffast-math`, which nami forbids. Input: `<time> integrator = rk2, xorder = 2, cfl_number = 0.3`,
one meshblock of the whole mesh, no outputs (the decks are in `research/layout-prototype/
athena-inputs/`). The figure taken is Athena++'s own "zone-cycles/cpu_second".

**Machine and tools.**

- Intel Core i9-13900KS (Raptor Lake: 8 P-cores up to 6.0 GHz, 16 E-cores, 36 MB L3), Linux
  7.0.0, governor `powersave` with EPP `balance_performance`, turbo on.
- Every timed run pinned with `taskset -c 8` (a P-core).
- rustc 1.100.0; g++ 15.2.0 (Ubuntu); GNU ld 2.46.

**Measurement.** Five repetitions of the whole matrix. Within each repetition every configuration,
layout and Athena++ build runs once, interleaved, so that background load spreads over all of
them. Runs last 2 to 10 s:

- nami: 100 cycles at 64³ and 20 at 128³ (A: 10 and 2, because it is slow);
- Athena++: 100 and 20.

nami's figure is wall-clock time over the time loop, which on a pinned, single-threaded process
matches Athena++'s CPU time to within noise.

**The machine was not idle.** Other agent sessions ran cargo builds and test suites in bursts
during the campaign. The 1-minute load average at the start of each run had a median of 5.8 and a
maximum of 20.6 (32 hardware threads), and the P-core clock dropped from 6.0 to about 5.1 GHz
under such bursts. The medians are stable; the minima in the table are the runs that a burst hit.
The ratios to Athena++ are therefore also computed within each repetition, against the Athena++
run of the same repetition and size, which ran minutes apart under similar load.

## Correctness

**Across layouts and configurations.** After one period, all four layouts produce bitwise-
identical states (FNV-1a hash of the bits):

- 64³: hash `cd8c234f981d50dc`, in configurations A, B, C, D and G alike;
- 128³: hash `b4ff5fa133dad012` (configuration C).

**Against Athena++** (L1 error after one period, Athena++'s norm: volume-weighted mean per
conserved variable, and Athena++'s "RMS-L1-Error", which is √Σ L1² over the variables, not a
mean):

| Size | Code | Cycles | RMS L1 | d | M1 (= M2 = M3) | E |
|---|---|---|---|---|---|---|
| 64³ | Athena++ (both builds) | 124 | 2.297543e-08 | 1.112785e-08 | 6.465961e-09 | 1.669178e-08 |
| 64³ | prototype, every layout | 124 | 2.297543e-08 | 1.112785e-08 | 6.465961e-09 | 1.669178e-08 |
| 128³ | Athena++ (both builds) | 247 | 6.057865e-09 | 2.936264e-09 | 1.700723e-09 | 4.404396e-09 |
| 128³ | prototype, every layout | 247 | 6.057865e-09 | 2.936264e-09 | 1.700723e-09 | 4.404396e-09 |

The errors agree to all printed digits; the observed order of convergence is 1.92. The
`algebraic_*` build (CA) also gives RMS L1 = 2.297543e-08 at 64³, with different bits.

## Numbers

Zone-cycles per second, in millions: median of 5 runs, with min and max. Ratio: the median of
the per-repetition ratios to Athena++'s SIMD build (for Athena++'s g++ build, the ratio of medians).

### 64³

| Code | Config | Variant | Median | Min | Max | Ratio to Athena++ |
|---|---|---|---|---|---|---|
| Athena++ | g++ | | 5.39 | 3.81 | 5.43 | 0.73× |
| Athena++ | g++-simd | | **7.34** | 5.87 | 7.40 | 1.00× |
| prototype | A (faithful) | soa | 0.72 | 0.70 | 0.72 | 0.10× |
| prototype | A | aos | 0.50 | 0.49 | 0.50 | 0.07× |
| prototype | A | hybrid4 | 6.18 | 5.81 | 6.20 | 0.84× |
| prototype | A | hybrid8 | 6.04 | 5.57 | 6.18 | 0.83× |
| prototype | B | soa | 6.47 | 6.02 | 6.66 | 0.90× |
| prototype | B | aos | 5.97 | 5.49 | 6.40 | 0.87× |
| prototype | B | hybrid4 | 6.20 | 5.96 | 6.24 | 0.84× |
| prototype | B | hybrid8 | 6.09 | 5.74 | 6.13 | 0.83× |
| prototype | C | **soa** | **14.08** | 13.21 | 14.38 | **1.95×** |
| prototype | C | aos | 8.41 | 7.53 | 8.67 | 1.14× |
| prototype | C | hybrid4 | 8.59 | 7.80 | 8.63 | 1.17× |
| prototype | C | hybrid8 | 8.41 | 7.12 | 8.51 | 1.15× |
| prototype | CA (`algebraic_*`) | soa | 16.24 | 12.70 | 16.41 | 2.20× |

### 128³

| Code | Config | Variant | Median | Min | Max | Ratio to Athena++ |
|---|---|---|---|---|---|---|
| Athena++ | g++ | | 4.84 | 4.28 | 5.03 | 0.75× |
| Athena++ | g++-simd | | **6.47** | 5.79 | 6.76 | 1.00× |
| prototype | A (faithful) | soa | 0.73 | 0.64 | 0.73 | 0.11× |
| prototype | A | aos | 0.49 | 0.44 | 0.50 | 0.08× |
| prototype | A | hybrid4 | 5.78 | 4.77 | 5.80 | 0.86× |
| prototype | A | hybrid8 | 5.54 | 5.06 | 5.77 | 0.87× |
| prototype | B | soa | 6.21 | 5.52 | 6.38 | 0.95× |
| prototype | B | aos | 5.79 | 3.90 | 5.93 | 0.88× |
| prototype | B | hybrid4 | 5.58 | 4.58 | 5.72 | 0.85× |
| prototype | B | hybrid8 | 5.10 | 4.54 | 5.73 | 0.85× |
| prototype | C | **soa** | **12.32** | 8.74 | 12.61 | **1.86×** |
| prototype | C | aos | 6.23 | 6.17 | 7.76 | 1.08× |
| prototype | C | hybrid4 | 7.46 | 6.04 | 7.72 | 1.14× |
| prototype | C | hybrid8 | 7.54 | 5.72 | 7.74 | 1.11× |
| prototype | CA (`algebraic_*`) | soa | 14.17 | 7.95 | 14.30 | 2.11× |

Single runs outside the campaign, same machine, 64³:

- configuration D (`libm::sqrt` without the per-call asserts), one period: SoA 0.86, AoS 0.63,
  hybrid 6.1 to 6.3 Mzc/s;
- configuration G (libm's generic software sqrt), 100 cycles: SoA 3.2 and AoS 3.2 Mzc/s.

Raw data: `research/layout-prototype/results/raw.tsv` on the prototype branch.

### Compile-time against runtime block size (SoA only)

`soa-const` is the SoA code with the block dimensions as a const generic (`Soa<Fixed<64>>`,
`Soa<Fixed<128>>`), so every row length and trip count is a compile-time constant after
inlining. `soa` is the same code with the sizes given at runtime. A separate campaign (commit
`f25084c`, 5 interleaved repetitions, Athena++ in every repetition, load average median 2.6,
max 8.5) compared the two. Bits are identical (`cd8c234f981d50dc` at 64³ over one period).

| Size | Config | soa (runtime) | soa-const | const / runtime, per repetition | soa-const ratio to Athena++ |
|---|---|---|---|---|---|
| 64³ | A | 6.65 (6.57–6.67) | 6.68 (6.56–6.75) | 1.003 (0.999–1.013) | 0.93× |
| 64³ | B | 6.70 (6.16–6.75) | 6.74 (6.03–6.75) | 1.000 (0.979–1.008) | 0.93× |
| 64³ | C | 13.71 (13.28–13.92) | 13.59 (13.19–13.91) | 0.993 (0.978–1.033) | 1.92× |
| 128³ | A | 6.03 (5.28–6.40) | 6.03 (5.84–6.42) | 1.007 (0.952–1.106) | 0.91× |
| 128³ | B | 6.39 (5.46–6.40) | 6.34 (5.81–6.38) | 0.999 (0.966–1.064) | 0.94× |
| 128³ | C | 11.79 (11.55–12.16) | 11.78 (11.54–12.29) | 0.999 (0.996–1.011) | 1.76× |

(Mzc/s, median with min–max. Athena++'s SIMD build ran at 7.22 at 64³ and 6.72 at 128³ in this
campaign.)

- **Same code.** The sweep functions of `Soa<Shape>`, `Soa<Fixed<64>>` and `Soa<Fixed<128>>`
  have the same instruction mix and sizes to within two instructions (for example 2346 against
  2346 instructions and 4 `vsqrtpd ymm` in each transverse sweep under C). LLVM keeps the same
  4-wide vector loop with a scalar remainder whether or not it knows the trip count. The loop
  bodies (one HLLC per face) are far too large to unroll fully, and the row lengths (n + 1,
  n + 2 faces) are not multiples of 4 anyway.
- **Same speed.** The per-repetition ratio is 1.00 within ±1% at the medians. The spread
  (0.95 to 1.11) is background noise.
- **So const generics are not a performance argument.** Whether block sizes are fixed at compile
  time can be decided on other grounds (AMR blocks of one size, the cost of monomorphising every
  size, binary size).
- **Side finding.** This build also marks the row accessors `#[inline(always)]` (they were
  `#[inline]`). That alone moves configuration A's SoA from 0.72 to 6.65 Mzc/s: the faithful libm
  build is fast here, at the speed of B. The arithmetic and the instruction counts of the hot
  loops are unchanged. Only the placement of calls, and with them `vzeroupper`, relative to libm's
  legacy `sqrtsd` changes. Rebuilding commit `2f971e5` reproduces 0.71 Mzc/s, and the new build
  gives 6.49 in the same minute. This confirms finding 2 below: with `libm::sqrt` under
  `x86-64-v3`, a 9× swing in speed follows from inlining decisions that look unrelated.

## Vectorisation

From `objdump -d` of the benchmark binaries (`research/layout-prototype/tools/vecstats.py` on the
prototype branch, which counts instructions per
function). Each sweep function contains the reconstruction, Riemann and flux-difference loops of
one direction, inlined. "Packed" means `ymm` (4 × f64) AVX2 arithmetic.

| Loop | A (faithful) | B | C |
|---|---|---|---|
| PLM, all layouts and directions except hybrid transverse | packed (`vdivpd ymm`, `vblendvpd ymm`) | packed | packed |
| PLM, hybrid transverse (W-lane groups) | scalar | scalar | scalar |
| HLLC, SoA x and transverse | scalar, 4 legacy `sqrtsd` | scalar, 4 `vsqrtsd` | **packed**: 4 `vsqrtpd ymm`, 15 `vdivpd ymm` per face loop |
| HLLC, AoS x-sweep | scalar | scalar | packed (with shuffles) |
| HLLC, AoS transverse | scalar | scalar | scalar |
| HLLC, hybrid transverse | scalar | scalar | scalar (LLVM fully unrolls the W-lane loop and does not SLP-vectorise it) |
| HLLC, hybrid x-sweep (unpacked rows) | scalar | scalar | packed |
| cons → prim, Heun update, flux difference | packed in SoA; partly in the others | packed | packed |

Findings:

1. **The per-call `assert!` in `math::sqrt` stops the HLLC loop from vectorising** in every
   layout (A and B). The loop gets a panic exit, and LLVM's loop vectoriser does not vectorise
   loops with early exits. Moving these to `debug_assert!` (C) vectorises it, and the per-block
   backstop still catches any NaN or negative pressure that reaches the state. ADR-0004 already
   puts per-cell checks in hot loops under `debug_assert!`; `kernels::math::sqrt` is the exception
   today, because it is written for general use.
2. **`libm::sqrt` with `features = ["arch"]` is an inline-asm, non-VEX `sqrtsd`.** LLVM cannot
   vectorise across it. Worse, in a function that also executes AVX code with dirty upper halves
   of the `ymm` registers (here the vectorised PLM loop), each legacy-SSE instruction pays an
   SSE/AVX transition penalty. SoA and AoS under A run at 0.5 to 0.7 Mzc/s, about 10× slower than
   B with identical arithmetic. The hybrid escapes by accident: its transverse sweeps contain no
   AVX code. Adding any opaque call to the loop (for example the phase timers) also hides the
   penalty, because LLVM emits `vzeroupper` before calls. This makes performance depend on
   inlining decisions. A fast faithful build of the hybrid (6 Mzc/s) is no argument for the
   hybrid. The const-generic campaign shows the same swing for SoA: marking the row accessors
   `#[inline(always)]` takes configuration A from 0.72 to 6.65 Mzc/s.
3. **No stable `no_std` route gives a vectorisable sqrt today:**
   - `f64::sqrt` needs `std` (and clippy bans it);
   - `core::f64::math::sqrt` is unstable (`core_float_math`) on 1.100;
   - `core::arch` intrinsics need `unsafe`;
   - libm without `arch` is a correct but slow software routine (G: 3.2 Mzc/s).
4. **The hybrid does not vectorise its transverse sweeps** in any configuration. With the W-lane
   loops behind a call boundary (feature `outlined-lanes`), the loop vectoriser does vectorise
   them, but the per-call overhead on 4 or 8 lanes cancels the gain (7.7 against 7.6 Mzc/s at
   64³). A hybrid that wins would need explicit SIMD types (`std::simd`, unstable), which buys
   nothing over SoA rows that LLVM already vectorises.

## Checks that stay on in release

Configuration A/B's finding was that `math::sqrt`'s per-call `assert!` gives the HLLC loop an
early exit, so it does not vectorise. Configuration C avoids that with `debug_assert!`, which
turns the checks off in release. Two configurations test whether the checks can stay on in
release (TigerStyle) and still vectorise:

- **E** (`intrinsic-sqrt,deferred-checks`): `f64::sqrt` as in C, but every square root of the
  per-face physics keeps the three checks of A/B: argument finite, argument ≥ 0, result ≥ 0.
  Each check is accumulated without a branch (`ok &= value.is_finite() & (value >= 0.0)`,
  `ok &= root >= 0.0`). The flag is asserted once after each row loop, with the sweep and the row
  in the message: "Y-sweep, row (3, 7): a face failed its sqrt checks". The sound speed in the
  conversion and dt loop feeds the existing per-block backstop flag the same way.
- **E2** (`intrinsic-sqrt,deferred-result`, SoA): as E, but a failed row returns
  `Err(RowFailure { sweep, row })`. The sweeps propagate it with `?`, and the stage asserts at
  its end.

E and E2 give the same bits as every other configuration (`cd8c234f981d50dc` at 64³ over one
period, all layouts). A release-build test feeds one face of a 9-face row a negative pressure
and checks that the row fails: E asserts, E2 returns the error.

Campaign: 5 interleaved repetitions with Athena++ in each, pinned to one P-core. The machine was
quiet (load average median 1.0, max 2.1). Figures are Mzc/s, median with min–max.

| Size | Variant | C (no checks in release) | E (deferred, assert per row) | E2 (deferred, `Result` per row) | E / C, per repetition | E vs Athena++ |
|---|---|---|---|---|---|---|
| 64³ | soa | 13.91 (12.70–14.07) | 13.19 (12.84–13.31) | 13.03 (12.74–13.42) | 0.950 | 1.81× |
| 128³ | soa | 12.61 (12.28–12.66) | 12.12 (12.01–12.18) | 11.87 (10.13–12.09) | 0.964 | 1.79× |
| 64³ | aos | 8.51 (7.28–8.59) | 7.21 (6.85–7.23) | 7.23 (6.84–7.26) | 0.846 | 0.98× |
| 128³ | aos | 7.68 (7.39–7.72) | 6.72 (6.65–6.72) | 6.70 (6.61–6.74) | 0.872 | 0.99× |

(Athena++'s SIMD build: 7.34 at 64³ and 6.78 at 128³ in this campaign. E2/C per repetition:
0.947 and 0.951 for SoA.)

Vectorisation, from `vecstats.py` on the E and E2 binaries:

- The HLLC loop stays packed wherever it was packed in C. SoA's x and transverse sweeps and AoS's
  x-sweep each keep their 4 `vsqrtpd ymm`.
- The check costs about 190 extra instructions per sweep function: packed compares and `and`s
  on the flag.
- AoS's transverse sweep stays scalar, as in C. There the extra scalar compares cost more, which
  is why AoS loses 13 to 15% and SoA 4 to 5%.

**Conclusion.** Per-face checks can stay on in release by default without giving up
vectorisation, provided they are deferred: accumulated without branching inside the row loop,
and asserted (or returned as an error) once per row. For SoA the price is 3.6 to 5% of the step,
and nami still runs at 1.8× Athena++'s fast-math build. Returning a `Result` per row instead of
asserting changes nothing that matters (E2 is within about 2% of E), so whether a failed row
is a `Result` or an assertion can be decided on design grounds. The price is in the diagnostics: the
assertion names the row, not the face. Finding the face needs a cold path that rescans the row,
which this prototype does not have. The pattern applies to any per-face check written as a
comparison. An `assert!` inside the per-face loop (A, B) does not vectorise and stays at 0.9×
Athena++ or below. An opt-out feature for these checks would buy about 4% (configuration C).

## Where the time goes

Configuration C with per-phase timers (`Instant::now` around every row-level phase), pinned, on
a quiet machine. Shares of the wall time; the rest (3 to 11%) is loop and timer overhead. The
timers slow the run by 4 to 12% (columns "timed" and "untimed").

| Phase | soa 64³ | aos 64³ | hybrid8 64³ | soa 128³ | aos 128³ | hybrid8 128³ |
|---|---|---|---|---|---|---|
| Riemann solver (HLLC) | 49% | 57% | 53% | 45% | 54% | 51% |
| Reconstruction (PLM) | 16% | 20% | 18% | 18% | 18% | 18% |
| Flux difference | 8% | 7% | 7% | 13% | 12% | 8% |
| RK update | 9% | 5% | 6% | 9% | 6% | 6% |
| Cons → prim, dt, backstop | 6% | 6% | 6% | 6% | 6% | 7% |
| Ghost fill | 2% | 1% | 1% | 2% | 1% | 1% |
| Hybrid repacking (x-sweep) | | | 4% | | | 7% |
| Mzc/s timed / untimed | 12.0 / 13.6 | 7.4 / 8.2 | 7.3 / 8.0 | 11.3 / 12.0 | 7.2 / 7.6 | 7.1 / 7.4 |

HLLC dominates. SoA wins mainly because its Riemann loop is the one that vectorises in all three
directions. At 128³ the memory-bound phases (flux difference, update) grow from 17% to 22% of
SoA's time. The rolling row buffers keep the stencil working set in cache, so the 128³ slowdown
is small (12.3 against 14.1 Mzc/s).

## Recommendation

1. **Adopt the hypothesis of § 4(a):** structure of arrays `[var][k][j][i]`, kernels on row
   stencils, per-face physics over named-field state structs called in a loop over i. It is
   fastest at both sizes in every configuration except A and D, the two with libm's `arch` sqrt.
   There the hybrid's 6 Mzc/s against SoA's 0.7 to 0.9 comes from escaping the SSE/AVX
   transition penalty by accident (finding 2), not from its layout. In the `f25084c` build,
   which differs only in inlining, SoA under A reaches 6.65 Mzc/s ("Side finding"); D was not
   measured in that build. It is the only layout whose hot loops all
   vectorise without help, and it needs no transposition or repacking. Raw indexing stays
   banned: the whole prototype is written with `chunks_exact`, `array_windows`, zipped rows and
   typed row accessors, and loses nothing for it. Do not pursue AoS or AoSoA.
2. **1.0× Athena++ per core looks reachable,** at about 1.9× for this problem, but only once
   `sqrt` is fixed. The ADR that amends ADR-0003 should decide, together with ADR-0004 and
   ADR-0005:
   - **the per-call domain asserts in `kernels::math::sqrt`:** not an `assert!` per call in
     per-face loops. Either deferred checks that stay on in release (configuration E: one
     branch-free flag per row, asserted once per row, 4 to 5% for SoA), or `debug_assert!`
     with the per-block backstop as the release-build guarantee (C, ADR-0004's current rule for
     per-cell checks). E fits a "checks on by default, a feature to opt out" policy, and the
     opt-out would buy about 4%. Either way, hot loops need an entry point other than today's
     `math::sqrt`;
   - **a vectorisable, correctly rounded sqrt in `no_std` `kernels`.** Every option except the
     libm patch changes ADR-0005 rule 1 ("`sqrt` comes from `libm::sqrt`"), and with it the
     `f64::sqrt` ban in `clippy.toml`. Each also touches the rules named against it:
     - `core::f64::math::sqrt` once stable: ADR-0005 rule 1 and the clippy ban (the ban names
       `f64::sqrt`, so it needs a row for the `core` path too);
     - `std` for `kernels`, which gives `f64::sqrt`: ADR-0005 rule 1, the clippy ban, and
       ADR-0003's "compiler rejects allocation", which it weakens;
     - a small audited `unsafe` wrapper around `_mm256_sqrt_pd`/`vsqrtsd`: ADR-0005 rule 1,
       ADR-0014 (`unsafe` only in `alloc-guard`), and `ARCHITECTURE.md` § 5.1 ("never rewritten
       with intrinsics"). It is also x86-only, so other targets need a second backend;
     - a libm patch that uses VEX encoding when AVX is enabled: keeps rule 1, but adds a patched
       dependency (ADR-0001). It removes the transition penalty but still does not vectorise.

     Until then, `libm::sqrt` with `arch` under `x86-64-v3` is a performance hazard that depends
     on inlining: 0.7 or 6.6 Mzc/s for the same SoA arithmetic. Without the per-call asserts,
     libm's `arch` backend (D) ran SoA and AoS at 0.86 and 0.63 Mzc/s in the `2f971e5` build,
     whose inlining is the slow one; D was not measured in the `f25084c` build, where A's SoA
     already reaches 6.65 Mzc/s with the asserts kept. libm's generic backend (G) ran at
     3.2 Mzc/s. So the data shows that libm is fragile here, not that it is capped below 1 Mzc/s.
3. **Determinism costs about 13%** of the `algebraic_*` build's speed on this problem
   (`algebraic_*` in PLM and HLLC: 16.2 against 14.1 Mzc/s at 64³, 14.2 against 12.3 at 128³).
   That is inside ADR-0005's estimate, and nami still beats Athena++'s fast-math build by 1.9×
   while staying bitwise deterministic. The ban on reordering (`algebraic_*`, implicit
   `mul_add`) needs no revisiting; rule 1's choice of `sqrt` backend does (recommendation 2).

4. **Block dimensions at compile time are optional.** Const generics change neither the code
   nor the speed (within ±1%), so § 4(c) can choose fixed or runtime block sizes for other
   reasons.

## What I could not measure

- **Hardware counters and a profile.** `perf` is not available to the user on this machine
  (`perf_event_paranoid = 4`). The SSE/AVX transition explanation rests on the instruction mix in
  the binaries and on the A/B/D experiments, not on counter readings. The phase split comes from
  wall-clock timers, which perturb the run by 4 to 12%.
- **An idle machine.** See Measurement. The medians are consistent across repetitions, but the
  absolute numbers are from a shared workstation with turbo, not a quiet benchmarking node.
- **Athena++'s own phase split,** for the same reason (no `perf`). Athena++'s per-cycle overhead
  (task list, boundary buffers, history) is not separated from its kernels.
- **AthenaK,** and Athena++ built with clang or icpx. Only g++ 15.2 was tried.
- **Other CPUs.** One Raptor Lake P-core only. AVX-512 machines, Apple Silicon (where the libm
  `arch` path differs) and AMD were not measured.
- **Multi-core and multi-node scaling,** and blocks smaller or larger than one block per run.
  The 128³ number is for a single 128³ block, not for a mesh of smaller blocks.
- **MHD** (HLLD, constrained transport), whose transverse terms are what ADR-0003 calls clumsy as
  typed gathers. This prototype has none.
- **A hybrid with explicit SIMD types** (needs the unstable `std::simd`).
