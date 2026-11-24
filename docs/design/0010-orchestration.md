# ADR-0010: Agent orchestration

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer

## Context

The best-documented swarm runs used simple coordination. Anthropic's C compiler used a bash `while true` loop with one Docker container per agent, `claude -p` sessions, and git lock files, with no orchestrator agent. Cursor found that shared locks and integrator agents became bottlenecks, and that a planner/worker/judge cycle worked. Gas Town has been retired.

Claude Code's options as of 2026-11:

- **Agent teams:** still experimental. Teammates **share the lead's working directory**, with no worktree isolation.
- **Subagents with `isolation: worktree`:** stable, up to 20 concurrent, but tied to one interactive session.
- **Headless `claude -p`:** scriptable, with `--max-budget-usd`, `--output-format json` and `--bare`. It needs an explicit permission mode.

Adherence to instructions decays within a long session, by about 5.6% per extra function generated ([arXiv 2605.10039](https://arxiv.org/abs/2605.10039)). That argues for short, fresh sessions.

Docker is not installed on the maintainer's Mac.

## Decision

Orchestration happens in two phases.

### Phase 1 (now): interactive lead with worktree subagents

**Scope:** building the guardrails (P0), the oracle crate and the first equation system.

- The maintainer works with one interactive Claude Code lead session.
- The lead dispatches one subagent per task with `isolation: worktree`. Each subagent opens a PR; nothing is committed directly to `main`.
- The point of this phase is to see every rule tested against real agent behaviour while a human is watching.

### Phase 2 (later): bash-loop runner

**Scope:** unattended scale.

- A small, vendor-agnostic runner in `tools/runner/`. Each worker loops through:
  1. sync `main`;
  2. claim a task (ADR-0011);
  3. run a fresh `claude -p` session (or `codex exec`) with an explicit permission mode and `--max-budget-usd`, in its own worktree inside a sandbox;
  4. open a PR;
  5. exit.

  One task per session.
- **Roles are prompts on a schedule, not long-lived agents:**
  - **planner:** turns specs and issues into tasks;
  - **workers:** implement tasks;
  - **judge/janitors:** review, deduplicate, update docs and quality scores.
- **No inter-agent chat.** Coordination happens only through the task files and the repo.
- **Sandbox:** Docker/OrbStack on the Mac, or Linux VMs/runners. This is decided when phase 2 starts.

**Exit criteria for phase 1:**

- every P0 guardrail exists and passes its fixture tests;
- the oracle crate covers the first equation system;
- the blocking gate runs in about 5 minutes;
- two consecutive weeks of phase 1 work where no regression reached `main`.

**Agent teams are not used** until they support worktree isolation.

## Enforcement

- Phase 1: the protected-path hooks and CODEOWNERS apply to subagent PRs like any other.
- Phase 2: each runner session has a budget cap (ADR-0012). The runner refuses to start without a sandbox.

## Consequences

- Nothing needs to be built for orchestration today.
- Phase 2 means a few hundred lines of runner script and a sandbox setup, written once and readable by humans.
- Because the runner is vendor-agnostic, `AGENTS.md` is the canonical instruction file and `CLAUDE.md` just imports it.

## Alternatives considered

- **Bash-loop runner from the start.** Rejected. It would scale before the gates have been proven.
- **Claude Code agent teams.** Rejected for now: experimental, no isolation, and long-lived contexts that drift.
- **Gas City or a custom framework.** Rejected: more complexity than the evidence supports.

## Evidence

- C compiler harness: [Anthropic](https://www.anthropic.com/engineering/building-c-compiler).
- Cursor planner/worker/judge: [Cursor](https://cursor.com/blog/scaling-agents).
- Agent teams limits: [Claude Code docs](https://code.claude.com/docs/en/agent-teams).
- CooperBench: cooperating agents about 30% less successful than solo ones ([arXiv 2601.13295](https://arxiv.org/html/2601.13295)).
