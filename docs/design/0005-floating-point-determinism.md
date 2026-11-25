# ADR-0005: Floating-point determinism contract

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer

## Context

When regression tests compare results within a tolerance, agents have a knob they will loosen to make a test pass. When results are bitwise reproducible, any change in output is an unambiguous signal, and every failure can be reproduced exactly from a seed.

Checks on Rust 1.100:

- rustc **never** contracts `a*b+c` into an FMA instruction, not even with `-C target-cpu=native`. Basic operations follow IEEE 754 exactly ([RFC 3514](https://rust-lang.github.io/rfcs/3514-float-semantics.html)).
- `mul_add` is a correctly rounded IEEE `fusedMultiplyAdd`, so it gives identical bits on every platform. It is only slow on baseline x86_64, where it calls a software routine.
- `algebraic_add`/`sub`/`mul`/`div`/`rem` (stable since 1.98) explicitly permit reordering and contraction, so they are **not deterministic**.
- `f64::sin`/`exp`/`powf` etc. in std call the platform libm and differ between macOS and glibc.
- NaN payload bits are not deterministic.

## Decision

nami's output is **bitwise identical across platforms** (at minimum Linux x86_64 and macOS arm64) for a fixed toolchain and a fixed `Cargo.lock`.

1. **Transcendental functions** come only from the `libm` crate (ADR-0001). The std methods (`f64::sin`, `exp`, `ln`, `powf`, `powi`, `cbrt`, `hypot`, …) are banned. `sqrt` comes from `libm::sqrt`, which uses the hardware instruction and is correctly rounded.
2. **`algebraic_*` operations are banned.** No fast-math of any kind.
3. **`mul_add` is allowed only where written explicitly.** Clippy's `suboptimal_flops` stays off, so nobody is pushed to rewrite expressions. x86_64 CI and release builds use `-C target-cpu=x86-64-v3`, so `mul_add` runs on hardware FMA. The target CPU does not change results.
4. **Reductions run in a fixed order.**
   - Sums use `kernels::reduce::sum_fixed`: 8 accumulator lanes combined in a fixed tree, which is deterministic and as fast as a reordering sum.
   - Conservation totals use `sum_compensated` (Neumaier).
   - `max`/`min` give the same result in any order.
   - Future threaded or SIMD reductions must combine partial results in partition-index order.
5. **NaN is always an error.** Any NaN in a state, flux or time step fails an assertion, so NaN payload bits never matter.
6. **No iteration over hashed collections** (`HashMap`, `HashSet`).
7. **Tolerances are only for correctness references** (convergence order, exact solutions; ADR-0007). Regression and determinism tests compare bits.
8. **A change in output bits must be explained.** That includes changes caused by a toolchain or `libm` bump. Such PRs are labelled `changes-results` and need a human (ADR-0008).

**Fallback.** If the cross-platform hash comparison shows that `libm` itself differs between platforms, a superseding ADR falls back to "bitwise on the pinned Linux CI platform, √ε-relative error norms elsewhere".

**Future.** If profiling shows the determinism cost matters for production runs, a later ADR may add a `Real` number type that routes kernel arithmetic through `algebraic_*` behind a cargo feature. CI would keep testing the deterministic build.

## Enforcement

- Clippy `disallowed-methods` covers the std transcendentals (reason: "use `libm::…`, ADR-0005") and `algebraic_*`. `disallowed-types` covers `HashMap`/`HashSet` (`iter_over_hash_type = "deny"` as well).
- Dylint (ADR-0006) flags naive float reductions (`.sum::<f64>()`, or a `fold` with `+` over `f64`) with the message "use `sum_fixed`".
- CI determinism test:
  - Run a canonical problem set and hash the raw output bits.
  - Compare the hashes from the Linux x86_64 and macOS arm64 jobs.
  - Rerun the same seed and compare again.
  - The macOS comparison runs post-merge (ADR-0009). The same-platform rerun blocks merging.
- `.cargo/config.toml` sets `-C target-cpu=x86-64-v3` for x86_64.

## Consequences

- Golden values are identical on every machine. Agents have no tolerance to tune, and every failure is reproducible from a seed.
- Per-cell flux arithmetic is about 20–25% slower than the compiler-reordered version. Estimated cost per full step: 5–15% (not measured).
- Future threads, SIMD or GPU backends must preserve reduction order.
- `libm` is slower than the system libm for `exp`, `sin`, `cbrt` and `powf`. This is irrelevant for ideal-gas Euler, shallow water, Burgers and ideal MHD, whose hot loops only need `sqrt`.

## Alternatives considered

- **Bitwise only on one pinned CI platform.** Rejected for now; it remains the fallback. Developer machines would need tolerances.
- **Tolerance-only, `algebraic_*` allowed.** Rejected. Thresholds become flaky and agents loosen them.

## Evidence

Local benchmarks, rustc 1.100, Apple Silicon, release builds:

| Measurement | Deterministic | `algebraic_*` | Ratio |
|---|---|---|---|
| Sum of 4M f64, sequential fold | 2.26 ms | 0.45 ms | 5.1× |
| Sum of 4M f64, 8 fixed lanes | 0.41 ms | 0.45 ms | 0.9× |
| 1D Euler Rusanov update, 1M cells, naive | 3.81 ms | 2.66 ms | 1.43× |
| … with explicit `mul_add` | 3.50 ms | 2.66 ms | 1.32× |
| … hand-simplified (one reciprocal) | 3.27 ms | 2.66 ms | 1.23× |

`libm` vs. macOS system libm (1M calls):

| Function | Ratio |
|---|---|
| `sqrt` | 0.92× |
| `exp` | 1.82× |
| `sin` | 1.71× |
| `cbrt` | 2.37× |
| `powf` | 3.68× |

- Codegen: `a*b+c` → `fmul`+`fadd` (no FMA), even with `target-cpu=native`. `mul_add` → `fmadd`.
- Reproducibility literature: CESM-ECT and FELTOR (bitwise only with identical compiler, flags and FMA behaviour), FLiT (tolerance bands across platforms). See the research report's section on numerical oracles (called references in nami).
