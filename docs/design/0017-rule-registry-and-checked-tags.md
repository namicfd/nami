# ADR-0017: One rule registry, enforcement tags that tidy checks, and the gaps they exposed

- Status: Proposed
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0001, ADR-0002, ADR-0004, ADR-0005, ADR-0006

## Context

ADR-0006 says that rules in `AGENTS.md` are requests and lints are guarantees. A review on 2026-11-25 tested the guarantees: it planted code that the documents call mechanically rejected and ran the full gate. Ten cases passed `just check-full` (see Evidence).

The causes fall into three groups.

- **Rules that were promised but never written.** ADR-0004 says tidy rejects `debug_assert!` outside `kernels`. ADR-0005 and ADR-0006 say a dylint rule flags naive float sums. `TIGER_STYLE.md` tags "no `let _ =` on a `Result`" as `[clippy]`. None of these existed.
- **Rules that were narrower than their one-line form.** The bounded-loop rule matched `loop` and `while`, but not `for` over an open range. The `Vec` rule looked at the outermost type of a field. The `#[expect]` budget matched the text `#[expect(` and missed `cfg_attr`. The recursion lint could not follow a trait call on a type parameter, which is the normal shape of a generic kernel.
- **Documents that described intent.** `TIGER_STYLE.md` said its tags name "the intended mechanism". About 60 tags could therefore not be told apart: enforced or planned. ADR-0006 also promised a fixture crate and a snapshot of the lint table, and neither was built.

Adding a rule was expensive as well. Every syntax rule lived in one 570-line visitor, and nothing listed the rules that exist.

## Decision

### 1. One registry

`xtask/src/tidy/registry.rs` lists every tidy rule: its id, its kind (decided from one file, or from the whole workspace), a one-line summary and the ADR or style-guide section that gives the reason. A violation can only be created with a registered id.

### 2. Every rule has a firing and a non-firing fixture

- `xtask/fixtures/tidy/<rule-id>.fires.fixture` breaks the rule, and `<rule-id>.passes.fixture` is the nearest correct code. The first line of a fixture names the virtual path that decides which rules apply to it.
- A test walks the registry and fails when a rule has no fixtures, when the firing fixture does not fire, or when the passing one does.
- Workspace-wide rules are covered by one test that builds a broken temporary workspace and compares the rules that fired with the registry.

This replaces the fixture crate of ADR-0006 for tidy. Dylint lints keep their UI tests in `lints/ui/`.

### 3. Ban tables

Two tables in the registry turn the commonest kind of guardrail into data:

- **path bans:** a leading path segment that may appear only in named files (first row: `libm` outside `kernels::math`);
- **macro bans:** macros that may appear only in named directories (first row: `debug_assert!` outside `kernels`).

Adding a ban is one row and two fixtures.

### 4. Tags in `TIGER_STYLE.md` name a mechanism that exists

| Tag | Tidy checks that |
|---|---|
| `[tidy:<rule-id>]` | the rule is in the registry |
| `[clippy:<lint>]` | the lint is `warn` or `deny` in the workspace lint table (`clippy:pedantic` for lints of that group) |
| `[rustc:<lint>]` | likewise, in the rustc table |
| `[dylint:<lint>]` | the lint is declared in `lints/src/` |
| `[planned:<id>]` | the id is in the registry's table of planned mechanisms |
| `[compiler]`, `[fmt]`, `[deny]`, `[review]` | the form is valid |

A tag may claim only what the tooling does today. The tags `[hook]` and `[reference]` are gone: hooks give feedback and enforce nothing, and the reference suite does not exist yet.

This also stands in for the lint-table snapshot of ADR-0006: a lint that the style guide names cannot be removed or set to `allow` without tidy failing.

### 5. Planned mechanisms are listed, and each is a task

The registry has a second table for mechanisms that a document promises and nobody has built. Each entry has a task with the same id in `docs/exec-plans/active/guardrails/tasks.json` (ADR-0011). When a mechanism is built, its entry moves to the rule table and its tags change from `planned:` to the real mechanism. Tidy fails until both are done.

### 6. `docs/RULES.md` is generated

`cargo xtask rules --write` writes the list of tidy rules, ban tables, dylint lints and planned mechanisms, and counts the rules that are enforced by review alone. Tidy fails when the file is stale.

### 7. Gaps closed

| Rule | Change | Amends |
|---|---|---|
| `direct-libm` (tidy, path ban) | New. `libm` may be named only in `crates/kernels/src/math.rs`. | ADR-0005 |
| `debug-assert-placement` (tidy, macro ban) | New. `debug_assert!` and its variants only inside `crates/kernels/`. | ADR-0004 |
| `unbounded-loop` (tidy) | Also a `for` over a range without an end, unless `take` or `zip` bounds it. | ADR-0006 |
| `vec-field` (tidy) | `Vec` anywhere inside a field type. | ADR-0003 |
| `expect-budget` (tidy) | Counts attributes on the token level, so `cfg_attr(.., expect(..))` and attributes inside macros count. | ADR-0006 |
| `usize` (tidy) | Also private function signatures. Trait impls are exempt, because the trait dictates the signature. | ADR-0002 |
| `magic-tolerance` (tidy) | New. A float literal of magnitude `1.0e-6` or less must be a documented `const`. | `TIGER_STYLE.md` |
| `runtime-dependency` (tidy) | Also `[dependencies.NAME]` table headers, and `package = ".."` renames. | ADR-0001 |
| `let_underscore_must_use` (clippy) | Enabled at `deny`. | `TIGER_STYLE.md` |
| `nami_recursion` (dylint) | The call graph follows generic instances: a trait call on a type parameter is resolved with the caller's type arguments. | ADR-0006 |
| `nami_float_reduction` (dylint) | New. `Iterator::sum` with a float result, and `fold` that adds floats, outside tests. | ADR-0005 |

