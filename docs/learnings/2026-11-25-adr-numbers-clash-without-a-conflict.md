# Two branches can take the same ADR number without a merge conflict

- Status: Codified in `adr-number`
- Found in: the guardrail review of 2026-11-25, while merging `main` into the guardrail branch

## What happened

The guardrail branch added `0017-rule-registry-and-checked-tags.md`. Meanwhile `main` gained
`0017-interim-passes-by-maintainer.md`. The file names differ, so git merged both without a
conflict on the files. Only the two index rows in `docs/design/README.md` clashed, and only
because they were added at the same place. `adr-index` passed: both files were listed.

The same branch had already been renumbered once that day, from 0016, after another branch took
that number.

## Proposed guardrail

Tidy, as a source rule `adr-number`: report an ADR whose first four digits are also the first
four digits of another file in `docs/design/`, and an ADR whose title is not
`# ADR-<number of the file name>: ...`. The second part catches a renumbering that renamed the
file and forgot the title.

The rule sees a clash once both files are in one tree: when a branch merges `main`, in CI on the
pull request's merge commit, and in the merge queue at the latest.
