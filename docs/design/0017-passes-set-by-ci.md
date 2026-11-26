# ADR-0017: CI sets `passes` through the bot's PR

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends: ADR-0011 (point 2, "Only CI may set `passes`")

## Context

ADR-0011 says only CI sets a task's `passes` flag, once the task's acceptance tests pass on `main`. Until now no job did. The first version of this ADR had the maintainer set it by hand, in a PR merged with the admin bypass, so every plan chain waited for the maintainer one step at a time (#62). The `guardrails` plan is a chain of twelve tasks; its first task merged in #34, and the second stays blocked until the first is marked.

The job needs two decisions:

- **How acceptance names map to test runs.** A name in `tasks.json` has to select exactly one test, and a test that does not exist must not count as passing.
- **How CI changes `main`.** The `main` ruleset (ADR-0009) blocks direct pushes, and nothing but the admin role may bypass it. A workflow's own token pushes and opens PRs as `github-actions[bot]`, whose PRs start no workflows, and which agents do not trust (ADR-0021, point 2).

Neither the agents nor their bot may set `passes`: an agent that marks its own task done skips the check that it is.

## Decision

1. **A post-merge workflow sets `passes`.** `.github/workflows/passes.yml` runs on every push to `main` and on manual dispatch. It has two jobs, split as ADR-0023 asks:
   - **`acceptance`** holds no secret and may use caches. For each active task that does not pass yet and whose dependencies pass, it runs the task's acceptance tests, `cargo xtask task passes`. It uploads a file with the IDs of the tasks whose tests all passed, as plain JSON: `{"passing": [...]}`.
   - **`open passes PR`** holds the key of the `nami-ci` GitHub App, in the environment `nami-ci` that only `main` may use. It restores no cache and runs no test. It builds xtask from a clean checkout of the same commit before the key exists, then mints a token narrowed to `{"contents":"write","pull_requests":"write"}`, and deletes the key file before xtask starts. `cargo xtask task passes --open-pr` reads the file as data, and keeps the tasks that may be set: active, not passing, every dependency passing, and a merged PR from `task/<id>`. It sets `passes` to `true` on exactly those, force-pushes the commit as the fixed branch `bot/passes`, opens that branch's PR or updates the open one, and turns on auto-merge. When no task is left, it closes the open PR.
2. **Acceptance names are exact test names.** A name is `<crate>::<module path>::<test>` for a test in a library or binary, and `<crate>::<test target>::<module path>::<test>` for one under `tests/`. `<crate>` is the package name with `-` written `_`. Each name must match one test that `cargo nextest list` reports and does not ignore, in the root workspace or in `lints/`. That test runs alone, through the filter `binary_id(=<binary>) & test(=<name>)`, with `--no-tests=fail`. A name that matches no test, or only an ignored one, fails the task: zero tests run is never a pass.
3. **A merged PR is required** as well as passing tests. Tests can pass before the work is done: two tasks may share a test, and a later task's test may already exist.
4. **`cargo xtask task check-pr` lets exactly that PR through.** A change that sets `passes` passes the `gate` only when all of these hold:
   - it is a pull request, or a merge-queue entry, whose head branch is `bot/passes` in this repository;
   - its author is `nami-ci[bot]`;
   - its diff changes nothing but `"passes": false` lines to `"passes": true`, in active plans' `tasks.json` files;
   - a PR from each flipped task's `task/<id>` has merged into `main`;
   - each flipped task's acceptance tests ran and passed in this PR's own CI run. `check-pr` runs them itself, after the other conditions hold.

   Everything else that sets `passes` stays rejected, whoever opened it. In the merge queue, each entry is judged on its own squashed commit, so a flip ahead of a PR in the group is not taken for that PR's.
5. **The admin bypass is only for exceptions:** a flip that the workflow cannot make, such as a task whose PR came from another branch. Such a PR changes only `tasks.json` files, because the bypass also skips every other required check.
6. **Agents never set `passes`.** They act as `nami-agent[bot]`, so `check-pr` rejects their PRs that do, and they have no bypass.
7. **This ADR is interim.** It records the mechanism until a human accepts it or replaces it, for example with a CI identity that may push to `main`.

## Enforcement

- `check-pr` in the `gate` job, a required check, with the conditions of point 4. `xtask/tests/task_passes.rs` tests each condition both ways.
- `passing_from_json` refuses a file that is anything but distinct task IDs under the one key `passing`, and `--open-pr` checks again that each ID may be set.
- The environment's deployment branch policy keeps the key on `main` (ADR-0023, rule 1). The token's permissions are named in the workflow (rule 2).
- CODEOWNERS on `.github/` and `xtask/` (ADR-0015): changes to the workflow or to `check-pr` wait for the code owner.
- No mechanism checks the workflow's shape: that the key step is the only one with the secret, or that `open passes PR` restores no cache. Review does, until the workflow rule proposed in #63 exists.

## Consequences

- A task is marked done within one post-merge run of its merge, plus one trip through the merge queue. Its dependants then become ready with no human step.
- The acceptance tests run three times: on `main`, in the bot's PR, and in the merge queue. They are named tests, so the cost is small.
- The bot's PR updates on every push to `main` while it is open, which takes it out of the merge queue and puts it back. At phase 1's merge rate this is cheap.
- Anyone who can push to `bot/passes`, agents included, can change the open PR. `check-pr` judges the PR's content, not who pushed it, so such a push can only flip a task that is done.
- Nothing is left for the maintainer to set up: the `nami-ci` app, its key in the `nami-ci` environment and the variable `NAMI_CI_CLIENT_ID` exist.

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
