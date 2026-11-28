# ADR-0001: Dependency policy

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer
- Summary: The only runtime dependency is `libm`. Dev and tooling crates are allow-listed. Anything new needs an ADR.

## Context

TigerStyle asks for zero dependencies. nami's numerical kernels live in a `#![no_std]` crate without `alloc` (ADR-0003), so the compiler itself rules out heap allocation there. On stable Rust 1.100, `core` has no `sqrt`, `mul_add`, `floor`, `exp`, `sin` or `powf`. `core_float_math` is unstable and not on track to stabilise ([rust-lang/rust#137578](https://github.com/rust-lang/rust/issues/137578)). A literal zero-dependency policy would therefore force the kernels into a std crate and lose the compile-time no-allocation guarantee.

Agents also pick dependencies freely. In Cursor's FastRender, agents chose almost all of their own dependencies and even vendored and modified one of them.

## Decision

**Runtime (normal) dependencies of every nami crate: only `libm`.**
- `libm` is owned by rust-lang (it now lives in `rust-lang/compiler-builtins`), has no dependencies of its own and needs no allocator. Its version is pinned exactly.
- It is also the single source of transcendental functions on all platforms (ADR-0005).

**Dev-dependencies (tests, benches): allow-list.**
- `proptest`, `insta`, and later `bolero`, plus their transitive dependencies.

**Tooling crates (only in `xtask/` and the dylint library, never in nami crates).**
- `syn`, `quote`, `proc-macro2`, plus whatever the dylint library needs (ADR-0006).

**Binaries, installed at pinned versions and never in `Cargo.lock`.**
- `cargo-nextest`, `cargo-deny`, `cargo-mutants`, `cargo-dylint`/`dylint-link`, `just`, and the Gungraun runner.

**Python tooling for reference generation only (ADR-0007).**
- SymPy and Clawpack/PyClaw in a `uv`-locked environment under `tools/`. Its output is committed, so building or testing nami never needs Python.

**Adding anything to any of these lists requires a new ADR approved by a human.**

**Deferred until a later ADR:**
- Parallelism (scoped `std::thread` vs. rayon).
- Output formats (hand-written VTK or raw binary vs. HDF5).
- Both start dependency-free.

## Enforcement

- `deny.toml`:
  - `[bans]` with an explicit allow-list covering the whole dev graph and `multiple-versions = "deny"`.
  - `[sources]` allowing only crates.io: unknown registries and git sources are denied.
  - `[licenses]` and `[advisories]` enabled.
- Tidy (ADR-0006) checks that the normal-dependency closure of every nami crate (`cargo metadata`, normal edges only) is exactly `{libm}`.
- All cargo invocations in CI and `just` use `--locked`.
- CODEOWNERS on `Cargo.toml` (every crate), `Cargo.lock`, `deny.toml`, `tools/` and `xtask/` (ADR-0008).

## Consequences

- Kernels stay `no_std` without `alloc` on stable Rust.
- Transcendental results are the same code on every platform.
- Agents will sometimes hit the allow-list and stall. The error message must tell them to propose an ADR instead of looking for a workaround.
- `libm` is 1.7–3.7× slower than the macOS system libm for `exp`, `sin`, `cbrt` and `powf`, and as fast for `sqrt` (ADR-0005 evidence). This matters only for equations of state or source terms that use transcendentals in hot loops.

## Alternatives considered

- **Literally zero dependencies.** Rejected. It loses the `no_std` kernel crate, makes results depend on the platform libm, and means writing our own property, snapshot and fuzz tooling.
- **The normal Rust ecosystem** (ndarray, num-traits, rayon, serde, …). Rejected. It conflicts with TigerStyle, those crates allocate internally, and it repeats the FastRender dependency sprawl.
- **`num-traits` with its `libm` feature as a generic float wrapper.** Not needed. nami calls `libm` directly.

## Evidence

- Local check on rustc 1.100.0: in a `#![no_std]` crate, `abs`, `max`, `copysign`, `clamp`, `signum` and `recip` compile; `sqrt`, `mul_add`, `powi`, `exp`, `sin`, `floor`, `cbrt` and `powf` do not.
- libm 0.2.16 (2026-01-24), now in [rust-lang/compiler-builtins](https://github.com/rust-lang/compiler-builtins).
- FastRender dependency sprawl: [Simon Willison, 2026-01-23](https://simonwillison.net/2026/jan/23/fastrender).
