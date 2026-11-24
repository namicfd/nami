# ADR-0002: Integer width and typed indices

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer

## Context

TigerStyle says to use explicitly sized types like `u32` and to avoid the architecture-dependent `usize`. Rust slices index only with `usize`. Finite-volume code juggles several index spaces: cells with and without ghost layers, faces, nodes and variables. Mixing them up is a classic bug that compiles without complaint.

## Decision

1. **Index and count types are newtypes over `u32`** (or a smaller type where the range is small). Examples are `CellIndex(u32)`, `FaceIndex(u32)`, `NodeIndex(u32)`, `CellCount(u32)` and `VarIndex(u8)`. Each index space gets its own type, and they are not interchangeable.
2. **`usize` appears only inside one module, `base::index`.** That module implements `Index`/`IndexMut` for nami's field and slice wrappers and owns every `u32` ↔ `usize` conversion:
   - `CellIndex::to_usize(self) -> usize` is a lossless widening.
   - `CellIndex::from_usize(n: usize) -> CellIndex` asserts that `n <= u32::MAX`.
   - A compile-time check `const _: () = assert!(usize::BITS >= 32);` guarantees the widening is lossless.
3. **Loops over index ranges use typed iterators** such as `CellIndex::range(count)`, never `enumerate()` with a cast.
4. **The limit is 2³² − 1 cells per rank.** Larger problems are decomposed.
5. Other integers (step counters, seeds, sizes in file formats) use explicit widths: `u32` by default, `u64` where 2³² is not enough.

## Enforcement

- Clippy: `as_conversions`, `cast_possible_truncation`, `cast_sign_loss` and `cast_possible_wrap` are set to `deny`. Each `disallowed-*` reason string names the fix (for example "use `CellIndex::to_usize()`").
- Tidy (ADR-0006): rejects `usize` in `pub` function signatures, struct fields and consts outside `base::index`.
- Dylint (ADR-0006): later an alias-aware version of the same rule, so a `type Idx = usize;` alias cannot get around it.

## Consequences

- The type checker catches cell/face/ghost index mix-ups, and connectivity arrays take half the memory of `usize` ones.
- Indexing costs one zero-extension instruction, which measured as free (evidence below).
- Agents will write `as usize` by habit. The deny lint, with its message naming the fix, turns that into a one-step correction.
- Under ADR-0003 kernels rarely use raw indices, so the friction is mostly in mesh and connectivity code, where the type safety pays off most.

## Alternatives considered

- **`usize` everywhere with overflow asserts.** Rejected. Index spaces would be indistinguishable, and it goes against TigerStyle.
- **Hybrid: `u32` for stored data and `usize` for local loop variables.** Rejected. The boundary is fuzzy and hard to enforce mechanically.
- **The `typed-index-collections` or `oxc_index` crates.** Not needed. They would be an extra dependency (ADR-0001) for about 50 lines of our own code.

## Evidence

- Local benchmark on Apple Silicon, rustc 1.100, 1M-cell 3-point stencil: `usize` 0.16 ms vs. `u32` with conversion 0.15 ms.
- Codegen check (Compiler Explorer, rustc 1.100): the `u32` → `usize` conversion is one `mov`, and the loop is still vectorised.
- [TIGER_STYLE.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md): "Use explicitly-sized types like `u32` for everything, avoid architecture-specific `usize`."
- TigerBeetle's Rust client converts at the boundary with `try_into().expect(..)`.
