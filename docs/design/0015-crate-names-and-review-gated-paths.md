# ADR-0015: Unprefixed crate names, "references", and protected paths gated at review

- Status: Accepted
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0007, ADR-0008, ADR-0010

## Context

- **Crate names.** The workspace crates carried a `nami-` prefix (`nami-core`, `nami-kernels`, …). None of them is published (`publish = false`), so the prefix only made paths and `use` lines longer.
- **"Oracle".** The research literature calls a trusted check an "oracle". In nami's code and docs, "reference" (reference solution, reference data) is the plainer word and matches how numerical people talk.
- **Protected paths.** ADR-0008 gates the guardrails (manifests, tool configuration, `xtask/`, `lints/`, CI, agent instructions, ADRs, references) behind human approval. Its enforcement had two layers:
  - **Server side:** CODEOWNERS with required code-owner review.
  - **Client side:** Claude Code `permissions.ask` rules on those paths, and `AGENTS.md` telling agents to edit them only when a task says so.

  The client-side layer blocks unsupervised execution. Under `claude -p`, an `ask` rule is a denial, so an agent whose task genuinely needs a manifest or a tidy-rule change cannot finish it, and stops. nami optimises for unattended runs (ADR-0010, phase 2). The human sign-off that ADR-0008 asks for already happens at PR review, which is the only layer that cannot be bypassed anyway.

## Decision

1. **Crates drop the `nami-` prefix.** The directory under `crates/` and the package name are the same:
   - `nami-core` becomes **`base`**. `core` would shadow Rust's built-in `core` crate, which every `no_std` crate uses.
   - `nami-kernels` becomes `kernels`, and `nami-solver` becomes `solver`.
   - `nami-alloc-guard` becomes `alloc-guard`.
   - `nami-oracle` becomes `reference` (point 2).
   - The dylint library keeps its name `nami_lints` and its lint `nami_recursion`. Lint names are a global namespace on the command line, so the prefix helps there.
2. **"Oracle" becomes "reference"** everywhere in nami's code and docs:
   - the crate `reference`;
   - `tools/reference-gen/`;
   - the style-guide tag `[reference]`;
   - "reference tests" and "reference tiers".

   ADR-0007 keeps its number under the new title "Numerical references". The research report in `docs/research/` keeps the literature's term.
3. **Mechanical renames update every ADR in place.** Points 1 and 2 were applied to all ADRs, accepted ones included. Only identifiers and the term changed, and no decision did. This ADR is the record of that edit. Any future rename works the same way: one ADR records it, and the text of the others follows.
4. **Protected paths are gated at review, not at edit time.**
   - An agent may edit any protected path when its task needs it. Nothing local blocks or prompts.
   - A PR that touches a protected path needs code-owner approval before it can merge (ADR-0008, unchanged).
   - The agent keeps such changes to what the task needs and puts them in their own commit. The PR description lists every protected file changed and says why.
   - When a rule is wrong for a task, the agent changes the rule openly, adds an ADR with status `Proposed`, and leaves the PR for a human. It never works around the rule.

## Enforcement

- `crates/*` directory names equal package names. The tidy rule `runtime-dependency` allows `libm` plus exactly the crate names found under `crates/`, so it no longer depends on a prefix.
- `.claude/settings.json` has no `permissions.ask` rules. The hooks stay: post-edit format plus single-file tidy, and `just check` at stop.
- `.github/CODEOWNERS` lists the protected paths, including `/crates/reference/`. The `main` ruleset requires code-owner review, so a PR touching any of them cannot enter the merge queue without the maintainer's approval.
- `AGENTS.md` says what to do when touching a protected path: keep the change minimal, use its own commit, and list it in the PR.
- This replaces the client-side bullets in the Enforcement sections of:
  - ADR-0007, the PreToolUse hook on reference data;
  - ADR-0008, the PreToolUse hooks on protected paths;
  - ADR-0010, the protected-path hooks for subagents.

## Consequences

- Unattended agents can finish tasks that need a manifest, tidy or CI change. They no longer stop on a permission prompt that nobody answers.
- **CI runs the PR's own guardrails.** A PR that weakens `justfile`, `xtask/` or `clippy.toml` passes its own gate. The code-owner review is then the only thing between that PR and `main`, so the maintainer reads the protected part of the diff first.
- More PRs wait for a human, namely every PR that touches a protected path. If that becomes a bottleneck, the remedy is to split tasks so that guardrail changes land as small separate PRs, not to drop the review.
- Crate paths and `use` lines are shorter. `base` is a generic name, which is fine because nothing is published.

## Alternatives considered

- **Keep `permissions.ask` and grant edits per task** (`--allowedTools` in the runner). Rejected. Every task that touches a guardrail would need runner configuration, and the server-side review is still required anyway.
- **`permissions.deny` on the most sensitive paths** (`thresholds.toml`, reference data). Deferred. That is the strongest case for a local block, but tasks that add a new equation system legitimately add reference data. To revisit if a weakened threshold ever slips through review.
- **Name the shared crate `core`.** Rejected: it shadows the built-in `core` crate.
- **Keep "oracle".** Rejected by the maintainer in favour of the plainer term.

## Evidence

- Claude Code permission rules: `ask` rules are answered interactively, and in non-interactive (`-p`) mode a call that needs approval is denied.
- A package named `core` builds, but a `no_std` crate that depends on it fails to compile: `cannot resolve a prelude import`, and `core::cmp` is not found. Checked locally with Rust 1.100.0.
