# ADR-0023: Bot keys in CI

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends: ADR-0021, point 2

## Context

Agents act only on what the bot or a code owner wrote (ADR-0021, point 2). A workflow's own token acts as `github-actions[bot]`, whom agents do not trust. A CI job whose output agents must act on therefore acts as a GitHub App, and holds that app's private key.

- **One job does so today.** `file queue failure` in `.github/workflows/queue.yml` (PR #52) files an issue when a PR fails in the merge queue (ADR-0021, point 6).
- **Two more are planned.** The CI-hosted reviewer (#59) posts reviews from a job that runs no PR code. The `bot/passes` PR (issue #62, PR #70) sets `passes` in `tasks.json` from CI.
- **A key is worth more than the job that holds it.** Whoever has it can mint a token with every permission that the app's installation has. With `nami-agent`'s key, that is pushing branches, opening PRs and filing issues that every agent trusts.
- **CI runs untrusted code.** The repository is public. `ci.yml` builds and tests every PR, and in the merge queue it runs the PR's own workflow files. `main`'s caches are written by jobs that run the build scripts and tests of code that only the AI review read (ADR-0008).
- **The rules were not policy.** Only the comments in `queue.yml` and the operations runbook (`docs/operations/github-setup.md`, step 8) said how the job keeps its key safe, and the job used the agents' own app.

## Decision

### One GitHub App per role

| App | Bot | Role | Where its key lives |
|---|---|---|---|
| `nami-agent` | `nami-agent[bot]` | Authoring: agents push, open PRs, comment and file issues (ADR-0008). | The agents' machines. Never CI. |
| `nami-reviewer` | `nami-reviewer[bot]` | The CI-hosted AI review (#59). | The environment `nami-reviewer`, once #59 lands. |
| `nami-ci` | `nami-ci[bot]` | CI jobs: the queue-failure issues, and the `bot/passes` PR once PR #70 (issue #62) lands. | The environment `nami-ci`. |

- **Each app's installation permissions are the least its role needs,** and each app is installed on `namicfd/nami` only. `nami-ci` has issues write for the queue-failure issues, and metadata read. Its contents and pull requests write are for the `bot/passes` PR of PR #70, granted before #70 lands; until then no job asks for them, since the queue-failure job narrows its token to `issues: write`. It has no Actions permission: its jobs read runs and logs with the workflow's own token.
- **A role never borrows another app's key.** A key leaked from CI is revoked without touching the agents' machines, and a key on an agent's machine never reaches CI. `xtask/agent-token.sh` selects the app with `NAMI_AGENT_APP` and never falls back from one app's key to another's.
- **Agents trust `nami-ci[bot]` only for the shapes it produces.** This amends ADR-0021, point 2:
  - an issue whose title is exactly `PR #<n> failed in the merge queue (run <id>)`, as `cargo xtask queue failed` writes it. Any issue with such a title is accepted from `nami-ci[bot]`, and is then handled as ADR-0021, point 6, says;
  - the `bot/passes` PR, only in the shape that PR #70 (issue #62) defines: its branch is `bot/passes` in this repository, its author is `nami-ci[bot]`, it changes only `passes` lines in `tasks.json` and each of them from `false` to `true`, the task's `task/<id>` PR has merged, and `cargo xtask task check-pr` re-runs that task's tests. #70 adds the check of this shape; until it lands, no `bot/passes` PR is trusted;
  - nothing else: not its other issues, not its comments, not its label changes.

### Rules for every CI job that holds an app's key

1. **Only `main` can reach the key.** The key is a secret of a GitHub environment named after the app, which only `main` may deploy from. It is never a repository or organisation secret. The environment has no required reviewers, because the jobs run unattended.
2. **Each job narrows its tokens.** The job mints the app's token with `xtask/agent-token.sh`, and sets `NAMI_AGENT_PERMISSIONS` to exactly what the app's identity is needed for. What the workflow's own token can do, such as reading runs, logs and PRs, the job does with that token, under a `permissions:` block that is as narrow. A tool that uses both tokens keeps them apart: no read takes the app's token, and no write takes the workflow's. The app's token expires within an hour, and the job masks it before it uses it.
3. **The secret is in one step's environment.** Exactly one step names the secret, in its `env:`. That step writes the key to a file under `$RUNNER_TEMP` that only the runner's user can read, and does nothing else. The step that mints the token deletes the file before any other program runs, and a last step with `if: always()` deletes it again in case a step in between failed.
4. **No PR code in, or after, a job that holds a key.** No job that holds a key executes code from a PR, and neither does any step of that job, before or after the key step: steps share the runner, its files and its processes. Code here means everything the job runs: workflow files, actions, xtask, build scripts and tests. The pattern is the one in `queue.yml`:
   - the job is triggered by `workflow_run`, which runs `main`'s version of the workflow file;
   - its `if:` checks the event and the triggering workflow's path, so a workflow that a PR adds under the same name triggers nothing;
   - `actions/checkout` has no `ref:`, so the job checks out `main`;
   - what a PR's run produced, such as its logs, artifacts or conclusion, reaches the job only as data that `main`'s code parses and never executes.
5. **No cache in a job that holds a key.** Such a job restores no cache of any kind: not `actions/cache`, not a setup action's own caches, not the cargo registry, `target/` or a tool cache. A restored cache is code that another job wrote. The job builds what it runs from a clean checkout, with the toolchain that `rust-toolchain.toml` pins and the crates that `Cargo.lock` pins, and installs any tool at a pinned version.

A job that needs both a PR's code and a bot's identity is split in two. One job runs the PR's code without a key and writes data. The other holds the key and acts on that data.

## Enforcement

What checks these rules today:

- **GitHub's deployment branch policy** on the environment refuses its secrets to a job on any ref but `main` (rule 1). For `nami-ci`, the maintainer created the environment, limited it to `main` and added the secret on 2026-11-26 (runbook, step 8). The setting lives on GitHub, where the bot cannot read it. Only the runbook's "Check it" commands, run by hand, verify it.
- **GitHub's `workflow_run` event** runs the workflow file from the default branch, and `actions/checkout` defaults to that branch for it (rule 4). This is GitHub's behaviour, and the job relies on it.
- **`xtask/agent-token.sh`** (rule 2, and the app table):
  - it refuses an unset, empty or `{}` `NAMI_AGENT_PERMISSIONS` when it runs in CI (`CI` or `GITHUB_ACTIONS` is `true`) or for the CI app, and anywhere a value that is not a JSON object;
  - it refuses `jwt` in CI and for the CI app, because a JWT mints tokens with every permission of the app. The only way to a token there is `token`, which narrows it;
  - it refuses to act as the agents' app in CI, so a job cannot use `nami-agent`'s key even if one reached CI;
  - it never signs for `nami-ci` with the agents' key or client ID.

  GitHub refuses a permission that the app's installation does not have. This checks that a job narrows its token at all, not that it narrows it to what the job needs. It also checks only what runs through the script: a job that signs a JWT with the key by other means is caught by review alone.
- **`cargo xtask queue failed`** does every read, of the open issues too, with `GH_TOKEN`, and only creates the issue with `NAMI_FILING_TOKEN` when that is set. It removes `NAMI_FILING_TOKEN` from the environment of every read (rule 2). Its tests check both directions. In `queue.yml`, the workflow's token has `actions`, `contents`, `issues` and `pull-requests` read.
- **`xtask/src/issue/trust.rs`** trusts `nami-ci[bot]` only for an issue whose title parses as a queue failure. Its tests check both directions: every such title is accepted, and any other title, comment or label change by the CI app is not.
- **CODEOWNERS** on `.github/` and `xtask/` (ADR-0008, ADR-0015): every change to a workflow, to the setup action, to `xtask/agent-token.sh` or to the trust rule waits for the code owner. This is the only check that a job narrows its tokens to what it needs (rule 2), keeps the secret in one step (rule 3), runs no PR code (rule 4) and restores no cache (rule 5): a human reads the job.
- **The key step in `queue.yml`** fails with an error that points to step 8 of the runbook when the environment has no key.

Rule 5 does not rely on GitHub's cache scoping. A `workflow_run` job runs in `main`'s cache scope, which keeps forks' caches out, but `main`'s own caches are written after running code that no human may have read.

No mechanism:

- that a job that names a secret also names an environment (rule 1), sets `NAMI_AGENT_PERMISSIONS` (rule 2), names the secret in exactly one step's `env:` and deletes the file (rule 3), runs only on `workflow_run`, on `push` to `main` or on `schedule`, and checks out no PR ref (rule 4), and restores no cache, directly or through the setup action (rule 5); nor that a job that mints a `nami-ci` token uses it only through the commands that produce the shapes agents trust. A tidy rule on `.github/workflows/` can see all of this in the text; #63 proposes it;
- that an environment exists before a job names it. **A job that names a missing environment makes GitHub create it, with no branch rule,** so every branch may then use its secrets. A typo in `environment:`, or a new app's job that lands before its environment is set up, creates such an environment, and a secret added to it later is open to every branch. Nothing checks for this automatically: the runbook's "Check it" commands are the only safeguard;
- that the environment's settings on GitHub still match step 8 between manual checks;
- that a `bot/passes` PR has the shape above, until PR #70 lands with its check;
- whether a job executes PR code in a way that the text does not show, for example a tool that runs a script from a checked-out PR. Review is the backstop.

## Consequences

- A job that acts as an app needs three more steps than one with the workflow token, and its design waits for the code owner.
- A leaked app token is narrowed and dies within an hour. A leaked CI key is revoked without touching the agents' machines, and the agents' key is never in CI to leak.
- Agents trust a third author, but only for two shapes. Code checks the shape of the queue-failure issue, and PR #70 adds the check of the `bot/passes` PR. `nami-ci[bot]` cannot speak for the agents or for a code owner.
- A key job builds xtask from scratch on every run, which costs a few minutes of CI per failed merge-queue run.
- The CI-hosted reviewer (#59) has the shape of the split above: a review job that reads the PR and holds no app key, and a posting job that holds `nami-reviewer`'s key and runs no PR code.
- The protection rests on two things that live on GitHub, the environment's branch policy and `workflow_run`'s behaviour. A setting changed by hand is invisible until someone runs the runbook's checks.

## Alternatives considered

- **One app for agents and CI.** Rejected. This is how the queue-failure job was first written. A CI key would then carry every permission the agents need, and revoking it would also cut off every agent.
- **Repository or organisation secrets.** Rejected. A workflow on any branch of the repository can read them, and the bot pushes its branches here. Only forks are kept out.
- **`pull_request_target`.** Rejected. It runs `main`'s workflow file with secrets, but in the PR's context, and one checkout of the PR's head then runs the PR's code with the key. `workflow_run` keeps the PR's run and the key in different jobs.
- **The workflow's own token for the output.** Rejected. It acts as `github-actions[bot]`, and trusting that account would trust any workflow that a PR adds (ADR-0021, point 2).
- **Trust `nami-ci[bot]` like `nami-agent[bot]`.** Rejected. Any step of any job that holds its key could then file work for agents. Trusting only the shapes it produces limits what a misused token can make agents do.
- **The key in the whole job's environment.** Rejected. Every process of every step, including `cargo` and the build scripts it runs, could read it.
- **Caches that only key jobs write.** Not chosen. It would keep the speed, but every key job would need its own cache keys and a check that no other job writes them. A clean build is simpler to review.
- **`actions/create-github-app-token`.** Not chosen. It hands the key to a third-party action's process, and it is one more pinned dependency. `xtask/agent-token.sh` is already reviewed, and agents and CI then mint their tokens the same way.
- **Required reviewers on the environment.** Rejected. The jobs run unattended, after every failed merge-queue run.

## Evidence

- `.github/workflows/queue.yml`, job `queue-failure`, and `docs/operations/github-setup.md`, step 8: the first job that follows these rules.
- Issue #59, the CI-hosted reviewer, and issue #62 with PR #70, the `bot/passes` PR: the next two.
- Environment secrets and deployment branches, and that a job naming a missing environment creates it: [GitHub docs, managing environments for deployment](https://docs.github.com/en/actions/managing-workflow-runs-and-deployments/managing-deployments/managing-environments-for-deployment).
- `workflow_run` runs the default branch's workflow file: [GitHub docs, events that trigger workflows](https://docs.github.com/en/actions/writing-workflows/choosing-when-your-workflow-runs/events-that-trigger-workflows#workflow_run).
- Why `pull_request_target` with a PR checkout leaks secrets: [GitHub Security Lab, preventing pwn requests](https://securitylab.github.com/resources/github-actions-preventing-pwn-requests/).
- Installation tokens expire after an hour and can be narrowed: [GitHub docs, create an installation access token for an app](https://docs.github.com/en/rest/apps/apps#create-an-installation-access-token-for-an-app).
