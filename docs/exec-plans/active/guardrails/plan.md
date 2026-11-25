# Guardrails: build the mechanisms that the documents promise

## Goal

The ADRs and `docs/TIGER_STYLE.md` name checks that nobody has built. Until one exists, the style
guide tags its rule `[planned:<id>]` instead of claiming enforcement, and the rule is held by
review only. This plan is the backlog of those mechanisms: one task per row of `PLANNED` in
`xtask/src/tidy/registry.rs`, with the same id. `docs/RULES.md` lists them with their references.

Gaps that nobody has promised a check for are not here. They are in `docs/learnings/`
(ADR-0019). A learning becomes a task of this plan when the planner picks it up.

## How the backlog stays honest

- Tidy rule `doc-tag`: a tag in the style guide must name something that exists, so a planned
  mechanism cannot be passed off as built (ADR-0017).
- Tidy rule `planned-task`: every row of `PLANNED` has a task here, so a promise cannot be
  dropped silently (ADR-0017).
- Tidy rule `rules-doc`: `docs/RULES.md` is generated from the registry and cannot drift.

## Finishing a task

A mechanism is built when its acceptance tests pass on `main`. The same PR then:

1. deletes the row from `PLANNED` and, for a tidy rule, adds the row to `RULES` (or to a ban
   table) with its two fixtures in `xtask/fixtures/tidy/`;
2. retags the style guide from `[planned:<id>]` to the real mechanism;
3. regenerates `docs/RULES.md` with `cargo xtask rules --write`;
4. leaves `passes` alone: only CI sets it (ADR-0011).

`docs/playbooks/add-a-guardrail.md` has the steps in full.

## Order

The tasks form one chain: each depends on the one before it.

- **Why a chain.** Steps 1 to 3 above edit `xtask/src/tidy/registry.rs`, `docs/TIGER_STYLE.md`
  and `docs/RULES.md` in every task. Two unfinished tasks may not own the same path (ADR-0011),
  so the tasks cannot run side by side.
- **Why this order.** The first seven need nothing that does not exist yet. The rest need the
  reference crate or the solver's time loop, which belong to the first equation-system plan.
- **To relax it,** the planner can take the three shared files out of `owner_paths` and accept
  a small rebase on them. That is a decision for the maintainer.

| # | Task | Mechanism | Needs |
|---|---|---|---|
| 1 | `float-eq-in-eq-fn` | tidy rule | nothing |
| 2 | `clippy-fixtures` | test | nothing |
| 3 | `doc-example-sync` | test | nothing |
| 4 | `usize-alias` | dylint lint | nothing |
| 5 | `recursion-cross-crate` | dylint lint | nothing |
| 6 | `glossary` | document and tidy rule | nothing |
| 7 | `tasks-schema` | CI job | nothing |
| 8 | `thresholds-file` | code-owned file | the reference crate |
| 9 | `reference-suite` | tests | `thresholds-file`, a solver |
| 10 | `tier-tag` | tidy rule | reference tests to tag |
| 11 | `floor-activation-count` | test | `reference-suite`, a scheme with a floor |
| 12 | `alloc-after-init` | test | the solver's time loop |
| 13 | `determinism-hash` | CI job | a canonical problem set |

Every task under `crates/reference/`, `xtask/`, `lints/`, `.github/` or `docs/TIGER_STYLE.md`
touches protected paths, so its PR waits for the code owner (ADR-0008, ADR-0015). A reference is
written by a different session from the one that implements the feature it tests (ADR-0007).

## Tasks

### float-eq-in-eq-fn

Clippy's `float_cmp` skips functions whose name contains `eq` (ADR-0006, Known gaps). Add a tidy
rule that reports `==` and `!=` between float expressions in such functions, in nami crates
outside tests. Tidy has no types, so the rule needs a syntactic stand-in for "float". Start
narrow: a float literal on either side, or operands that are parameters declared `f64`. Say in
the rule's summary what it does not see.

Out of scope: comparisons outside `eq`-named functions, which clippy already covers.

