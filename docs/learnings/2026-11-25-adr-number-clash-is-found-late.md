# An ADR number clash is found only after one of the two PRs has merged

- Status: Open
- Found in: the guardrail review of 2026-11-25, while merging `main` into the guardrail branch

## What happened

Tidy rule `adr-number` rejects two ADRs with one number, but only once both are in one tree.
While two pull requests are open side by side, each is green. The second one learns of the clash
when it merges `main` or enters the merge queue. Renumbering then touches every reference: 16
files in the guardrail PR, twice in one day.

Nothing tells a session which numbers the other open pull requests have taken. Reading
`git branch -a` helps only if the other branches are fetched and someone thinks of looking.

## Proposed guardrail

One of these, for the maintainer to choose:

- CI, in `.github/workflows/guardrails.yml`: a job that lists the ADR files added by the other
  open pull requests and fails when this one adds the same number. The pull request with the
  lower number keeps the ADR number.
- A claim, like `cargo xtask task claim`: `cargo xtask adr new <slug>` takes the next free
  number through a lock on the remote and creates the file from the template.
