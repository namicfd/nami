# ADR-0014: Unsafe code confined to the allocation guard

- Status: Accepted
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0004 (enforcement of "no unchecked hints")

## Context

ADR-0003 requires a counting global allocator in tests to prove that nothing allocates after init. Implementing a global allocator means writing `unsafe impl GlobalAlloc`.

ADR-0004 relied on `unsafe_code = "forbid"` to exclude `assert_unchecked`, `unreachable_unchecked` and `get_unchecked`. A `forbid` cannot be overridden locally, not even with `#[expect]`, so the allocator cannot be written under it. The published allocation-guard crates are unmaintained (`assert_no_alloc`, last released in 2021), and ADR-0001 rules out new dependencies anyway.

## Decision

1. The workspace lint changes from `unsafe_code = "forbid"` to **`unsafe_code = "deny"`**, and `undocumented_unsafe_blocks = "deny"` is added, so every `unsafe` block or impl needs a `// SAFETY:` comment explaining why it is sound.
2. `unsafe` is allowed in **exactly one crate**, `crates/alloc-guard`. It is a test-support crate that holds the counting allocator and nothing else. Its single `unsafe impl GlobalAlloc` carries one `#[expect(unsafe_code, reason = "...")]`.
3. That `#[expect]` is the only one in the repository. The `#[expect]` budget (ADR-0006) is set to 1.
4. `alloc-guard` is only ever a dev-dependency. No runtime code links it.

## Enforcement

- **Clippy and rustc:** `unsafe_code = "deny"` and `undocumented_unsafe_blocks = "deny"` in `[workspace.lints]`.
- **Tidy rule `unsafe`:** rejects `unsafe` blocks, `unsafe fn`, `unsafe impl` and `unsafe trait` outside `crates/alloc-guard`, with a fixture test.
- **Tidy `#[expect]` budget:** fixed at 1 in `xtask/expect-budget.txt`. Only a human can raise it (code-owned).
- **Tidy runtime-dependency rule (ADR-0001):** keeps the crate out of `[dependencies]`.

## Consequences

- ADR-0003's no-allocation proof can be implemented in about 60 lines with no new dependency.
- The unchecked hints stay excluded. They need `unsafe`, which tidy now rejects outside the guard crate, so the guarantee moves from the compiler to tidy plus review of a single code-owned file.
- Counts are kept per thread, so tests that run in parallel threads do not disturb each other.

## Alternatives considered

- **Keep `forbid` and give the guard crate its own lint table.** Rejected. Cargo cannot override workspace lints per crate, so the table would have to be copied, and copies drift.
- **Depend on `assert_no_alloc` or a similar crate.** Rejected. It is unmaintained, and ADR-0001 rules it out.
- **Drop the allocation test and rely on `no_std` alone.** Rejected. `solver` is a std crate, and its time loop is exactly where an accidental allocation would happen.

## Evidence

- `#[expect]` cannot override `forbid`: this is rustc's documented semantics for `forbid`.
- Local check: `crates/alloc-guard/tests/counting.rs` shows that one `Vec::with_capacity` is counted exactly once and that an allocation inside `assert_no_allocations` fails the test.
