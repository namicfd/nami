# Probe: does the reviewer app's approval satisfy `main`'s ruleset?

This file exists only for a pull request that is never merged (issue #59). The PR touches only
an unowned path and has no auto-merge. The maintainer approves its head as `nami-reviewer[bot]`
and reads the merge state, to settle two questions that GitHub's documentation leaves open:

1. Does an approving review by a GitHub App count toward the ruleset's `pull_request` rule,
   and with which installation permissions?
2. Does it satisfy "Require approval of the most recent reviewable push" when another app,
   `nami-agent[bot]`, pushed last?

A third check follows: a push by `nami-agent[bot]` after the approval must dismiss it. The PR is
then closed, and this file is never on `main`.
