# ADR-0020: Guardrails that grow: learnings, the pull-request record and the base gate

- Status: Proposed
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0008, ADR-0015
- Summary: Agents add guardrails at the lowest rung that works, or record a learning. The PR description lists protected files and guardrail gaps. The base branch's tidy runs on every PR's tree.

## Context

The guardrails are meant to grow with the project: every mistake an agent makes should become a check that stops the next agent from making it. A review of the setup on 2026-11-25 found that nothing made this happen.

- **Nobody was asked to add a rule.** `AGENTS.md` said what to do when a rule is wrong for your case. It never said what to do when a rule is missing. `TIGER_STYLE.md` notes that "a rule that keeps getting broken in review is a candidate for a mechanism", but names no owner and no process.
- **A discovery had nowhere to go.** A session has one task, may touch only that task's `owner_paths` and may leave no debt markers. `docs/exec-plans/`, where unfinished work is recorded, did not exist. What an agent learned ended with its session.
- **Tightening cost as much as loosening.** Every guardrail path is protected (ADR-0015), so a PR that adds a ban waits for the same review as a PR that removes one, and the reviewer cannot tell them apart without reading the diff.
- **A PR is judged by its own gate.** ADR-0015 records this: CI runs the PR's `xtask/`, so a PR that weakens tidy passes tidy.
- **The PR description carried rules that nothing checked.** `AGENTS.md` asks for a list of the protected files a PR changes. No template asked for it and no check read it.

## Decision

### 1. Agents add guardrails

When one of these happens, the agent turns it into a check or records it (point 2):

- a bug, or a wrong turn that cost time, would have been caught by a check;
- a reviewer says something that no check enforces;
- an error message did not say how to fix the problem;
- a rule fired on correct code.

**The ladder.** Use the lowest rung that catches the problem. Lower rungs are cheaper to add, cheaper to review and harder to get around.

| Rung | Mechanism | Typical cost |
|---|---|---|
| 1 | A type, or the compiler (`no_std`, a newtype, a private field) | a design change |
| 2 | A clippy lint in the workspace table, or a ban in `clippy.toml` with a reason | one line |
| 3 | A row in a tidy ban table (ADR-0018) | one row and two fixtures |
| 4 | A tidy rule | a visitor method, a registry entry and two fixtures |
| 5 | A dylint lint | a lint pass and a UI test, on the pinned nightly |
| 6 | A reference or invariant test (ADR-0007) | a reference PR, written by another session |
| 7 | A playbook in `docs/playbooks/`, or a paragraph in `TIGER_STYLE.md` tagged `[review]` | prose |
| 8 | A line in `AGENTS.md` | prose that every session pays for |

Rungs 7 and 8 are requests, not guarantees (ADR-0006). They are for what cannot be checked.

**Scope.** A guardrail that the current task needs goes into the same PR, in its own commit (ADR-0015). Anything else becomes a learning, so that a kernel PR does not also rewrite tidy.

**When an ADR is needed.** A check for a rule that an ADR or `TIGER_STYLE.md` already states needs no new ADR: the decision exists, and only its mechanism is new. A new constraint needs a paragraph in `TIGER_STYLE.md`. A change that reverses or narrows an earlier decision needs an ADR with status `Proposed` (ADR-0000).

**A new rule that fires on existing code** is fixed by changing that code in the same PR. The rule is never narrowed to make it pass.

### 2. Learnings

`docs/learnings/` holds one Markdown file per learning, named `YYYY-MM-DD-<slug>.md`.

- **Content.** A status line, `## What happened` and `## Proposed guardrail`. The proposal names a rung of the ladder.
- **Status.** `Open`, then `Codified in <rule id, lint or ADR>` when a check enforces it, or `Rejected` with a reason. The PR that adds the check sets the status. Only a human rejects.
- **Access.** The directory is not protected, and any task may add a file to it, whatever its `owner_paths` say. A learning must cost the agent one small file and no review.
- **One file each,** because parallel agents that append to one shared file conflict (ADR-0009 evidence).
- **Open learnings are the guardrail backlog,** next to the planned mechanisms in `docs/RULES.md` (ADR-0018). The planner turns them into tasks (ADR-0011), and a janitor session may pick one directly.

### 3. The pull-request description is a record

`.github/pull_request_template.md` has four sections. `cargo xtask pr-body` checks the last two.

| Section | Content | Checked |
|---|---|---|
| `## Summary` | What changed and why. Output changes with before/after numbers (ADR-0008). | no |
| `## Rule changes` | `None`, or each changed check or threshold, whether it tightens or loosens, and the `Proposed` ADR. | no |
| `## Protected files` | `None`, or every protected path the PR changes, with the reason. | Every changed path that `.github/CODEOWNERS` lists is named, itself or through a directory. |
| `## Guardrail gaps` | `None`, or the learning files the PR adds. | The section is filled in, and every file it names exists. |

