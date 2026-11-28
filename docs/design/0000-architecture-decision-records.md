# ADR-0000: Record decisions as ADRs

- Status: Accepted (amendment proposed in ADR-20261128-1856)
- Date: 2026-11-24
- Deciders: maintainer
- Summary: Policy lives here, with rationale and an enforcement mechanism.

## Context

nami is implemented mostly by AI coding agents. Agents cannot ask why a rule exists, and they repeat whatever patterns they find. Decisions therefore have to be written down where an agent can read them, together with the reasoning and the mechanism that enforces them.

## Decision

1. Every decision that constrains how code is written, tested, merged or paid for is recorded as an ADR in `docs/design/NNNN-short-title.md`.
2. Each ADR has the sections **Context**, **Decision**, **Enforcement**, **Consequences**, **Alternatives considered** and **Evidence**. Enforcement names the mechanism (lint, tidy rule, dylint rule, hook, CI job, CODEOWNERS) that makes the rule hold without anyone having to remember it. A rule with no mechanism must say so.
3. Status is one of `Proposed`, `Accepted`, `Superseded by ADR-NNNN` or `Deprecated`.
4. After an ADR is accepted, only its status line and links may change. To change a decision, write a new ADR that supersedes the old one.
5. Agents may open PRs that propose ADRs (status `Proposed`). Only a human may accept one: `docs/design/` is code-owned (ADR-0008).
6. `docs/design/README.md` indexes all ADRs and has to be updated in the same PR as any ADR change.

## Enforcement

- CODEOWNERS on `docs/design/` (ADR-0008, ADR-0009).
- Tidy (ADR-0006) checks that every ADR file is listed in the index and has the required sections and a valid status.

## Consequences

- The ADRs are the source of truth for policy. `AGENTS.md` contains only the one-line rules and points here for the reasons.
- Changing a policy costs one short document and one human review. That cost is intended.

## Alternatives considered

- **Policy only in AGENTS.md.** Rejected. Instruction files change agent behaviour but barely change success rates, and long files reduce adherence (research report §1).
- **No written rationale.** Rejected. When agents do not know why a rule exists, they argue with it or route around it.

## Evidence

- Research report: [`docs/research/ai-agent-swarm-repo-readiness.md`](../research/ai-agent-swarm-repo-readiness.md), sections on context files and on progressive disclosure.
