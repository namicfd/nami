# ADR-0023: Bot keys in CI

- Status: Proposed
- Date: 2026-11-27
- Deciders: maintainer

## Context

Agents act only on what the bot or a code owner wrote (ADR-0021, point 2). A workflow's own token acts as `github-actions[bot]`, whom agents do not trust. A CI job whose output agents must act on therefore acts as a bot, and holds a GitHub App's private key.

- **One job does so today.** `file queue failure` in `.github/workflows/queue.yml` (PR #52) files an issue as `nami-agent[bot]` when a PR fails in the merge queue (ADR-0021, point 6).
- **A second is planned.** The CI-hosted reviewer (#59) posts reviews as the reviewer's app, from a job that runs no PR code.
- **A key is worth more than the job that holds it.** Whoever has it can mint a token with every permission of the app: push branches, open PRs, and file issues that every agent trusts.
- **CI runs untrusted code.** The repository is public. `ci.yml` builds and tests every PR, and in the merge queue it runs the PR's own workflow files.
- **The rules were not policy.** Only the comments in `queue.yml` and the operations runbook (`docs/operations/github-setup.md`, step 8) said how the job keeps its key safe. The second job needs the same rules, written where policy lives.

## Decision

A bot key is the private key of a GitHub App that agents trust: `nami-agent`, and the reviewer's app once #59 lands. Every CI job that holds one follows four rules.

1. **Only `main` can reach the key.** The key is a secret of a GitHub environment that only `main` may deploy from, never a repository or organisation secret. CI has its own key, separate from the keys on the agents' machines, so that it can be revoked alone. The environment has no required reviewers, because the jobs run unattended.
2. **Each job mints a narrow token.** The job mints its token with `xtask/agent-token.sh`, the same script as an agent's session, and sets `NAMI_AGENT_PERMISSIONS` to exactly the permissions it needs. The token expires within an hour, and the job masks it before it uses it. The job's `permissions:` for the workflow token are as narrow.
3. **The secret is in one step's environment.** Exactly one step names the secret, in its `env:`. That step writes the key to a file under `$RUNNER_TEMP` that only the runner's user can read, and does nothing else. The step that mints the token deletes the file before any other program runs, and a last step with `if: always()` deletes it again in case a step in between failed.
4. **No PR code where a key is.** No job that holds a key executes code from a PR, and neither does any step of that job, before or after the key step: steps share the runner, its files and its processes. Code here means everything the job runs: workflow files, actions, xtask, build scripts and tests. The pattern is the one in `queue.yml`:
   - the job is triggered by `workflow_run`, which runs `main`'s version of the workflow file;
   - its `if:` checks the event and the triggering workflow's path, so a workflow that a PR adds under the same name triggers nothing;
   - `actions/checkout` has no `ref:`, so the job checks out `main`;
   - what a PR's run produced, such as its logs, artifacts or conclusion, reaches the job only as data that `main`'s code parses and never executes.

A job that needs both a PR's code and a bot's identity is split in two. One job runs the PR's code without a key and writes data. The other holds the key and acts on that data.

## Enforcement

What checks these rules today:

- **GitHub's deployment branch policy** refuses the environment's secrets to a job on any ref but `main` (rule 1). It holds only while the environment is set up as in step 8 of the runbook. The setting lives on GitHub, and only the runbook's "Check it" commands, run by hand, verify it.
- **GitHub's `workflow_run` event** runs the workflow file from the default branch, and `actions/checkout` defaults to that branch for it (rule 4). This is GitHub's behaviour, and the job relies on it.
- **CODEOWNERS** on `.github/` and `xtask/` (ADR-0008, ADR-0015): every change to a workflow, to the setup action or to `xtask/agent-token.sh` waits for the code owner. Today this is the only check of rules 2 to 4: a human reads the job.
- **`xtask/agent-token.sh`** refuses a `NAMI_AGENT_PERMISSIONS` that is not a JSON object, and GitHub refuses a token with a permission that the app does not have (rule 2).
- **The key step in `queue.yml`** fails with an error that points to step 8 of the runbook when the environment has no key.

No mechanism:

- that a job that names a secret also names an environment (rule 1), sets `NAMI_AGENT_PERMISSIONS` (rule 2), names the secret in exactly one step's `env:` and deletes the file (rule 3), runs only on `workflow_run`, on `push` to `main` or on `schedule`, and checks out no PR ref (rule 4). A tidy rule on `.github/workflows/` can see all of this in text; #63 proposes it;
- that the environment's settings on GitHub still match step 8 between manual checks;
- whether a job executes PR code in a way that the text does not show, for example a tool that runs a script from a checked-out PR. Review is the backstop.

## Consequences

- A job that acts as a bot needs three more steps than one with the workflow token, and its design waits for the code owner.
- A leaked token is narrowed and dies within an hour. A leaked CI key is revoked without touching the agents' machines.
- The CI-hosted reviewer (#59) has the shape of the split above: a review job that reads the PR and holds no app key, and a posting job that holds the key and runs no PR code.
- The protection rests on two things that live on GitHub, the environment's branch policy and `workflow_run`'s behaviour. A setting changed by hand is invisible until someone runs the runbook's checks.

## Alternatives considered

- **Repository or organisation secrets.** Rejected. A workflow on any branch of the repository can read them, and the bot pushes its branches here. Only forks are kept out.
- **`pull_request_target`.** Rejected. It runs `main`'s workflow file with secrets, but in the PR's context, and one checkout of the PR's head then runs the PR's code with the key. `workflow_run` keeps the PR's run and the key in different jobs.
- **The workflow's own token.** Rejected. It acts as `github-actions[bot]`, and trusting that account would trust any workflow that a PR adds (ADR-0021, point 2).
- **The key in the whole job's environment.** Rejected. Every process of every step, including `cargo` and the build scripts it runs, could read it.
- **`actions/create-github-app-token`.** Not chosen. It hands the key to a third-party action's process, and it is one more pinned dependency. `xtask/agent-token.sh` is already reviewed, and agents and CI then mint their tokens the same way.
- **Required reviewers on the environment.** Rejected. The jobs run unattended, after every failed merge-queue run.

## Evidence

- `.github/workflows/queue.yml`, job `queue-failure`, and `docs/operations/github-setup.md`, step 8: the first job that follows these rules.
- Issue #59: the CI-hosted reviewer, the second.
- Environment secrets and deployment branches: [GitHub docs, managing environments for deployment](https://docs.github.com/en/actions/managing-workflow-runs-and-deployments/managing-deployments/managing-environments-for-deployment).
- `workflow_run` runs the default branch's workflow file: [GitHub docs, events that trigger workflows](https://docs.github.com/en/actions/writing-workflows/choosing-when-your-workflow-runs/events-that-trigger-workflows#workflow_run).
- Why `pull_request_target` with a PR checkout leaks secrets: [GitHub Security Lab, preventing pwn requests](https://securitylab.github.com/resources/github-actions-preventing-pwn-requests/).
- Installation tokens expire after an hour and can be narrowed: [GitHub docs, create an installation access token for an app](https://docs.github.com/en/rest/apps/apps#create-an-installation-access-token-for-an-app).
