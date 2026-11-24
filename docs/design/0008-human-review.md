# ADR-0008: Human review model

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer

## Context

A swarm produces far more PRs than one person can read. OpenAI's harness team merged about 3.5 PRs per engineer per day with agent-to-agent review. Reviewing everything makes the human the bottleneck and leads to rubber-stamping. Reviewing nothing lets agents loosen their own constraints when they get stuck, which is the failure the case studies report most often. LLM reviewers catch style and structure problems but miss physics bugs (ADR-0007).

## Decision

### Always requires human approval

These are enforced through CODEOWNERS and required reviews.

- **Guardrails:**
  - `Cargo.toml` (all of them) and `Cargo.lock`;
  - `deny.toml`, `clippy.toml`, the lint tables, `rust-toolchain.toml` and `.cargo/`;
  - `xtask/` (tidy and fixtures), `lints/` (dylint) and the expect-budget file;
  - `justfile`, `.github/` and `.claude/` (hooks, settings, agents, skills);
  - `AGENTS.md` and `CLAUDE.md`.
- **References:** `reference/`, `thresholds.toml`, reference data and `tools/reference-gen/` (ADR-0007).
- **Decisions:** `docs/design/` (ADR-0000).
- **Output changes:** any PR that changes output bits. The determinism hash (ADR-0005) changes, CI labels the PR `changes-results`, and CI posts before/after convergence and error numbers. The PR waits for a human, whatever paths it touches.

### Everything else merges automatically

The merge queue merges every other PR once all blocking gates pass (ADR-0009) and an AI review has run.

- **AI review:** a separate session with fresh context, made up of a `tigerstyle-reviewer` subagent and a `numerics-verifier` subagent. Their blocking comments must be resolved before the PR enters the queue. They complement the references and do not replace them.

### Sampled audits

- At the start, the maintainer reads about 5 random merged kernel PRs per week.
- The rate goes down while the metrics below stay stable.

### Metrics, tracked from the first merge

- surviving mutants;
- `#[expect]` count;
- reference failures caught per merged PR;
- revert rate;
- janitor-PR volume;
- cost per merged PR (ADR-0012).

### Agent identity

Agents act through their own identity, a GitHub App (ADR-0009). This is required because GitHub does not let a user approve their own PR, so CODEOWNERS review only works if agents are not the maintainer.

## Enforcement

- `.github/CODEOWNERS` lists the paths above. The branch ruleset requires code-owner review.
- A CI job compares the determinism hash with `main` and applies `changes-results`. A ruleset or required check blocks such PRs until a human approves.
- Claude Code PreToolUse hooks block agent edits to the protected paths (defence in depth; the server-side rules are authoritative).

## Consequences

- Human attention goes where agents are weakest (references, guardrails, result changes) and nowhere else.
- A subtle bug that changes no output bits and passes every reference can merge unseen. The sampled audits and mutation testing are the defence against that.

## Alternatives considered

- **Review every PR.** Rejected. It does not scale beyond a few agents.
- **Fully autonomous, including guardrails.** Rejected. Agents loosen their own constraints.

## Evidence

- OpenAI harness engineering (agent-to-agent review; 20% of time spent on clean-up before it was automated): [openai.com](https://openai.com/index/harness-engineering/).
- LLM reviewers missed semantic PDE bugs: [arXiv 2509.25194](https://arxiv.org/html/2509.25194).
