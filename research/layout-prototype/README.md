# Layout prototype (throwaway)

Measured prototype for `docs/ARCHITECTURE.md` § 4(a): which data layout nami's kernels should
use, measured on 3D Euler. Results and recommendation: `docs/research/layout-prototype.md` (on
`main`, once its PR lands). This branch is never merged.

## What it is

- 3D Euler, ideal gas (γ = 5/3), one periodic block with 2 ghost cells per side, single core.
- PLM on primitives with the van Leer limiter, HLLC, SSP-RK2 (Heun), CFL 0.3, dt from the
  largest signal speed. PLM and HLLC are ported operation by operation from Athena++ v24.0
  (`src/reconstruct/plm.cpp`, `src/hydro/rsolvers/hydro/hllc.cpp`).
- Problem: Athena++'s `linear_wave` (adiabatic hydro, `wave_flag = 0`, amplitude 1e-6) on the unit
  cube, with the wave vector along the grid diagonal.
- Four layouts, all calling the same per-face functions in `src/physics.rs` with the same loop
  structure, so the arithmetic is the same operation for operation:
  - `soa`: `[var][k][j][i]`, row stencils (the hypothesis), sizes given at runtime;
  - `soa-const`: the same code with the block size as a const generic (`Soa<Fixed<64>>`,
    `Soa<Fixed<128>>`), to measure what compile-time trip counts buy;
  - `aos`: `[k][j][i][var]`, one `[f64; 5]` per cell;
  - `hybrid4`, `hybrid8`: `[k][j][i/W][var][i%W]` (AoSoA), groups of 4 or 8 cells along i.
- Constraints kept from the real code: no raw slice indexing (`clippy::indexing_slicing` denied;
  rows come from `chunks_exact().nth()` and `get()`), `libm` through a copy of `kernels::math`,
  `assert!` on lengths before loops, `debug_assert!` per cell, a per-block backstop
  (density and pressure positive and finite) on every stage, the workspace release profile
  (`overflow-checks = true`) and `-C target-cpu=x86-64-v3`, and no allocation in the time loop
  (a counting global allocator checks it on every run).

## Feature configurations

| Name | Features | What it measures |
|---|---|---|
| A | none | The faithful build: `kernels::math::sqrt` as it is (`assert!` on every call, `libm::sqrt`). |
| B | `intrinsic-sqrt` | `f64::sqrt` (LLVM intrinsic, VEX `vsqrtsd`) instead of `libm::sqrt` (inline-asm `sqrtsd`). Same bits. |
| C | `intrinsic-sqrt,sqrt-debug-asserts` | B, with the sqrt domain asserts as `debug_assert!`. Same bits. |
| D | `sqrt-debug-asserts` | `libm::sqrt` without the per-call asserts. Same bits. |
| G | `sqrt-debug-asserts`, with libm's `arch` feature removed in `Cargo.toml` | libm's pure-Rust generic sqrt. Same bits. |
| E | `intrinsic-sqrt,deferred-checks` | B's sqrt checks kept on in release, but accumulated branch-free per face into a flag and asserted once per row. Same bits. |
| E2 | `intrinsic-sqrt,deferred-result` | E, but a failed row returns an `Err` that propagates to the end of the stage (SoA). Same bits. |
| CA | C + `algebraic` | Data point only: PLM and HLLC in `algebraic_*` arithmetic. Different bits. |
| T | C + `phase-timers` | Per-phase wall-clock breakdown. |
| CO | C + `outlined-lanes` | Hybrid only: lane loops behind a call so the loop vectoriser sees them. |

## Reproduce

```sh
cd research/layout-prototype
for cfg in "A:" "B:intrinsic-sqrt" "C:intrinsic-sqrt,sqrt-debug-asserts" \
           "D:sqrt-debug-asserts" "CA:intrinsic-sqrt,sqrt-debug-asserts,algebraic" \
           "T:intrinsic-sqrt,sqrt-debug-asserts,phase-timers"; do
    name=${cfg%%:*}; feats=${cfg#*:}
    cargo build --release --locked ${feats:+--features $feats}
    mkdir -p bin && cp target/release/layout-prototype bin/lp-$name
done

# Correctness: all layouts bitwise identical, and the L1 error after one period.
taskset -c 8 bin/lp-C compare 64 period

# One measurement: zone-cycles per second.
taskset -c 8 bin/lp-C run soa 128 20

# Where the time goes.
taskset -c 8 bin/lp-T run soa 128 20

# Vectorisation: packed AVX2 instructions per function in the real binary.
python3 -I tools/vecstats.py bin/lp-C
```

Athena++ baseline (v24.0, commit `209a9258ee158d1965557f1a5c4193101b704dd9`):

```sh
git clone https://github.com/PrincetonUniversity/athena ~/bench/athena
cd ~/bench/athena && git checkout v24.0 && cp -r . ../athena-simd
python3 configure.py --prob=linear_wave --flux=hllc --eos=adiabatic --cxx=g++ && make -j16
cd ../athena-simd
python3 configure.py --prob=linear_wave --flux=hllc --eos=adiabatic --cxx=g++-simd && make -j16
# Inputs: athena-inputs/lw64.in and lw128.in (integrator = rk2, xorder = 2, no outputs).
```

Full campaign (5 repetitions, everything interleaved, pinned to one P-core):

```sh
./bench.sh ~/bench/athena/bin/athena ~/bench/athena-simd/bin/athena athena-inputs 5 8
python3 -I tools/summarise.py results/raw.tsv
```

## Results in this directory

- `results/raw.tsv`: every timed run of the campaign (`bench.sh`), with the load average.
- `results/bitwise-64-period.txt`, `results/bitwise-128-period.txt`: one wave period in every
  layout and configuration; all hashes agree, and the L1 errors match Athena++'s.
- `results/phases.txt`: per-phase breakdown (config T) next to the untimed rate (config C).
- `results/vectorisation.txt`: packed AVX2 instruction counts per function (`tools/vecstats.py`).
- `results/algebraic-64-period.txt`: the `algebraic_*` data point over one period.
- `results/raw-deferred.tsv`: configurations C, E and E2 for `soa` and `aos`
  (`CONFIGS="C E E2" VARIANTS="soa aos" ALGEBRAIC=0 OUT=results/raw-deferred.tsv ./bench.sh ...`).
- `results/raw-const.tsv`: `soa` against `soa-const` in configurations A, B and C
  (`CONFIGS="A B C" VARIANTS="soa soa-const" ALGEBRAIC=0 OUT=results/raw-const.tsv ./bench.sh ...`).
  This build also marks the row accessors `#[inline(always)]`, which by itself moves configuration
  A's SoA from 0.7 to 6.6 Mzc/s (see the results document).
