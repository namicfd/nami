# ADR-0023: Bot keys in CI

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends: ADR-0021, points 2 and 6

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
| `nami-agent` | `nami-agent[bot]` | Authoring: agents push, open PRs, comment and file issues (ADR-0008), and re-run or dispatch a failed workflow run (Actions write, below). | The agents' machines. Never CI. |
| `nami-reviewer` | `nami-reviewer[bot]` | The CI-hosted AI review (#59, `.github/workflows/review.yml`): it posts the review, resolves its own threads, and approves a PR's head when nothing blocks, which satisfies `main`'s "approve the last push". | The environment `nami-reviewer`. The model job holds no Anthropic key: it signs in through Anthropic's workload identity federation, whose rule accepts only the OIDC token of this repository's environment `claude-review` on `main`. |
| `nami-ci` | `nami-ci[bot]` | CI jobs: the queue-failure issues, and the `bot/passes` PR once PR #70 (issue #62) lands. | The environment `nami-ci`. |

- **Each app's installation permissions are the least its role needs,** and each app is installed on `namicfd/nami` only. `nami-ci` has issues write for the queue-failure issues, and metadata read. It has no Actions permission: its jobs read runs and logs with the workflow's own token. It also holds contents and pull requests write ahead of PR #70, which needs them for the `bot/passes` PR: the maintainer granted them on 2026-11-26, so that #70 needs no second setup. That is more than today's role needs. No job asks for them, since the queue-failure job narrows its token to `issues: write`, but a misused key can mint them. With them, until #70 lands and after, it could push commits to any branch that the ruleset leaves open, including the branches of open agent PRs, and open, edit or close PRs as `nami-ci[bot]`. `main` stays behind the ruleset: the app has no bypass, every change goes through the merge queue, and protected paths need the code owner's review. A push to an open PR shows as a commit by `nami-ci[bot]` in the PR. The alternative, granting them only when #70 lands, is the maintainer's choice.
- **`nami-agent` holds Actions write** (maintainer's decision, 2026-11-28), so that an agent can re-run or dispatch a run that failed for a passing reason without a human. The first such run was the `passes` job, while `nami-ci`'s new permissions were still being granted.
  - **Why it is accepted:** the permission cannot be narrowed to re-runs, and its other powers fail closed for merging.
    - The merge queue's own `merge_group` run decides every merge, and a re-run does not put a dropped PR back in the queue.
    - A disabled required workflow blocks merges rather than letting one through.
    - A deleted ratchet evidence artifact must give no verdict, not a pass. This is a requirement on #57's design, which carries the PR's evidence into the merge group as an artifact, and holds once #57 lands.
  - **What it costs.** Any holder of the agents' key, which is in agents' reach by design (ADR-0032), can also:
    - delete workflow runs, logs and artifacts, cancel runs, and disable workflows. The record of what CI saw is therefore only as safe as the agents' key;
    - **re-run old runs of the jobs that hold `nami-ci`'s key.** A re-run uses the run's original commit, not today's `main`. For a key-holding job, that is an older `main`'s workflow and `xtask`, and so `main`'s code at that time, never a PR's.
      - An older `xtask` with a bug fixed since can therefore act with `nami-ci`'s key again.
      - A re-run of an old `passes` run can rebuild `bot/passes` on an older `main`, against ADR-0017's "an older run never runs after a newer one". GitHub keeps only the newest waiting run of the group, so the re-run can also replace a newer run that was waiting. `bot/passes` then stays on the older `main` until the next push to `main` starts a run. `check-pr` judges the PR's content either way, so this delays `passes` but never sets one wrongly.
      - A dispatch (`workflow_dispatch`) runs today's `main`;
    - **approve workflow runs from fork PRs,** which gets around the fork-approval setting (runbook step 1). A fork's run gets the read-only workflow token and no secrets, so it runs untrusted code on GitHub's runners without access to a key. The approval is what the setting existed to keep human.
  - **Agents' practice** (in AGENTS.md, because it is the only control): re-run only the latest run of a workflow, and only for a failure whose cause is gone. For `passes`, the one key-holding workflow with `workflow_dispatch`, dispatch rather than re-run an old run. Never delete, cancel or disable anything in Actions, and never approve a fork's run.
- **`nami-reviewer` holds pull requests write, contents write and metadata read** (#59). Pull requests write posts its reviews, approves and resolves its threads. Contents write is not used: third-party tests report that GitHub counts an app's approval toward a ruleset only when the app has write access to the contents, and the runbook's experiment (step 11, point 2) checks it before the key is stored. The posting job narrows its token to `pull_requests: write`. A misused key can still mint contents write and push to any branch that the rulesets leave open, open PR branches included; `main` stays behind the ruleset, and a push by the app makes it the last pusher, whose own approval of that head GitHub does not count.
- **A role never borrows another app's key.** A key leaked from CI is revoked without touching the agents' machines, and a key on an agent's machine never reaches CI. `xtask/agent-token.sh` selects the app with `NAMI_AGENT_APP` and never falls back from one app's key to another's.
- **Agents trust `nami-ci[bot]` only for the shapes it produces.** This amends ADR-0021, point 2:
  - an issue whose title is exactly `PR #<n> failed in the merge queue (run <id>)` and whose body is what `cargo xtask queue failed` writes for that title: fixed text, numbers, links into this repository only, and names from the closed vocabulary. That vocabulary is the job and step names of `ci.yml` and `guardrails.yml` on `main`, tidy's rule IDs as the running xtask binary has them (`main`'s in CI, the checkout's on an agent's machine), and the dylint lints' names in the checkout's `lints/`. This also amends ADR-0021, point 6, whose bullet "The issue quotes only checked text" names jobs and steps only by `ci.yml`'s names: the job and step names of `guardrails.yml` on `main` join that vocabulary, because the ratchet runs in `guardrails.yml`, and is a required check once the policy flip lands (ADR-0022): a failed merge-queue run of it then takes the PR out of the queue just as one of `ci.yml` does, and that failure needs an issue and an owner too. With two workflows, point 6's "one issue per failed run" becomes one issue per failed merge group: a failed run whose merge group, the same queue branch and commit, an open issue's run already tested files nothing, since the filing token cannot add it to that issue, and two sessions on two issues would push to one branch. If both runs read the open issues before either files, the two issues name neither each other, and `cargo xtask task` holds each of them while the other is claimed or closed by the PR's description, so that one session at a time works them. An issue that names another, as a later merge group's issue names the earlier ones, is not held by it. Any issue of that shape is accepted from `nami-ci[bot]`, and is then handled as ADR-0021, point 6, says. Agents work on it only while PR #n was opened by `nami-agent[bot]` and has its branch in this repository. For any other PR it is held for a human, whatever its labels say, as is the bot's own queue-failure issue;
  - the `bot/passes` PR, only in the shape that PR #70 (issue #62) defines: its branch is `bot/passes` in this repository, its author is `nami-ci[bot]`, it changes only `passes` lines in `tasks.json` and each of them from `false` to `true`, the task's `task/<id>` PR has merged, and `cargo xtask task check-pr` re-runs that task's tests. #70 adds the check of this shape; until it lands, no `bot/passes` PR is trusted;
  - nothing else that it authors: not its other issues, not its comments, not its label changes.

  **Agents trust `nami-reviewer[bot]` for its reviews and review threads on PRs that `nami-agent[bot]` opened** (#59), as they trusted the AI review that `nami-agent[bot]` posted before: each thread is a finding to fix or to answer with why it is wrong. A thread asks for a change to the PR it is on, and nothing beyond it. Its text came from a model that read the PR, so it never authorises anything that AGENTS.md forbids. The reviewer app authors no issue or comment that agents act on.

  Trust follows the author only. GitHub lets any holder of `issues: write` edit the title and body of any issue, and any issue comment, without changing its author. So `nami-ci`'s token, like the agents' key, can rewrite an issue or comment by `nami-agent[bot]` or a code owner, and agents then trust the new text as that author's. With pull requests write, the same holds for the description of an open PR by `nami-agent[bot]`, where `cargo xtask task` reads `Fixes #<n>` to decide whether a queue failure's fix is in flight: an edit can hold such an issue, or release it. Nothing checks edits yet: issue #76 proposes refusing an issue or comment last edited by anyone but its author or a code owner.

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
  - it refuses an unset, empty or `{}` `NAMI_AGENT_PERMISSIONS` when it runs in CI (`CI` or `GITHUB_ACTIONS` is `true`) or for a CI app (`nami-ci` or `nami-reviewer`), and anywhere a value that is not a JSON object;
  - it refuses `jwt` in CI and for the CI apps, because a JWT mints tokens with every permission of the app;
  - it refuses `env` and `git-credential` for the CI apps: both serve the agents' git identity. So for the CI apps, and in CI, the only way to a token is `token`, which narrows it;
  - it refuses to act as the agents' app in CI, so a job cannot use `nami-agent`'s key even if one reached CI;
  - it never signs for a CI app with the agents' key or client ID, or with the other CI app's (`each_ci_app_signs_with_its_own_key`, `no_ci_app_falls_back_to_another_app_s_key`);
  - every refusal ends the script, also inside a command substitution: bash turns `set -e` off there, so the script sets `inherit_errexit` where bash has it and checks every substitution's status with `|| exit 1`. Its tests run `git-credential get` and `env` against a fake GitHub with unset, empty, `{}` and malformed permissions, and see no credential printed and no token requested.

  GitHub refuses a permission that the app's installation does not have. This checks that a job narrows its token at all, not that it narrows it to what the job needs. It also checks only what runs through the script: a job that signs a JWT with the key by other means is caught by review alone.
- **`cargo xtask queue failed`** does every read, of the open issues and of the runs they record too, with `GH_TOKEN`, and only creates the issue with `NAMI_FILING_TOKEN` when that is set. It removes `NAMI_FILING_TOKEN` from the environment of every read (rule 2). Its tests check both directions, through the same wiring that the command uses. In `queue.yml`, the workflow's token has `actions`, `contents`, `issues` and `pull-requests` read.
- **`xtask/src/issue/trust.rs`** with **`xtask/src/queue/shape.rs`** trusts `nami-ci[bot]` only for an issue whose title parses as a queue failure and whose body is what the finding's renderer writes again from parameters read out of the body. The comparison reads CRLF as LF and ignores ASCII whitespace at the end; everything else must match. The body's repository must be the one that the checkout's `origin` names, and names and codes are checked against the vocabulary above. When the vocabulary or the repository cannot be loaded, no issue by the CI app is trusted, and the refusal names the error. The body is untrusted text, so the check never asserts on it: a hostile body is refused, never a panic. Their tests check both directions:
  - accepted: every body the renderer writes;
  - refused: edited text, a link into another repository, a name or code outside the vocabulary, a stray backtick or mention, a malformed list, any other title, and any comment or label change by the CI app;
  - never a panic: hostile bodies fed through `trust::refused`, `task ready`, `claim` and `queue failed`.

  A name that `main`'s `ci.yml` or `guardrails.yml` drops after the issue was filed makes the issue untrusted: failing closed.
- **`cargo xtask task ready`, `claim` and `status`** hold every queue-failure issue, by the bot or the CI app, whose PR was not opened by `nami-agent[bot]`, or whose head branch is not in this repository (a fork's, or deleted). The reason says that a human handles the PR. They check this from GitHub for every such issue, not through `needs-decision`, and a head that cannot be read holds the issue too. When the CI app's issues cannot be checked at all, `ready` names each one it leaves out, with the reason, and exits non-zero. Their tests cover each condition. Repository names are compared without ASCII case, as GitHub compares them, and a remote with more than one URL names no repository.
- **One issue per failed merge group:** `cargo xtask queue failed` files nothing for a run whose queue branch and commit an open trusted issue's run already tested, and names that issue. `cargo xtask task ready`, `claim` and `status` hold a queue-failure issue while another open trusted one of the same PR, which it does not name as another merge group's, is claimed or closed by the PR's description, and the reason names that issue. Their tests cover the other workflow of one merge group, a re-run, a later merge group, and two issues of one PR claimed in turn.
- **CODEOWNERS** on `.github/` and `xtask/` (ADR-0008, ADR-0015): every change to a workflow, to the setup action, to `xtask/agent-token.sh` or to the trust rule waits for the code owner. This is the only check that a job narrows its tokens to what it needs (rule 2), keeps the secret in one step (rule 3), runs no PR code (rule 4) and restores no cache (rule 5): a human reads the job.
- **The key step in `queue.yml`** fails with an error that points to step 8 of the runbook when the environment has no key.

Rule 5 does not rely on GitHub's cache scoping. A `workflow_run` job runs in `main`'s cache scope, which keeps forks' caches out, but `main`'s own caches are written after running code that no human may have read.

No mechanism:

- that a job that names a secret also names an environment (rule 1), sets `NAMI_AGENT_PERMISSIONS` (rule 2), names the secret in exactly one step's `env:` and deletes the file (rule 3), runs only on `workflow_run`, on `push` to `main` or on `schedule`, and checks out no PR ref (rule 4), and restores no cache, directly or through the setup action (rule 5); nor that a job that mints a `nami-ci` token uses it only through the commands that produce the shapes agents trust. A tidy rule on `.github/workflows/` can see all of this in the text; #63 proposes it;
- that an environment exists before a job names it. **A job that names a missing environment makes GitHub create it, with no branch rule,** so every branch may then use its secrets. A typo in `environment:`, or a new app's job that lands before its environment is set up, creates such an environment, and a secret added to it later is open to every branch. Nothing checks for this automatically: the runbook's "Check it" commands are the only safeguard;
- that the environment's settings on GitHub still match step 8 between manual checks;
- that a `bot/passes` PR has the shape above, until PR #70 lands with its check;
- that an issue or comment was not edited after it was filed. Any holder of `issues: write` (`nami-ci`'s token, the agents' key, a workflow token given that permission) can rewrite an issue or comment by `nami-agent[bot]` or a code owner, and agents trust it on its author alone. Issue #76 proposes the check;
- whether a job executes PR code in a way that the text does not show, for example a tool that runs a script from a checked-out PR. Review is the backstop;
- that agents use `nami-agent`'s Actions write only to re-run the latest run, or to dispatch, and never to delete runs, logs or artifacts, cancel or disable workflows, re-run an old key-holding run, or approve a fork's run. GitHub cannot narrow the permission, and nothing in the repository sees these calls. Only the instructions and the audit log hold it.

## Consequences

- A job that acts as an app needs three more steps than one with the workflow token, and its design waits for the code owner.
- A leaked app token is narrowed and dies within an hour. A leaked CI key is revoked without touching the agents' machines, and the agents' key is never in CI to leak.
- Agents trust a third author, but only for two shapes. Code checks the queue-failure issue's title, its body and its PR. So with an issue that it files, a misused `nami-ci` token can at most do this: file a false report of a merge-queue failure of an open PR that `nami-agent[bot]` opened with its branch in this repository, in the renderer's own words, with links into this repository only. That report makes an agent check out that branch, rebase or renumber it, run `just check-full` on it, push to it and re-queue it: work that agents already do on their own PRs.
- That bound covers only what `nami-ci[bot]` authors. With the same `issues: write`, the token can also edit the title and body of any open issue, and any comment, by `nami-agent[bot]` or a code owner, with any text. Agents then trust the new text on its author alone, so this is the larger risk of a misused key until issue #76 lands: the same as a misused agents' key has today. `nami-ci[bot]` cannot author anything that speaks for the agents or for a code owner, but it can edit what they wrote. What the key's other permissions allow is in the decision above. PR #70 adds the check of the `bot/passes` PR.
- A key job builds xtask from scratch on every run, which costs a few minutes of CI per failed merge-queue run.
- The CI-hosted reviewer (#59) has the shape of the split above: a review job that reads the PR and holds no app key, and a posting job that holds `nami-reviewer`'s key and runs no PR code. The review job holds no stored key at all: it exchanges GitHub's OIDC token for a short-lived Anthropic token (workload identity federation), and executes no PR code either: Claude Code runs with read-only tools, no hooks, no settings files, no CLAUDE.md and no MCP servers, on an export of the head. The posting job reads its answer as data (`xtask/src/review/findings.rs`) and judges again which PR it may review.
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
