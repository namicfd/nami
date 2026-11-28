# ADR-0012: Budget and spend controls

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer
- Summary: Pilot at ~$1–3k/month with per-session and daily caps. Scaling up is decided on measured cost per merged PR.

## Context

The only well-documented cost anchor is Anthropic's C compiler: just under $20k for 16 agents over 2 weeks, about **$90 per agent-day running continuously**, at early-2026 model prices. Larger swarms have no primary cost data; critics put Gas Town at "thousands of dollars a month". Throwing more agents at a single shared failure does not help: they overwrite each other's fixes.

## Decision

1. **Starting budget for phase 2 (ADR-0010): a pilot of about $1–3k per month.** That is roughly one agent running continuously, or 2–3 part-time.
2. **Spend controls, from the first unattended run:**
   - **Per session:** `claude -p --max-budget-usd` caps every worker run.
   - **Per day:** the runner keeps a running total and stops launching sessions once the daily cap is reached. There is also a manual kill switch, a file or flag the runner checks before each launch.
   - **Concurrency:** never more agents than there are independent ready tasks (ADR-0011) or independent failing references.
   - **Model mix:** janitor and planner roles may use cheaper models when the metrics allow it.
3. **Metrics** (shared with ADR-0008):
   - cost per merged PR;
   - cost per task that reaches `passes: true`;
   - revert rate;
   - surviving mutants.

   They are reported weekly.
4. **Review point:** after the first month of phase 2, a new ADR either keeps the pilot level, moves to about $5–10k per month, or plans bursts at C-compiler scale (about $20k per 2-week push) for well-specified, well-referenced milestones. The decision rests on measured cost per merged PR, not on the anchor above.

Phase 1 is interactive and its cost is mostly maintainer time. No additional controls apply in that phase.

## Enforcement

- The runner (`tools/runner/`) refuses to start without the per-session and daily caps configured.
- Weekly metrics come from a CI job or a script whose output is committed under `docs/metrics/`.

## Consequences

- Spending is bounded and visible from the first unattended run, and scaling up is a measured decision.
- The pilot is too small to show swarm-scale coordination problems. Those are deferred to the review point.

## Alternatives considered

- **Medium (about $5–10k/month) from the start.** Rejected. It would scale before the gates are proven.
- **C-compiler-scale bursts.** Deferred until there is a milestone with strong references.
- **Leaving the budget undecided.** Rejected. Controls need a number to enforce.

## Evidence

- C compiler cost: 2,000 sessions, 2B input and 140M output tokens, just under $20k ([Anthropic](https://www.anthropic.com/engineering/building-c-compiler)).
- Gas Town cost criticism: [Maggie Appleton](https://maggieappleton.com/gastown).