`cargo xtask protected` prints the changed protected paths, so the agent does not have to work them out.

### 4. The base gate

A CI job replays tidy across the two revisions of a guardrail PR.

| Run | Fails when | Reading |
|---|---|---|
| The **base branch's** tidy on the **PR's** tree | the PR's code breaks a rule that `main` enforces | **Loosening.** The PR is green only because it changed the rule. |
| The **PR's** tidy on the **base** tree | `main`'s code breaks a rule that the PR enforces | **Tightening.** The job summary lists what the new rule found. |

- Both runs use tidy's policy mode (`--policy`, ADR-0018): the `#[expect]` budget comes from the base branch, and the rules that compare the docs with the binary's own rule registry are skipped, because two revisions with different registries fail those by construction.
- The job is **not a required check.** A legitimate loosening, approved through an ADR, must still be able to merge.

### 5. How review uses this

Every guardrail PR still waits for the code owner (ADR-0015). The two signals say how to read it.

| Signal | The code owner checks |
|---|---|
| Base gate red, or `Rule changes` says "loosens" | The `Proposed` ADR, first. This is the case ADR-0008 exists for. |
| Base gate green and only additions | That the rule has a firing and a non-firing fixture, that its message names the fix, and that it does not fire on correct code. This should take a minute. |

## Enforcement

- `AGENTS.md`, section "Leave the guardrails stricter than you found them": the triggers, the ladder and the pointer to `docs/playbooks/add-a-guardrail.md`.
- Tidy rule `learning-format`: file name, status line and headings of every learning.
- `.github/pull_request_template.md`, and the `pr body` job in `.github/workflows/guardrails.yml`, which runs `cargo xtask pr-body`.
- The `base gate` job in the same workflow.
- `.github/CODEOWNERS` deliberately does not list `docs/learnings/` or `docs/playbooks/`.
- **No mechanism:** whether an agent writes a learning when it should. The PR-body check forces an explicit `None`, nothing more. The sampled audits (ADR-0008) are the place to notice a PR that says `None` and should not.

## Consequences

- A guardrail gap costs an agent one file, and a ban costs one row. The backlog of gaps is in the repo, where the planner and the janitors can read it.
- The code owner sees at a glance whether a guardrail PR loosens anything, and what a new rule would have caught.
- **The base gate replays tidy only.** A loosened clippy level, `clippy.toml` entry, `deny.toml` list, dylint lint or `justfile` recipe shows only in the diff. The doc-tag check (ADR-0018) covers part of this: a lint that the style guide names cannot be removed without tidy failing.
- **`pr-body` reads `.github/CODEOWNERS` from the PR's own tree.** A PR that deletes a pattern shortens the list it has to write. GitHub applies the base branch's file, so the review itself is not bypassed.
- **The first PR after this ADR has no base gate,** because the base branch's tidy cannot check another tree yet. The job skips itself in that case.
- More small PRs touch protected paths. That is intended (ADR-0015): they are quick to review, and the alternative is that the gap stays open.

## Alternatives considered

- **Merge tightening-only PRs without review.** Rejected for now. Whether a change to `xtask/` only tightens cannot be decided mechanically, and a rule that fires on correct code stalls every agent at once. To revisit for the declarative rungs (2 and 3) once the base gate has a track record.
- **One shared `LEARNINGS.md`.** Rejected. Parallel agents appending to one file conflict, and a long file is read by nobody.
- **A stop hook that asks for learnings at every stop.** Rejected. It costs every session a turn, including sessions that changed nothing. The PR description asks once, at the point where the work is summarised anyway.
- **Labels (`tightens`, `loosens`) set by CI.** Rejected. Workflow tokens are read-only (ADR-0009, `docs/operations/github-setup.md`). The check status and the job summary carry the same information.
- **Keeping learnings in the agent's own memory files.** Rejected. That is local-machine state outside git, invisible to other agents and vendors (the same reasons as ADR-0011).
- **Making the PR-body check required.** Rejected for now. It would block a merge over wording. To revisit if descriptions are left unfilled.

## Evidence

- Review of 2026-11-25: ten planted violations of documented rules passed `just check-full` (ADR-0018 lists them). None of those gaps had been written down anywhere before the review found them.
- ADR-0015, Consequences: "A PR that weakens `justfile`, `xtask/` or `clippy.toml` passes its own gate."
- OpenAI's harness team: agents replicate existing patterns, and custom lints whose messages carry the remediation are the cheapest way to change behaviour ([openai.com](https://openai.com/index/harness-engineering/)).
- Claude Code's guidance on where a rule belongs: an instruction is a request, a hook or a check is enforcement (research report, section on hooks).
- 27.67% of simulated merges of agent PRs conflicted ([arXiv 2604.03551](https://arxiv.org/abs/2604.03551)), which is why learnings are one file each.
