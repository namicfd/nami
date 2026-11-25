# Guardrails: build the mechanisms that the documents promise

## Goal

The ADRs and `docs/TIGER_STYLE.md` name checks that nobody has built. Until one exists, the style
guide tags its rule `[planned:<id>]` instead of claiming enforcement, and the rule is held by
review only. This plan is the backlog of those mechanisms: one task per row of `PLANNED` in
`xtask/src/tidy/registry.rs`, with the same id. `docs/RULES.md` lists them with their references.

## How the backlog stays honest

- Tidy rule `doc-tag`: a tag in the style guide must name something that exists, so a planned
  mechanism cannot be passed off as built (ADR-0006).
- Tidy rule `planned-task`: every row of `PLANNED` has a task here, so a promise cannot be
  dropped silently (ADR-0011).
- Tidy rule `rules-doc`: `docs/RULES.md` is generated from the registry and cannot drift.

## Finishing a task

A mechanism is built when its acceptance tests pass on `main`. The same PR then:

1. deletes the row from `PLANNED` and, for a tidy rule, adds the row to `RULES` (or to a ban
   table) with its two fixtures in `xtask/fixtures/tidy/`;
2. retags the style guide from `[planned:<id>]` to the real mechanism;
3. regenerates `docs/RULES.md` with `cargo xtask rules --write`;
4. leaves `passes` alone: only CI sets it (ADR-0011).

Steps 1 to 3 touch `xtask/src/tidy/registry.rs`, `docs/TIGER_STYLE.md` and `docs/RULES.md` in
every task, so the `owner_paths` of the tasks overlap on those three files. The planner should
hand out one task of this plan at a time, or expect a rebase on those files.

## Tasks by mechanism

| Mechanism | Tasks | Where the reason is |
|---|---|---|
| tidy rule | `float-eq-in-eq-fn`, `tier-tag` | ADR-0006, ADR-0007 |
| dylint lint | `float-reduction`, `usize-alias`, `recursion-cross-crate` | ADR-0005, ADR-0002, ADR-0006 |
| test | `alloc-after-init`, `reference-suite`, `floor-activation-count`, `doc-example-sync` | ADR-0003, ADR-0007, `TIGER_STYLE.md`, ADR-0013 |
| CI job | `determinism-hash`, `tasks-schema` | ADR-0005, ADR-0011 |
| code-owned file | `thresholds-file` | ADR-0007 |
| document | `glossary` | `TIGER_STYLE.md`, Naming |

## Order

Most tasks are independent. Two are not:

- `reference-suite` needs `thresholds-file`, because its tolerances live there.
- `floor-activation-count` needs `reference-suite`, because the count is asserted by a
  reference test.

`alloc-after-init`, `determinism-hash` and most of `reference-suite` also need the solver's time
loop, which belongs to the first equation-system plan, not to this one. They can be claimed once
that plan has landed a time loop.

Every task under `crates/reference/`, `xtask/`, `lints/`, `.github/` or `docs/TIGER_STYLE.md`
touches protected paths, so its PR waits for the code owner (ADR-0008, ADR-0015). A reference is
written by a different session from the one that implements the feature it tests (ADR-0007).
