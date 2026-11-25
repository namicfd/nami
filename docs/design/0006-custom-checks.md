# ADR-0006: Mechanical checks: clippy, tidy and dylint

- Status: Accepted (amended by ADR-0013)
- Date: 2026-11-24
- Deciders: maintainer

## Context

Rules written in AGENTS.md are requests. Lints are guarantees. Clippy (Rust 1.100) covers about half of TigerStyle, but has three verified gaps:

- **No recursion lint.** Only `unconditional_recursion` exists. A mutual-recursion lint was attempted in rustc and dropped over performance problems.
- **`too_many_lines` counts code lines only**, not physical lines.
- **Nothing bans heap allocation** beyond name-based `disallowed_*` lists.

A recursion check that matches calls by name would be noisy in nami, because many types implement `flux()`, `wave_speeds()` and so on, and a wrapper such as `fn flux(&self) { self.inner.flux() }` looks recursive by name.

Semgrep's free edition and ast-grep match syntax only: they have no type information and no call graph.

## Decision

Three tools. **Each rule lives in exactly one of them.**

| Tool | Toolchain | Runs in | Owns |
|---|---|---|---|
| **clippy** (workspace `[workspace.lints]` + `clippy.toml`) | stable 1.100.0 (pinned) | after-edit hook (per crate), `just check`, CI | type-aware bans with reason strings (std transcendentals, `algebraic_*`, `HashMap`, `Vec::push`, `vec!`), cast and overflow lints, `unwrap_used`, `indexing_slicing`, `float_cmp`, `allow_attributes(_without_reason)`, `missing_assert_message`, `too_many_lines` (backstop), `excessive_nesting` |
| **tidy** (`cargo xtask tidy`, built on `syn`, also registered as a `#[test]`) | stable | after-edit hook (single file), `just check`, CI | 70 physical lines per function (closures included); assertion density and placement (ADR-0004); bounded loops (`loop`/`while` only through `bounded_loop!(max_iterations, …)`); `usize` outside `base::index` (ADR-0002); no `dyn`/`fn` pointers in `kernels` (ADR-0003); no `TODO`/`FIXME`/`dbg!`; no `#[allow(`; `#[expect]` count ratchet; no `#[ignore]` without a reason; no `.snap.new` files; snapshot of the lint table (it must not be weakened); normal-dependency closure = `{libm}` (ADR-0001); ADR index consistency (ADR-0000); 100-column lines |
| **dylint** (one library in `lints/`, its own workspace and `rust-toolchain.toml` pinned to one nightly) | pinned nightly + `rustc-dev` | `just check-full`, the Stop/SubagentStop hook, CI | **no recursion** (cycle detection over each crate's resolved call graph; Cargo forbids crate cycles, and ADR-0003 bans `dyn`/fn pointers in kernels, so checking each crate is complete for kernels); naive float reductions → `sum_fixed` (ADR-0005); alias-aware `usize` rule |

### Rules for all three tools

- **Error format:** `file:line: error[<rule-id>]: <what>; use <replacement> (ADR-NNNN)`. The message tells the agent how to fix the problem.
- **Fixture crate** (`xtask/fixtures/`): one deliberate violation per rule. A test asserts that each rule still fires, so rules cannot go silent after a toolchain or library upgrade.
- **Suppressions:** only `#[expect(lint, reason = "...")]`. The total count is ratcheted in a committed file that only humans may raise.
- **Warnings policy:** `-D warnings` only in CI and `just`. Never write `#![deny(warnings)]` in source, because new toolchains add lints and would break builds.
- **Initial thresholds** (to be tuned in the implementing PR):
  - `too-many-lines-threshold = 60` (code lines, as a backstop to tidy's 70 physical lines);
  - `excessive-nesting-threshold = 4`.
- **Dylint upkeep:** bump the nightly every few months (`cargo dylint upgrade`). Agents may do it; the fixture tests prove the rules survived.

### Not used

- **Semgrep.** Its Rust support is syntax-only in the free edition, it brings a Python runtime, and tidy already covers the same ground.
- **ast-grep.** Fast, but it would be a fourth place where rules live. We may revisit this if tidy's after-edit latency becomes a problem.

## Enforcement

- `just check` runs fmt → tidy → clippy → tests. `just check-full` adds dylint and diff mutants.
- CI runs both.
- CODEOWNERS on `clippy.toml`, the lint tables, `xtask/`, `lints/` and the expect-budget file.

## Consequences

- Every TigerStyle non-negotiable has an exact, mechanical check.
- Builds and tests stay on stable. Only the dylint library needs a nightly, which is one more toolchain to maintain and one more slow step in CI.

### Known gaps

- `float_cmp` skips functions whose name contains `eq` (verified locally). Tidy flags `==`/`!=` between floats in such functions.
- Whether `allow_attributes_without_reason` also covers `#[expect]` is unconfirmed. Tidy checks that every `#[expect]` has a reason.

## Alternatives considered

- **Clippy plus syn tidy only.** Rejected. The recursion check would be either noisy (matching by name) or weak, and type-aware custom rules would be impossible.
- **Adding ast-grep for structural rules.** Rejected for now (see "Not used").
- **Semgrep instead of tidy.** Rejected. It has no type information or call graph in the free edition, and it adds a Python dependency.

## Evidence

Local clippy 0.1.100 run against sample violations:
- no lint fired on direct or mutual recursion;
- 7 warnings on one raw stencil line;
- `float_cmp` was silent in a function named `eq` and fired after renaming it.

- dylint 6.4.0 (2026-11-10): libraries are tied to a nightly and `clippy_utils` has no stability guarantee ([how dylint works](https://github.com/trailofbits/dylint/blob/master/docs/how_dylint_works.md)).
- Dropped mutual-recursion lint: [rust-clippy discussion #8446](https://github.com/rust-lang/rust-clippy/discussions/8446).
- Semgrep free-edition limits: [docs.semgrep.dev](https://docs.semgrep.dev/semgrep-pro-vs-oss).
- TigerBeetle's `tidy.zig` (ban list with replacement hints, function-length ratchet, runs inside the test suite): research report, section on clippy and tidy.
- BeautifulRust's lint-fixture technique: [repo](https://github.com/morganlinton/BeautifulRust).
