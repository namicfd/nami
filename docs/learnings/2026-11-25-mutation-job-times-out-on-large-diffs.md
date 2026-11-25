# The mutation job stops halfway through a large diff

- Status: Open
- Found in: PR #5, the guardrail PR, whose `mutants (diff, advisory)` job ran out of time

## What happened

The job runs `cargo mutants --in-diff` one mutant at a time, with `timeout-minutes: 20`. #5's
diff had 654 mutants. The job reported five survivors, reached about mutant 270, and was
cancelled at 20 minutes. It is `continue-on-error`, so the PR merged with roughly 380 mutants
never tested, and nothing on the PR said that the answer was partial.

A local run with five jobs tested all 654 in 14 minutes. It found no survivors beyond the five,
but only because someone ran it by hand.

ADR-0009 plans to make the job required once it fits the time budget. As it stands, the larger
the PR, the less of it is tested.

## Proposed guardrail

CI, in `.github/workflows/ci.yml`, job `mutants-diff`. One of these, for the maintainer:

- Shard: a matrix of N runners, each `cargo mutants --in-diff pr.diff --shard k/N`. Wall time
  falls about N-fold, at N times the runner minutes.
- Parallelise on one runner: `-j 2` or `-j 3` on the 4-core runner. Cheaper, but builds compete
  for memory and the speed-up is smaller.
- At least, fail loudly: when the job is cancelled, say so in the step summary with the number of
  mutants not tested, so that a partial result is not read as a clean one.