### clippy-fixtures

`clippy.toml` bans paths by name, and the workspace lint table switches lints on by name. A
toolchain bump can rename a lint or stop a path from resolving, and the ban then goes silent.
Add one planted violation per `disallowed-methods` and `disallowed-types` entry and per lint
that a `[clippy:..]` or `[rustc:..]` tag in the style guide names, under `xtask/fixtures/clippy/`,
and a test that runs clippy on them and expects each to fire.

Out of scope: lints of the pedantic group that no tag names.

### doc-example-sync

The code blocks in `docs/TIGER_STYLE.md` are not compiled, so an example can break a rule that
the text around it states. Extract every `rust` block and run it through the same gate as the
code: it compiles, clippy is clean and the tidy source rules pass. A block that is also real
code (`update_pencil`) must equal its source in `crates/kernels/`.

Out of scope: blocks marked as fragments, which need a marker that the task defines.

### usize-alias

Tidy's `usize` rule reads syntax, so `type Idx = usize;` in another crate, or a local variable
whose type is inferred, gets around it (ADR-0002). Add a dylint lint that reports a value of
type `usize` in a public signature or a field of a nami crate outside `base::index`, after
aliases are resolved. Decide in the task whether locals are in scope, and say so in the lint's
documentation: slice lengths are `usize`, and they must stay usable.

### recursion-cross-crate

`nami_recursion` checks each crate on its own. A generic function in `kernels` that calls a
trait method, with an impl in `solver` that calls the function back, is a cycle that neither
check sees: the bodies of upstream functions are not available under `cargo check`. Find a
sound approximation, for example: a call to a non-local generic function whose type arguments
name a local type may call any local impl of the traits in its bounds. It must not report
`xtask`'s `syn` visitors without a documented exemption for tooling.

### glossary

`TIGER_STYLE.md` says that each word has one meaning, recorded in `docs/glossary.md`. The file
does not exist. Write it from the terms the ADRs and the code already use ("flux" and
`flux_physical`, "pencil", "ghost layer", "reference", "tier"), and add a tidy rule that checks
its form: one entry per term, sorted, no term defined twice.

Out of scope: checking that the code uses each word in its one meaning. That stays with review.

### tasks-schema

ADR-0011 promises that CI validates every `tasks.json` and rejects a PR in which an agent sets
`passes`. Add the schema check to tidy and the `passes` check to CI.

### thresholds-file

ADR-0007 puts every reference tolerance in one human-owned place. Create it under
`crates/reference/`, with the derivation of each value next to it, and make the reference
checks read their tolerances from there by name.

### reference-suite

Fill `crates/reference` with the tiers of ADR-0007 for the first equation system: invariants
(conservation to round-off, the wave-speed bound, ghost cells written before they are read),
symmetry, convergence order, exact solutions, and one reference per boundary-condition type.
A no-op scheme must fail it. The style guide's `[planned:reference-suite]` tags name the rules
that these tests carry.

### tier-tag

Every reference test names its tier (ADR-0007). Add a tidy rule that requires the tier in the
goal comment of each test under a `tests/` directory of `crates/reference`, and fix the tests
that exist by then.

### floor-activation-count

A floor or limiter that is part of a scheme counts each activation and reports the count per
step (`TIGER_STYLE.md`, Physical admissibility). Add the counter to the first scheme that has
one, and a reference test that fails on an unexpected count. This is what turns "no silent
floors" from a review rule into a check.

### alloc-after-init

`crates/alloc-guard` can prove that code does not allocate, and nothing uses it yet. Add a test
in `crates/solver` that installs the counting allocator, runs `Solver::init`, and wraps a number
of time steps in `assert_no_allocations` (ADR-0003).

### determinism-hash

ADR-0005 promises bitwise identical output across platforms. Add a command that runs a
canonical problem set and hashes the raw output bits, a blocking CI step that runs it twice
with the same seed and compares, and a post-merge step that compares Linux x86_64 with macOS
arm64. A changed hash labels the PR `changes-results` (ADR-0008).