New rules that check the documents themselves: `doc-tag` (point 4), `rules-doc` (point 6), `planned-task` (point 5) and `learning-format` (ADR-0019).

### 8. Tidy can check another tree

`cargo xtask tidy --root <dir> [--policy <dir>]` runs this binary's rules on another checkout. With `--policy`, the `#[expect]` budget is read from that directory, and the rules that compare documents with the binary's own registry (`doc-tag`, `rules-doc`, `planned-task`) are skipped. ADR-0019 uses this for the base gate.

## Enforcement

- The registry test and the fixture files in `xtask/`, which run in `just check`.
- The tidy rules `doc-tag`, `rules-doc` and `planned-task`.
- `Violation::new` asserts that its rule id is registered.
- CODEOWNERS on `xtask/`, `lints/`, `Cargo.toml`, `clippy.toml` and `docs/TIGER_STYLE.md` (ADR-0008).

## Consequences

- A document can no longer claim a mechanism that does not exist, and a rule cannot exist without a test that it fires.
- The planned mechanisms and the review-only rules are visible in one generated file. They are the backlog of guardrail work.
- Most new guardrails are a row in a table.

### Known gaps

- **Silent floors are still a review rule.** `magic-tolerance` catches the usual form, `rho.max(1.0e-12)`. A floor against a named constant passes. Banning `max` and `min` against constants was rejected, because slope limiters and HLL wave speeds use them legitimately. The planned mechanism is `floor-activation-count`.
- **`usize` in local variables and private constants stays allowed.** Slice lengths and array sizes are `usize`.
- **Recursion across crates through a generic callback is not seen,** because the bodies of upstream functions are not available under `cargo check`. Planned as `recursion-cross-crate`.
- **A float sum written as a `for` loop with `+=`** is not flagged. It cannot be told apart from other accumulations.
- **Clippy bans have no fixtures.** A `clippy.toml` path that stops resolving after a toolchain bump goes silent. Planned as `clippy-fixtures`.

## Alternatives considered

- **Keep prose tags and audit them by hand.** Rejected. This review was that audit, and it found the drift only after the tooling was called complete.
- **A rule file in TOML, read at run time.** Rejected. It needs a parser, and xtask has no dependency for one (ADR-0001). Rust constants are typed and checked by the compiler.
- **The fixture crate of ADR-0006, compiled under every tool.** Rejected for tidy. Tidy judges text, so a text fixture per rule is simpler and cannot break the build.
- **Link an unresolved trait call to every impl of the trait** (recursion lint). Rejected. It reports the generic decorator, `impl<T: Flux> Flux for Limited<T>` calling `self.inner.flux()`, which is not recursion, and nami has no way to wave a false positive through.
- **Report every `max`, `min` or `clamp` against a literal.** Rejected, as explained under Known gaps.
- **Ban open ranges outright.** Rejected. `(0_u32..).zip(values)` is the natural way to count with a `u32` instead of `enumerate()` and a cast (ADR-0002).

## Evidence

Planted in `crates/kernels` and `crates/solver` on 2026-11-25. `just check-full` passed with all of them in place.

| Planted code | Documented rule | Why it passed |
|---|---|---|
| `libm::sqrt(x)` outside `kernels::math` | AGENTS.md 7 | no check existed |
| `values.iter().sum::<f64>()` | AGENTS.md 7, ADR-0005 | the dylint rule was never written |
| `for step in 0_u32.. { .. }` | AGENTS.md 1 | tidy matched only `loop` and `while` |
| `Cycle::value` → `through_generic::<Cycle>` → `Cycle::value` | AGENTS.md 1 | the lint did not resolve a trait call on a type parameter |
| `debug_assert!` in `solver` | AGENTS.md 5, ADR-0004 | the tidy rule was never written |
| a field of type `Option<Vec<f64>>` | AGENTS.md 2 | only the outermost type was checked |
| `#[cfg_attr(all(), expect(..))]` | the `#[expect]` budget | the budget matched text |
| a private `fn f(n: usize)` | AGENTS.md 3 | only public signatures were checked |
| `let _ = text.parse::<u32>();` | `TIGER_STYLE.md`, Errors | no lint was enabled |
| `density.max(1.0e-12)` | AGENTS.md 8 | review only |

A directly recursive function in the same file was reported, which shows that the lint ran.

- `#[cfg_attr(all(), allow(..))]` is caught by clippy's `allow_attributes`. Only `expect` slipped through.
- A `[dependencies.serde]` table header was not treated as a runtime dependency by tidy (found by reading `is_runtime_dependency_section`). `cargo deny` still rejects crates that are not on its allow-list.
