# Learnings

What you found out during a task that the next agent should not have to find out again. Each
learning is a guardrail that does not exist yet (ADR-0019).

Write one when:

- a bug, or a wrong turn that cost you time, would have been caught by a check;
- a reviewer told you something that no check enforces;
- a rule's error message did not tell you how to fix the problem;
- a rule fired on correct code;
- you worked out a pattern that the next kernel, boundary condition or test will need.

Do not write one for unfinished work in your own task. That is a task
(`docs/exec-plans/`), not a learning.

## How

- **One file per learning**, named `YYYY-MM-DD-<slug>.md`. Separate files never conflict when
  agents work in parallel.
- **Keep it to a screen.** Show the code shape, not the story of your session.
- **You may always add a file here**, whatever your task's `owner_paths` say. The directory is
  not a protected path.
- **List the file** under `Guardrail gaps` in the PR description.
- **If you can codify it yourself, do.** A guardrail in the same PR is better than a note. See
  [`docs/playbooks/add-a-guardrail.md`](../playbooks/add-a-guardrail.md).

Tidy checks the file name, the status line and the two headings (`learning-format`).

```markdown
# Open ranges pass the bounded-loop rule

- Status: Open
- Found in: task euler1d-hllc

## What happened

`for step in 0_u32.. { ... }` with a `break` passed `just check`. Tidy's `unbounded-loop` rule
matched only `loop` and `while`.

## Proposed guardrail

Tidy, in `unbounded-loop`: report a `for` whose iterable is a range without an end, unless a
`take` or `zip` bounds it.
```

## Status

| Status line | Meaning | Who sets it |
|---|---|---|
| `- Status: Open` | Nobody has acted on it yet. | The author. |
| `- Status: Codified in <rule id, lint or ADR>` | A check now enforces it. | The PR that adds the check. |
| `- Status: Rejected` | It will not become a check. The line below says why. | A human. |

Open learnings are the backlog for guardrail work. A session that has no assigned task can pick
one, turn it into a check and set the status.
