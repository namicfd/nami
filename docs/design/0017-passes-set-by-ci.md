# ADR-0017: CI sets `passes` through the bot's PR

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends: ADR-0011 (point 2, "Only CI may set `passes`")

## Context

ADR-0011 says only CI sets a task's `passes` flag, once the task's acceptance tests pass on `main`. Until now no job did. The first version of this ADR had the maintainer set it by hand, in a PR merged with the admin bypass, so every plan chain waited for the maintainer one step at a time (#62). The `guardrails` plan is a chain of twelve tasks; its first task merged in #34, and the second stays blocked until the first is marked.

The job needs two decisions:

- **How acceptance names map to test runs.** A name in `tasks.json` has to select the tests it names and nothing else, and a test that does not exist must not count as passing.
- **How CI changes `main`.** The `main` ruleset (ADR-0009) blocks direct pushes, and nothing but the admin role may bypass it. A workflow's own token pushes and opens PRs as `github-actions[bot]`, whose PRs start no workflows, and which agents do not trust (ADR-0021, point 2).

Neither the agents nor their bot may set `passes`: an agent that marks its own task done skips the check that it is.

**Where the safety lies.** `cargo xtask task check-pr` in the required `gate` job, together with tidy's plan rules (`task-graph`: no task passes before its dependencies), carries every safety property of `passes` on `main`. The workflow below only proposes a change. If it is wrong, buggy or compromised, the worst it can do is open a PR that `check-pr` then judges like any other.

## Decision

1. **A post-merge workflow sets `passes`.** `.github/workflows/passes.yml` runs on every push to `main` and on manual dispatch. It has two jobs, split as ADR-0023 asks:
   - **`acceptance`** holds no secret and may use caches. For each active task that does not pass yet and whose dependencies pass, it runs the task's acceptance tests, `cargo xtask task passes`. It uploads a file with the IDs of the tasks whose tests all passed, as plain JSON: `{"passing": [...]}`.
   - **`open passes PR`** holds the key of the `nami-ci` GitHub App, in the environment `nami-ci` that only `main` may use. It restores no cache and runs no test. It builds xtask from a clean checkout of the same commit before the key exists, then mints a token narrowed to `{"contents":"write","pull_requests":"write"}`, and deletes the key file before xtask starts. That token is used only for the push and the PR writes. Every read, such as whether a task's PR merged, `main`'s rules and history, or the checks on the branch's tip, uses the workflow's own read-only token, and gh's environment for reads has the app's token removed (ADR-0023, rule 2). `cargo xtask task passes --open-pr` reads the file as data, and keeps the tasks that may be set: active, not passing, every dependency passing, and a merged PR from `task/<id>`. It sets `passes` to `true` on exactly those, and builds the commit that the fixed branch `bot/passes` should hold: those flips on top of the current `main`.
     - **Push only on a difference.** It force-pushes that commit only when the branch does not already hold the same change. The tip holds it when it has one parent, that parent is on `main`, and the tip's own change, its diff against that parent, replays cleanly onto the current `main` and gives the new commit's tree. The replay is `git merge-tree --write-tree --merge-base=<the tip's parent> <main> <tip>`. It names its merge base, so it needs only the tip and its parent, not `main`'s history. Whether the parent is on `main` comes from GitHub's compare API, read with the read token, because the depth-1 checkout has no history to ask. A tip on an older `main` therefore stays, however far `main` moved, and the PR keeps its place in the merge queue (#62). The branch is rebuilt on the current `main` when the flips differ, when the tip carries any other change, when its change conflicts with `main`, when its parent is not on `main`, or when it has no parent or several. In `xtask/tests/task_passes.rs`, `open_pr_keeps_a_stale_tip_until_it_conflicts_or_its_flips_differ` tests an older `main`, a conflict and other flips; `open_pr_pushes_only_on_drift_and_always_reconciles` a foreign file; and `open_pr_rebuilds_a_tip_whose_parent_is_not_on_main` a parent off `main`. A root or merge tip is tested only by the unit test `only_a_one_parent_commit_has_its_own_change` in `xtask/src/task/passes.rs`.
     - **Push again when the required checks failed.** A held tip is pushed again when a check that `main`'s rules require (`gate` and `lints (UI tests)` today) completed on it without passing: failure, cancellation, timeout, `action_required`, `stale` or `startup_failure`. Nothing else reruns them: `ci.yml` starts on a push or on opening the PR, and the reconcile writes start nothing, so a `gate` that failed once, from an API outage, a flaky test or a runner timeout, would keep the PR red until the flips change. Only the newest run of each check counts, as for merging. Pending checks, passed checks, or no open PR push nothing. The required set is read from `main`'s rules, not every check on the head: advisory checks such as `mutants (diff, advisory)` may fail without blocking the merge, and a push for each would take the PR out of the queue on every run until it starves. The pushed commit is new, so GitHub runs the checks again: it sits on the current `main`, and when that is the tip's own parent, its committer time is one second after the tip's. A failed read of the checks fails the run before it changes anything, like every other read here, and the next run asks again. `open_pr_pushes_a_held_tip_again_only_when_its_checks_failed` tests each case.
     - **Reconcile every run.** Whether it pushed or not, it opens the PR if there is none, and reads the PR with the read token. It sets the title and description to the current flips when they differ, and turns auto-merge back on when it is off. A run that failed or stopped after its push is repaired by the next one.
     - When no task is left, it closes the open PR.
     - An open PR from `bot/passes` that `nami-ci[bot]` did not open is never adopted. The job fails, names the PR for a human to close, and changes nothing.
     - **Concurrency.** The workflow is one concurrency group without cancel-in-progress. Runs go one at a time and in order, and none is stopped halfway. GitHub keeps only the newest waiting run, so an older run never runs after a newer one, unless someone re-runs an old run, which uses its original commit (ADR-0023, `nami-agent`'s Actions write). The next push to `main` corrects that.
2. **Acceptance names are exact test names.** A name is `<crate>::<module path>::<test>` for a test in a library or binary, and `<crate>::<test target>::<module path>::<test>` for one under `tests/`. `<crate>` is the package name with `-` written `_`. Each name must match at least one test that `cargo nextest list` reports and does not ignore, in the root workspace or in `lints/`. It matches more than one only when two targets of a crate share a prefix, such as a library and a binary; then all of them run. A task's tests run together, in one `cargo nextest run` per workspace, through a filter that names each matched test by binary and full name, `binary_id(=<binary>) & test(=<name>)`, joined with `|`, and with `--no-tests=fail`. Nothing else runs. A name that matches no test, or only an ignored one, fails the task: zero tests run is never a pass.
3. **A merged PR is required** as well as passing tests. Tests can pass before the work is done: two tasks may share a test, and a later task's test may already exist. This condition is weak: any agent can open and merge a trivial PR from `task/<id>`. It keeps an honest mistake out, not a determined one. The acceptance tests carry the weight.
4. **`cargo xtask task check-pr` lets exactly that PR through.** A change that sets `passes` passes the `gate` only when all of these hold:
   - it is a pull request, or a merge-queue entry, whose head branch is `bot/passes` in this repository;
   - its author is `nami-ci[bot]`;
   - its diff changes nothing but `"passes": false` lines to `"passes": true`, in active plans' `tasks.json` files;
   - a PR from each flipped task's `task/<id>` has merged into `main`;
   - each flipped task's acceptance tests ran and passed in this PR's own CI run. `check-pr` runs them itself, after the other conditions hold.

   Everything else that sets `passes` stays rejected, whoever opened it. A PR from `bot/passes`, or one that `nami-ci[bot]` opened, that sets no `passes` is rejected too: that branch and that bot exist only to set it. On `pull_request` events, `ci.yml` passes the PR's head ref, head repository and author from the event, so that check needs no network. GitHub is asked only for a merge-queue entry, and for a PR that sets `passes`. In the merge queue, each entry is judged on its own squashed commit, so a flip ahead of a PR in the group is not taken for that PR's.
5. **The admin bypass is only for exceptions:** a flip that the workflow cannot make, such as a task whose PR came from another branch. Such a PR changes only `tasks.json` files, because the bypass also skips every other required check.
6. **Agents never set `passes`.** They act as `nami-agent[bot]`, so `check-pr` rejects their PRs that do, and they have no bypass.
7. **This ADR is interim.** It records the mechanism until a human accepts it or replaces it, for example with a CI identity that may push to `main`.

## Enforcement

- `check-pr` in the `gate` job, a required check, with the conditions of point 4. `xtask/tests/task_passes.rs` tests each condition both ways.
- `passing_from_json` refuses a file that is anything but distinct task IDs under the one key `passing`, and `--open-pr` checks again that each ID may be set.
- The environment's deployment branch policy keeps the key on `main` (ADR-0023, rule 1). The token's permissions are named in the workflow (rule 2).
- CODEOWNERS on `.github/` and `xtask/` (ADR-0015): changes to the workflow or to `check-pr` wait for the code owner.
- `xtask/src/task/pulls.rs` tests that every read reaches gh as the read token, with the app's token removed, and every write as the app's token; and that a write without that token fails instead of falling back. It also tests how GitHub's rules, check runs and comparisons are read: `required_checks_come_from_main_s_rules`, `checks_fail_pass_or_wait_on_their_newest_run` and `ancestry_comes_from_the_comparison`.
- No mechanism checks the workflow's shape: that the key step is the only one with the secret, or that `open passes PR` restores no cache. Review does, until the workflow rule proposed in #63 exists.

Known gaps, with no mechanism yet:

- **Vacuous tests.** A test that asserts nothing passes, so a task whose acceptance test is vacuous passes too. Review of the plan, and mutation testing of the PR that adds the test, are the only defence.
- **Default nextest filters.** A `default-filter` in `.config/nextest.toml` would combine with the filter above. A test that `nextest list` reports but the default filter leaves out would then be named and not run. If all of a task's tests are filtered out, `--no-tests=fail` fails it. If only some are, the task passes on the rest. The repository sets no default filter today.
- **Checks that never report.** A required check with no check run on the tip counts as pending, so it never makes a push. That covers a CI run that GitHub never started, and a required check that is reported as a commit status instead of a check run. Both required checks are Actions jobs today. With no run, there is nothing to re-run: a human or an agent closes and reopens the PR, which starts CI again, or closes it so that the next run opens it again.
- **Healing waits for the next run.** A failed check is pushed again by the next `passes` run, which starts on the next push to `main`. Until something merges, the PR stays red.

## Consequences

- A task is marked done within one post-merge run of its merge, plus one trip through the merge queue. Its dependants then become ready with no human step.
- The acceptance tests run three times: on `main`, in the bot's PR, and in the merge queue. They are named tests, so the cost is small.
- The bot's PR is rebuilt only when its flips change, its change no longer merges cleanly into `main`, or its required checks failed. A rebuild takes it out of the merge queue until its checks pass again; a PR whose checks failed is not in the queue anyway. Other merges to `main` leave it in place: the run only edits its description, which names the `main` that the tests passed on. If the queue drops it for another reason, the next run turns auto-merge back on.
- A merge-queue entry, and a PR that sets `passes`, need GitHub's API in the `gate`. When GitHub cannot answer, the check fails. Other PRs need no network for it.
- Anyone who can push to `bot/passes`, agents included, can change the open PR. `check-pr` judges the PR's content, not who pushed it, so such a push can only flip a task that is done. The next run rebuilds the branch unless the push is exactly the wanted flips on a commit of `main`, which is what the run would push itself. A ruleset that lets only the `nami-ci` app update `bot/passes` would close this; the runbook asks the maintainer for it.
- The `nami-ci` app, its key in the `nami-ci` environment and the variable `NAMI_CI_CLIENT_ID` exist. One optional maintainer step remains: the `bot/passes` ruleset in step 8 of `docs/operations/github-setup.md`, so that only the `nami-ci` app can update the branch. Without it, safety still holds, because `check-pr` judges the PR's content. But liveness depends on no one rewriting the branch: each foreign push is undone only by the next run, and a push that lands between runs keeps the PR out of the queue until then.

Risks, not yet seen:

- **A steady stream of newly done tasks.** A merge to `main` alone no longer rebuilds `bot/passes`. But a run that finds another task done changes the flips, so it rebuilds the branch on the new `main` and force-pushes it, and the PR re-enters the merge queue only after its checks pass again. So does a merge that edits a line next to a flipped `passes` line, because the old change then conflicts. If tasks finish faster than the PR's checks run, the PR could be requeued again and again and land late.
- **A check that always fails.** If the bot's PR fails a required check for a reason that a rerun does not fix, such as a flipped task's test that passes on `main` but fails in the PR, every run pushes it again: one CI run of the PR per merge to `main`, as every merge cost before the branch was kept. The red `gate` names the cause for a human to fix.
- **GitHub's `workflows` permission on the push.** The app's token has no `workflows` permission. GitHub refuses a push that creates or changes files under `.github/workflows/` without it. The rebuilt commit's only new content is the flip of `tasks.json` lines; its parent, `main`, is already on GitHub. So the check should not apply. This is unverified: if GitHub ever applies it to the whole branch, for example when the branch is first created on a `main` whose workflows changed, the push is refused and the job fails loudly.

## Alternatives considered

- **The maintainer sets `passes` by hand.** The first version of this ADR. Replaced: every step of every chain waits for a human.
- **The workflow token.** Rejected. Its PRs start no workflows, so the required checks never run, and agents do not trust `github-actions[bot]`.
- **`nami-agent`'s key in CI.** Rejected. `check-pr` must tell CI's PR from an agent's, so they need different identities.
- **A CI identity that pushes to `main`.** Rejected for now. It needs a bypass of the ruleset, and a commit on `main` that no gate checked.
- **One job that runs the tests and holds the key.** Rejected by ADR-0023: steps share the runner, and the tests' build could read the key.
- **Matching acceptance names as nextest substrings.** Rejected. `test(foo)` also runs `foo_bar`, and a name that matches nothing would pass with zero tests.

## Evidence

- ADR-0011, ADR-0009 and ADR-0023.
- Issue #62: nothing set `passes`, so every plan chain waited for the maintainer.
- The first review of PR #2, which found that nothing could set `passes`.
- nextest filtersets: [nexte.st, filterset DSL](https://nexte.st/docs/filtersets/reference/).
