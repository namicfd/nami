# GitHub setup

How `namicfd/nami` is configured on GitHub, and how to set up the same thing again for another repository. The reasons are in the ADRs:

- ADR-0008: the human review model and the agents' identity;
- ADR-0009: hosting and the merge gate;
- ADR-0015: protected paths gated at review;
- ADR-0022: rule paths that merge on the ratchet's proof;
- ADR-0023: one GitHub App per role, and the rules for a CI job that holds an app's key.

Every command below was run against `namicfd/nami` on 2026-11-25.

## What lives where

| In the repository | Only on GitHub, or on the agents' machines |
|---|---|
| `.github/workflows/`, `.github/actions/setup/` | Repository and Actions settings (step 1) |
| `.github/CODEOWNERS` | The ruleset (step 3) |
| `xtask/agent-token.sh` | The GitHub App and its installation (step 5) |
| `.claude/settings.json` (`env`): the bot's git identity (step 6) | The app's private key on each machine, at `$NAMI_AGENT_KEY` or `~/.config/nami/nami-agent.pem` |
| `.github/workflows/queue.yml` and `.github/workflows/passes.yml`: the jobs that act as `nami-ci[bot]` in CI | The `nami-ci` app, its environment `nami-ci` with the secret `NAMI_CI_PRIVATE_KEY`, and the repository variable `NAMI_CI_CLIENT_ID` (step 8) |
| `.github/workflows/review.yml`: the CI-hosted AI review, which posts and approves as `nami-reviewer[bot]` | The `nami-reviewer` app, its environment `nami-reviewer` with the secret `NAMI_REVIEWER_PRIVATE_KEY`, the environment `claude-review` with the variables of Anthropic's federation rule, the federation rule and its service account in the Claude Console, and the repository variables `NAMI_REVIEWER_CLIENT_ID` and `NAMI_REVIEWER_LOGIN` (step 11) |

When you change a setting on GitHub, update this file.

## 1. Repository and Actions settings

- **Organisation.** The repository must belong to an organisation, because merge queues are only available there. A free organisation needs the repository to be public.
- **Permissions.** You need admin rights on the repository. `gh` needs the `repo` scope.

```sh
REPO=namicfd/nami

# Merging: squash only, auto-merge for the queue, delete merged branches.
gh api -X PATCH "repos/$REPO" -F allow_squash_merge=true -F allow_merge_commit=false \
  -F allow_rebase_merge=false -F allow_auto_merge=true -F delete_branch_on_merge=true

# Workflow tokens are read-only, and Actions may not approve PRs.
gh api -X PUT "repos/$REPO/actions/permissions/workflow" \
  -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false

# Workflows from forks wait for approval. The repository is public, so CI must not run for
# anyone who opens a PR.
gh api -X PUT "repos/$REPO/actions/permissions/fork-pr-contributor-approval" \
  -f approval_policy=all_external_contributors
```

## 2. Push `main` once, before the ruleset

- The ruleset requires three status checks: `gate` and `lints (UI tests)`, job names in `.github/workflows/ci.yml`, and `ratchet`, a job in `.github/workflows/guardrails.yml` (ADR-0022).
- Both workflows must keep their `merge_group:` trigger. Without it, the queue waits for checks that never run.
- Push `main` and let CI run once, so that `gate` and `lints (UI tests)` exist when the ruleset refers to them. After step 3, nobody without bypass rights can push to `main` directly.
- `ratchet` runs only on pull requests and merge groups: `guardrails.yml` has no push trigger, so pushing `main` never creates it. Its name first appears with the first pull-request run of a `guardrails.yml` that has the job, which for this repository was #57's own. Add it in step 10, not here. **On a rebuild of this setup** for a repository whose `main` already has the ratchet, require `ratchet` in step 3 at once, with the other two; the name appears with the first pull request.

## 3. Ruleset on the default branch

Go to Settings → Rules → Rulesets → New ruleset → New branch ruleset, and set the following:

- **Name:** `main`, with enforcement **Active**.
- **Target:** the default branch.
- **Rules:** as in the table below.

| Rule | Setting | Why |
|---|---|---|
| Restrict deletions; block force pushes | on | `main` is shared state for the whole swarm. |
| Require linear history | on | Each commit on `main` is one PR that passed the gate, so bisect and `git revert` stay simple (ADR-0009). |
| Require a pull request | approvals: 0 | PRs that touch no owned path need no human (ADR-0008): the approval that the last-push rule below asks for comes from the reviewer app (step 11). |
| | code-owner review: on | Human sign-off on protected paths (ADR-0015). |
| | dismiss stale approvals: on | An agent cannot push again after it has been approved: the push dismisses the approval. |
| | approve the last push: **on** | It asks for one approval of every PR by someone other than its last pusher, whatever the count above ("at least one other authorized reviewer has approved", GitHub's docs). Every PR's last push is a bot's: `nami-agent[bot]`'s, or `nami-ci[bot]`'s for `bot/passes`. The reviewer app pushes nothing, so its approval of the head satisfies the rule, and proves that the AI review ran on exactly that commit (#59). Until the app is set up, every PR needs the maintainer's approval (the maintainer's decision, 2026-11-28). |
| | additional approval for unattributed Copilot PRs: on | GitHub's default. It has no effect while the count is 0. |
| | conversation resolution: on | Blocking AI-review comments must be resolved. |
| | merge methods: squash | |
| Required status checks | `gate`, `lints (UI tests)`, `ratchet`; "up to date" is off | The merge queue tests the combined result anyway. `ratchet` is what lets rule paths merge without the code owner; it is added in step 10 (ADR-0022). |
| Merge queue | squash; build 5; group of 1–5; wait 5 min; all green; timeout 30 min | ADR-0009. The ratchet's two jobs fit in it: in a merge group they have 15 and 10 minutes, against about 2 each measured, which leaves time for runners to pick them up, and they carry the PR's mutation evidence over instead of mutating again (ADR-0022, point 5, which amends ADR-0009 for them). |
| Bypass | Repository admin, **pull requests only** | The maintainer can merge PRs past the rules: their own, since GitHub never allows approving your own PR, and, as an exception, a `tasks.json` flip that the `passes` workflow cannot make (ADR-0017). Nobody can push to `main` directly, not even with the maintainer's credentials. In an emergency, open a PR and merge it with the bypass. |

To check the live ruleset against this table, print it with `gh api "repos/$REPO/rulesets"`, then `gh api "repos/$REPO/rulesets/<id>"`.

**"Approve the last push" stays on** (the maintainer's decision, 2026-11-28). An earlier version of this step asked for it to be turned off, because it held #84, the first `passes` PR, and #47, both green and touching only unowned paths, until the maintainer approved them. Do not turn it off. The reviewer app's approval of the head is what clears it (step 11, #59). To check it, the setting must print `true`:

```sh
gh api "repos/$REPO/rules/branches/main" \
  --jq '.[] | select(.type == "pull_request") | .parameters.require_last_push_approval'
```

## 4. CODEOWNERS

- **Owner handle.** `.github/CODEOWNERS` names the maintainer (`@cxkoda`). In another repository, replace the handle.
- **Write access.** The owner needs write access. Otherwise GitHub silently ignores the line.
- **Syntax check.** Open the file on GitHub. Syntax errors are shown above its contents.
- **Keep in sync.** The list must match the protected paths in `AGENTS.md`.
- **Rule paths have no owner.** The lines without an owner after `/xtask/` and `/lints/` free the rule paths (ADR-0022); on GitHub the last matching line wins. They must equal `RULE_PATHS` in `xtask/src/ratchet/scope.rs`, which a test checks. Only those paths may be unowned: `cargo xtask pr-body` refuses any other line without an owner. Never free a path while `ratchet` is not a required check (step 10).

## 5. The agents' GitHub App

Agents act as `nami-agent[bot]`, never as the maintainer. GitHub does not let anyone approve their own PR, so code-owner review only works when the author is someone else.

### Create the app

1. Go to Organisation → Settings → Developer settings → GitHub Apps → **New GitHub App**.
2. Fill in the basics:
   - **Name:** `nami-agent`. The name must be unique on GitHub, and the bot is called `<name>[bot]`.
   - **Homepage URL:** the repository.
   - **Webhook:** uncheck **Active**.
3. Set the **repository permissions**:

   | Permission | Access | Why |
   |---|---|---|
   | Contents | read and write | Push branches. |
   | Pull requests | read and write | Open and update PRs. |
   | Workflows | read and write | Pushes that touch `.github/workflows/` need it. Agents may change protected paths (ADR-0015). |
   | Issues | read and write | Read and answer intake issues. |
   | Actions | read and write | Read CI results, and re-run or dispatch a workflow run that failed for a passing reason, such as the `passes` job (granted 2026-11-28, ADR-0023). |
   | Checks | read | Read CI results. |
   | Metadata | read | Set automatically. |

4. **Where can this app be installed:** only on this account. Then **Create GitHub App**.
5. **Generate a private key** and store it on every machine that runs agents. Never put it in a repository. The script reads it from `$NAMI_AGENT_KEY`, or from `~/.config/nami/nami-agent.pem` when that is unset:

   ```sh
   mkdir -p ~/.config/nami
   mv ~/Downloads/<app>.<date>.private-key.pem ~/.config/nami/nami-agent.pem
   chmod 600 ~/.config/nami/nami-agent.pem
   ```

   On a machine that manages secrets differently, such as an orchestrator, put the file wherever its secrets live and set `NAMI_AGENT_KEY` to that path in the agents' environment.

6. **Install App** → the organisation → **Only select repositories** → this repository.
7. Give the app **no** bypass in the ruleset, and do **not** make it a code owner.
8. **Check it.** The installation's permissions must match the table above, and nothing more. On a machine with the key:

   ```sh
   # The JWT goes through stdin, as in agent-token.sh, so it never shows in the process list.
   printf 'Authorization: Bearer %s\n' "$(xtask/agent-token.sh jwt)" |
     curl -sf -H @- -H "Accept: application/vnd.github+json" \
       https://api.github.com/app/installations | jq -c '.[] | {account: .account.login, permissions}'
   # Installed on this repository only: must print ["namicfd/nami"].
   GH_TOKEN=$(xtask/agent-token.sh) gh api installation/repositories --jq '[.repositories[].full_name]'
   ```

   On 2026-11-28 it printed `actions: write`, `checks: read`, `contents: write`, `issues: write`, `metadata: read`, `pull_requests: write` and `workflows: write`, for `namicfd`. A permission raised later takes effect only once the installation accepts it (organisation settings → GitHub Apps → the app → review the request).

### Point the script at the app

`xtask/agent-token.sh` has defaults for `nami-agent` on `namicfd/nami`. For another app or repository, change the defaults at its top, or set the environment variables. The values are public:

```sh
gh api apps/<app-slug> --jq .client_id              # NAMI_AGENT_CLIENT_ID
gh api 'users/<app-slug>%5Bbot%5D' --jq .id         # the number in bot_email
# NAMI_AGENT_REPO=<owner>/<name>; NAMI_AGENT_KEY=<path to the .pem>
```

The bot's commit email is `<bot user id>+<app-slug>[bot]@users.noreply.github.com`. GitHub uses that address to attribute commits to the bot. For another app, change the name and email in `.claude/settings.json` as well (step 6).

### Rotate or revoke the key

- **To rotate:** generate a new key in the app's settings, replace the file, then delete the old key there.
- **If a key leaks:** delete it in the app's settings first. Tokens already issued expire within an hour.

## 6. Running something as the bot

Agents act as the bot, never with a human's credentials. A maintainer's key reaches every repository they can access and is the ruleset's bypass actor; the bot's token reaches this repository only and expires within an hour.

### Claude Code sessions

The `env` block in `.claude/settings.json` makes every Claude Code session in this repository act as the bot for git, on any machine and in any worktree. Nothing is configured per clone, and nothing names a path on a particular machine. It sets:

- the bot as author and committer of every commit;
- an empty `credential.helper` first, which switches off the user's own helpers such as the macOS keychain, then `xtask/agent-token.sh git-credential`, found at run time through `git rev-parse --show-toplevel`, which hands git a fresh token for each operation;
- a rewrite of `git@github.com:` remotes to HTTPS, so a clone made over SSH pushes as the bot too;
- `GIT_TERMINAL_PROMPT=0`, so a missing key fails at once instead of waiting for a password.

Agents therefore run plain commands: `git push origin <branch>`. Claude Code refuses git commands that inject configuration, such as `git -c credential.helper=...`, in worktree-isolated sessions, and plain commands are what it accepts. For gh, which reads no git configuration, agents prefix each command: `GH_TOKEN=$(xtask/agent-token.sh) gh ...`.

The only thing a machine provides is the private key (step 5). The settings apply from the next session that starts; a running session keeps its environment. They need git 2.31 or newer for `GIT_CONFIG_COUNT`.

**If a push or a token is refused, stop and ask the maintainer.** Do not retry with another spelling of the command, and never fall back to a human's SSH key or keychain.

### Other launchers

Agent tools other than Claude Code, or orchestrators that set their own environment, do not read `.claude/settings.json`. Give them the same variables. In a shell:

```sh
eval "$(xtask/agent-token.sh env)"   # this shell and its children now act as nami-agent[bot]
claude -p "..."
```

`env` exports the variables above, with the script's absolute path as the helper, plus a `GH_TOKEN` that expires after an hour. Run it again for longer sessions. An orchestrator that cannot run a command at launch can copy the `env` block from `.claude/settings.json` as it stands, and set `NAMI_AGENT_KEY` if the key is not at the default path.

`xtask/agent-token.sh` alone prints one token, and `jwt` prints the app's own JWT for debugging. In CI the script refuses to act as `nami-agent` at all, and it refuses `jwt` for the CI app (step 8).

## 7. Issue labels

Agents file what they find outside their task as issues, and work on the ones that `cargo xtask task ready` lists (ADR-0021). The labels are the kinds and the maintainer's flag. `--force` updates a label that exists already.

```sh
gh label create guardrail-gap  --repo "$REPO" --force --color d93f0b --description "A check should have caught it (ADR-0020)"
gh label create bug            --repo "$REPO" --force --color b60205 --description "Something is broken"
gh label create docs           --repo "$REPO" --force --color 0075ca --description "A document is wrong or missing"
gh label create needs-decision --repo "$REPO" --force --color fbca04 --description "Waits for the maintainer; only a code owner removes it"
```

Which issues agents act on depends on who opened them, never on labels: only the bot and the users in `.github/CODEOWNERS`. To hand an outsider's report to the agents, file it again in your own words, with one kind label.

## 8. The CI app, `nami-ci`

**Done on 2026-11-26, by the maintainer.** The bot cannot read environments, secrets or variables, so nothing in this step was verified by the bot. Run "Check it" below to verify it.

Workflow tokens act as `github-actions[bot]`, whom agents do not trust (ADR-0021). A CI job whose output agents must act on therefore acts as a GitHub App. CI has its own app, `nami-ci`, separate from the agents' `nami-agent` (step 5): one app per role, each with the least permissions its role needs (ADR-0023). The agents' key is never in CI.

Two jobs use it. `file queue failure` in `.github/workflows/queue.yml`: when the required checks fail on a merge-queue branch and the PR has really left the queue, it files an issue `PR #<n> failed in the merge queue (run <id>)` of kind `bug`, so that `cargo xtask task ready` lists it (issue #51, ADR-0021, point 6). Agents trust `nami-ci[bot]` for an issue with that title and the body that `queue failed` writes for it, linking only into this repository, and work on it only while the PR was opened by `nami-agent[bot]` with its branch here. `open passes PR` in `.github/workflows/passes.yml` (#70, ADR-0017) opens the one PR from `bot/passes` that sets `passes`, which `check-pr` judges in the `gate`. The CI-hosted reviewer (#59) gets an app of its own.

ADR-0023 sets the rules for every CI job that holds an app's key. Read it before you add one. The queue-failure job follows them:

- it runs on `workflow_run`, which takes the workflow file and xtask from `main`, never from a PR, and its checkout has no `ref:`;
- the key is in an environment that only `main` may use, so a PR that edits a workflow to read it is refused;
- the key is in the environment of one step only, which writes it to a file. The next step mints the token from the file and deletes it before xtask starts, and a last step that always runs deletes it if anything failed in between;
- it files only for runs of `.github/workflows/ci.yml` and `.github/workflows/guardrails.yml`, which holds the `ratchet` check, so a workflow that a PR adds under the name `ci` or `guardrails` triggers nothing. Each failed merge group gets one issue: when both workflows fail for one merge group, the run handled second finds the first one's open issue, whose run tested the same queue branch and commit, and files nothing;
- every read, of the open issues and their runs too, uses the workflow's own token, with `actions`, `contents`, `issues` and `pull-requests` read. The `nami-ci` token is narrowed to `issues: write`, only creates the issue, and expires within an hour;
- it restores no cache, and builds xtask from a clean checkout.

The other job in that workflow, `adr clashes (advisory)`, comments with the workflow's own token and needs no setup.

### What was set up

| What | Value |
|---|---|
| GitHub App | `nami-ci`, client ID `Iv23ctUmyQzRCojbV2Em`, webhooks off |
| Bot | `nami-ci[bot]`, user ID 361707243 |
| Installation | `namicfd/nami` only |
| Repository permissions | Contents: read and write. Issues: read and write. Pull requests: read and write. Metadata: read. Contents and pull requests write are for the `bot/passes` PR (ADR-0017); the queue-failure job never asks for them. No Actions permission: jobs read runs and logs with the workflow's own token. |
| Environment | `nami-ci`, deployment branches limited to `main`, no required reviewers |
| Environment secret | `NAMI_CI_PRIVATE_KEY`: the full contents of a private key of `nami-ci` |
| Repository variable | `NAMI_CI_CLIENT_ID`: `Iv23ctUmyQzRCojbV2Em` |

There is no `nami-agent` environment, and there must not be one. A job that names a missing environment makes GitHub create it with no branch rule, so that every branch may use any secret added to it later. Create an environment, and limit it to `main`, before any job names it. Until ADR-0023's PR (#64) merged, `main`'s `queue.yml` named `environment: nami-agent`, so a merge-queue failure before then may have created one. After that merge, run "Check it" below, and delete any `nami-agent` environment it lists.

### The `bot/passes` ruleset

**Requested of the maintainer, not set yet:** a branch ruleset on `bot/passes` that restricts creations, updates and deletions, with the `nami-ci` app as its only bypass actor. Then only `nami-ci[bot]` can push the branch whose PR sets `passes` (ADR-0017). Until it exists, anyone with write access can push there, agents included. `check-pr` judges such a push by its content, so safety does not depend on the ruleset, but liveness does: the next `passes` run rebuilds the branch, unless the push is exactly the wanted flips on a commit of `main`, which is what the run would push itself, and until then the PR is out of the queue.

In Settings → Rules → Rulesets → New ruleset → **New branch ruleset**:

- **Name:** `bot/passes`, with enforcement **Active**.
- **Bypass list:** Add bypass → the `nami-ci` app → **Always allow**. Nothing else, not even the admin role.
- **Target branches:** Add target → Include by pattern → `bot/passes`.
- **Rules:** **Restrict creations**, **Restrict updates** and **Restrict deletions** on; every other rule off. In particular leave "Block force pushes" off: the job force-pushes the branch every time it rebuilds it.
- **Create.**

Or with `gh`. The bypass actor's ID is the app's ID, `gh api apps/nami-ci --jq .id`, which was 5942623 on 2026-11-28; it is neither the bot's user ID nor the installation's. Save this as `bot-passes-ruleset.json`:

```json
{"name": "bot/passes", "target": "branch", "enforcement": "active",
 "conditions": {"ref_name": {"include": ["refs/heads/bot/passes"], "exclude": []}},
 "rules": [{"type": "creation"},
           {"type": "update", "parameters": {"update_allows_fetch_and_merge": false}},
           {"type": "deletion"}],
 "bypass_actors": [{"actor_id": 5942623, "actor_type": "Integration", "bypass_mode": "always"}]}
```

Then create the ruleset and delete the file:

```sh
gh api -X POST "repos/$REPO/rulesets" --input bot-passes-ruleset.json
rm bot-passes-ruleset.json
```

**Check it.** As the maintainer, `gh api "repos/$REPO/rulesets" --jq '.[] | select(.name == "bot/passes") | .id'`, then `gh api "repos/$REPO/rulesets/<id>" --jq '{enforcement, conditions, rules: [.rules[].type], bypass_actors}'` must print enforcement `active`, the one include `refs/heads/bot/passes`, the rules `["creation","update","deletion"]`, and the one bypass actor `{"actor_id":5942623,"actor_type":"Integration","bypass_mode":"always"}`. An agent can check the rules without admin rights: `GH_TOKEN=$(xtask/agent-token.sh) gh api repos/$REPO/rules/branches/bot/passes --jq '[.[].type]'` must print `["creation","update","deletion"]`. The next `passes` run that pushes must still succeed; if its push is refused with `GH013`, the bypass names the wrong actor.

The job never deletes the branch. If GitHub's deletion of a merged head branch is refused under this ruleset, the branch stays after the merge. That is harmless: the next run that has flips to make force-pushes over it, and a run with none changes nothing.

### Set it up again

For another repository, or after the app is lost:

1. Create a GitHub App as in step 5, named `nami-ci`, with webhooks off and the repository permissions in the table above, and no others. Install it on this repository only. Give it no bypass in the ruleset, and do not make it a code owner.
2. Go to Settings → Environments → **New environment**, named `nami-ci`. Under **Deployment branches and tags**, choose **Selected branches and tags** and add the rule `main`. Add no required reviewers: the job runs unattended.
3. Generate a private key in the app's settings and add it as the environment secret `NAMI_CI_PRIVATE_KEY`, then delete the downloaded file.
4. Add the repository variable `NAMI_CI_CLIENT_ID` with the app's client ID.

With `gh`, as the maintainer:

```sh
gh api -X PUT "repos/$REPO/environments/nami-ci" --input - <<'EOF'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
EOF
gh api -X POST "repos/$REPO/environments/nami-ci/deployment-branch-policies" \
  -f name=main -f type=branch
gh secret set NAMI_CI_PRIVATE_KEY --repo "$REPO" --env nami-ci < nami-ci.private-key.pem
gh variable set NAMI_CI_CLIENT_ID --repo "$REPO" --body "$(gh api apps/nami-ci --jq .client_id)"
rm nami-ci.private-key.pem
```

### Check it

Run these as the maintainer. They print names and settings, never a secret's value:

```sh
gh api apps/nami-ci --jq '{client_id, permissions}'
gh api 'users/nami-ci%5Bbot%5D' --jq .id
gh api "repos/$REPO/environments" --jq '[.environments[].name]'
gh api "repos/$REPO/environments/nami-ci" \
  --jq '{deployment_branch_policy, rules: [.protection_rules[].type]}'
gh api "repos/$REPO/environments/nami-ci/deployment-branch-policies" \
  --jq '[.branch_policies[] | {name, type}]'
gh secret list --repo "$REPO" --env nami-ci
gh variable list --repo "$REPO"
```

Expect:

- the client ID `Iv23ctUmyQzRCojbV2Em`, and the permissions `contents`, `issues` and `pull_requests` `write` and `metadata` `read`, with no `actions`;
- the user ID 361707243;
- an environment list with `nami-ci` and without `nami-agent`;
- `{"protected_branches":false,"custom_branch_policies":true}`, and the rules `["branch_policy"]` only, with no `required_reviewers`;
- the one branch policy `{"name":"main","type":"branch"}`;
- `NAMI_CI_PRIVATE_KEY` in the environment's secrets, and `NAMI_CI_CLIENT_ID` among the variables.

In the app's settings, **Install App** shows one installation, on this repository only. The first failed merge-queue run should leave an issue `PR #<n> failed in the merge queue (run <id>)` by `nami-ci[bot]`, labelled `bug`. If the job fails instead, its log says which step: a missing secret points here, and a missing variable says `NAMI_CI_CLIENT_ID is not set`.

### Rotate or revoke the key

- **To rotate:** generate a new key in the `nami-ci` app's settings and replace the secret with it (`gh secret set NAMI_CI_PRIVATE_KEY --repo "$REPO" --env nami-ci < <the new key's file>`). Delete the downloaded file, then delete the old key in the app's settings. No agent's machine is touched.
- **If the key leaks:** delete it in the app's settings first, then set a new one as above. Tokens already issued expire within an hour. Close any issue by `nami-ci[bot]` that the queue did not file. The key's `issues: write` also lets it edit any issue or comment without changing its author, and agents trust that text by its author (ADR-0023, issue #76). So check the edit history of the open issues and comments by `nami-agent[bot]` and the code owners, and of the descriptions of open PRs by `nami-agent[bot]`, where `cargo xtask task` reads `Fixes #<n>` for queue failures; and check the pushes and PRs by `nami-ci[bot]`.

## 9. Check that it works

```sh
GH_TOKEN=$(xtask/agent-token.sh) gh api installation/repositories --jq '[.repositories[].full_name]'
(eval "$(xtask/agent-token.sh env)"; gh auth status; git var GIT_AUTHOR_IDENT)
gh api "repos/$REPO/rulesets" --jq '.[] | {name, enforcement}'
```

In a new Claude Code session, check that the settings reached the shell. The username must be `x-access-token`; anything else means a human's credentials would be used:

```sh
git var GIT_AUTHOR_IDENT
printf 'protocol=https\nhost=github.com\n\n' | git credential fill | grep '^username'
```

Then try the flow end to end:

1. **Protected path.** As the bot, push a branch that touches a protected path (any path listed in `.github/CODEOWNERS`) and open a PR. It must show "Review required" from the code owner, and must not enter the queue until that review is given.
2. **Unowned path.** A PR touching only unowned paths, for example `crates/kernels/src/`, must enter the queue once `gate` and `lints (UI tests)` pass and the reviewer app has approved its head (step 11), with no review by you. A PR that the app has not approved shows "Review required", because "approve the last push" is on.
3. **Direct push.** A direct `git push origin main` by the bot must be rejected.
4. **Rule path** (only once the policy flip of step 10 has merged; before that, CODEOWNERS owns the rule paths and point 1 applies). A PR that changes only rule paths, for example a new UI case in `lints/ui/` with its `.stderr` (a new case in `xtask/fixtures/tidy/` alone cannot pass `gate`, which allows exactly the two fixtures of each source rule), must show no "Review required", must show `ratchet` as required, and must enter the queue only once `ratchet` passes. An additive PR, such as that new UI case, passes `ratchet` with no review, its summary saying `(additive)`; a request for changes or an open thread by the reviewer app that `NAMI_REVIEWER_LOGIN` names turns it red. For a PR that edits existing rule code, for example a changed fixture, the summary says `(edit)`, and `ratchet` stays red until the app approves the head commit, or you do. GitHub does not request your review for it, because the rule paths have no owner; the agent requests it (AGENTS.md, protected paths).

## 10. Turning on the ratchet (ADR-0022)

The ratchet PR (#57) adds the ratchet in shadow mode: both jobs run and show their verdict in the job summary, and CODEOWNERS still owns every guardrail path. The policy PR (#55) then frees the rule paths in `.github/CODEOWNERS`. GitHub applies the base branch's CODEOWNERS, so the rule paths lose their owner the moment that PR merges. So `ratchet` must be required before #55 merges, and it can only be required once its name exists, which #57's own pull-request runs give it. Steps 1 and 2 are done: on 2026-11-28 `ratchet` was made a required check, and #57 merged as 3706935. Do these steps in this order:

1. **Add `ratchet` as a required check, immediately before merging #57** (done). Do points 1 and 2 of this step back to back: from the moment it is required until #57 merges, every other PR is blocked, because no base has a `ratchet` job that reports. Settings → Rules → Rulesets → `main` → Require status checks to pass → Add checks → type `ratchet` → choose the GitHub Actions source → Save changes. The name is offered because #57's runs reported it. Only `ratchet` is required; `ratchet (head)` is the job that runs the PR's code, and `ratchet` judges its output. A review runs `ratchet` again from `.github/workflows/ratchet-review.yml`, under the same name. The bot cannot do this: it has no administration permission, and must not have one.
2. **Merge #57** (done). On #57, `ratchet` passes with "The base branch has no ratchet yet; skipped." in its summary, because its base cannot judge yet. The PR touches the trusted core, so it waits for your review in any case.
3. **Rebase #55 onto `main`, let its `guardrails` run, then merge it.** #57 lands as one squashed commit, so rebase only #55's own commits: `git fetch origin && git rebase --onto origin/main <#57's head SHA> task/issue-50`, then push with `--force-with-lease`. Its run is the first that `main`'s ratchet judges. #55 changes CODEOWNERS and other core files, so it waits for your review as well. From its merge on, the rule paths have no owner, and the checks of step 9, point 4, hold.
4. **Rebase or close stale PRs.** A PR whose branch predates the ratchet has no `ratchet` job, so the required check never reports; it waits until it is rebased onto `main`.
5. **Set up the reviewer app** as step 11 says. It reviews in CI (#59), and the variable `NAMI_REVIEWER_LOGIN` that step 11 sets makes the ratchet count it. Until you do, an additive PR that only changes rule paths passes `ratchet` alone, but still waits for your approval of its last push (step 3), and an edit gets no verdict unless you approve its head commit. GitHub no longer requests that review, because the rule paths have no owner: the agent requests it with `gh pr edit --add-reviewer` when the summary says `(edit)` (AGENTS.md), and the PR waits for your approval of its head commit. Your approval of the head commit settles a "no verdict" only when each reason is unprovable though the evidence is complete: an edit without the app's approval, a hunk without a mutant, a build file, or a form the ratchet refuses (a file outside the dialect, a symbolic link, a name that is not valid UTF-8, a rule path whose name holds a `\` or a character outside printable ASCII, or two names with one key). It becomes a pass that lists the reason (ADR-0022, point 3). Missing or broken evidence (a tool that failed, a failed phase of `observe`, missing observations, a failed null-mutant run, a skipped check) is never settled, and the verdict line says "fix the PR or re-run". Neither is a merge group whose rule-path diff differs from the carried one: rebase the PR and push, so that it is observed on its new base. A mutant that timed out recurs on every re-run: make the tests that cover it finish within the timeout, or split the PR. An honest PR's remedy is a change to the PR, or a re-run; the admin bypass is for deliberate loosenings (below). Findings always fail, even with your approval: a PR the ratchet finds a loosening in is fixed, or merged as a deliberate loosening (below). A manifest or build file in a rule path needs your approval, whatever the app says. The app must be able to submit `APPROVE`: an edit to existing rule code merges only on its approval of the head commit, or yours (ADR-0022, point 2).
6. **Check it** with step 9, point 4.

To print what the ruleset requires:

```sh
id=$(gh api "repos/$REPO/rulesets" --jq '.[] | select(.name == "main") | .id')
gh api "repos/$REPO/rulesets/$id" --jq '.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context'
```

The list must contain `gate`, `lints (UI tests)` and `ratchet`.

**A deliberate loosening.** The ratchet cannot pass a PR that loosens a rule on purpose, approved through an ADR. Merge such a PR with the admin bypass (step 3, Bypass), as for `passes` flips (ADR-0017).

**Turning it off.** Remove the owner-less lines from `.github/CODEOWNERS` first, in a PR of its own, and only then the required check. That PR must change the two tests of `xtask/src/pr.rs` that hold the file to the flip, `real_codeowners_unowns_exactly_the_rule_paths` and `the_real_codeowners_is_the_ratchets_flip`, in the same PR.

## 11. The reviewer app, `nami-reviewer`

**Requested of the maintainer, not set up yet** (#59). The bot cannot create apps, environments, secrets or variables, and nothing in this step has been verified by it.

`.github/workflows/review.yml` runs the AI review of ADR-0008 in CI, after `ci` passes on a pull request from this repository, and posts it as `nami-reviewer[bot]` (ADR-0023, one app per role):

- **A PR by `nami-agent[bot]` or a code owner** is reviewed by Claude Code, with the `tigerstyle-reviewer` and `numerics-verifier` definitions from `main`'s `.claude/agents/`, the lead's prompt from `main`'s `.claude/skills/review-pr/ci.md` and read-only tools. The app approves the head when nothing blocks and none of its earlier threads stays open. Otherwise it comments, with a thread per blocking finding. Its next run, on the next head, reads the replies by `nami-agent[bot]` and the code owners on its threads, never anyone else's, and resolves the ones that head fixes, or that a reply shows to be wrong. A thread that anyone else resolved stays open, and the app opens it again: only it resolves its threads (ADR-0022). It gives no verdict when files under `crates/` or `tools/` changed and `numerics-verifier` did not run. It reviews only PRs into `main`.
- **The `bot/passes` PR by `nami-ci[bot]`** is approved once `ci` passed on its head, with no model run: its review is `cargo xtask task check-pr` in `ci`'s `gate`, which judges every `passes` flip (ADR-0017). `nami-ci` cannot approve it itself, because GitHub never lets a PR's author approve it.
- **Every other PR** gets nothing: an outsider's PR never costs a model run.

The approval satisfies "approve the last push" (step 3), because the app never pushes. A push dismisses it, and the new head is reviewed again. The ratchet counts the same app's reviews for rule paths (ADR-0022, `NAMI_REVIEWER_LOGIN`).

### What the separate app protects, and what it does not

- **The app's key is out of the authoring machines' reach.** It is a secret of the environment `nami-reviewer`, which only `main` may use, and the one job that holds it runs no PR code. No agent can post, resolve or approve as the reviewer.
- **The model reads the PR, and can be misled by it.** A PR written to deceive the reviewer could be approved. The review runs only for PRs by the agents' app or a code owner, its tools only read, and `post` reads its answer as data: known reviewers, places in the PR's files, bounded text, mentions broken. Protected paths still need your approval, rule-path edits still need the ratchet's checks, and the sampled audits of ADR-0008 stand behind the rest.
- **Contents: write is a permission the role does not use.** Third-party tests report that GitHub counts an app's approval only when the app has write access to the repository's contents. The job narrows its token to `pull_requests: write`, but a leaked key can mint contents write and push to any branch that the rulesets leave open, open PR branches included. `main` stays behind the ruleset. A push by the app makes the app the last pusher, so its own approval of that head never counts.
- **No stored Anthropic key.** The model job signs in through Anthropic's workload identity federation. GitHub gives the job an OIDC token, which Anthropic exchanges for a token that lives at most ten minutes, as a Console service account. The rule accepts only this repository's environment `claude-review`, on `main`, from `review.yml`, so no other branch, workflow or repository can use it.

### Set it up, in one sitting before #98 merges

A job that names a missing environment makes GitHub create it with no branch rule (ADR-0023), so all of this comes before `review.yml` reaches `main`.

1. **The GitHub App.** Create it as in step 5, named `nami-reviewer`: webhooks off, only on this account. Repository permissions: Pull requests read and write, to review, approve and resolve its threads; Contents read and write, without which GitHub does not count its approval (above); Metadata read. Install it on this repository only. Give it no bypass in any ruleset, and do not make it a code owner. Generate a private key.
2. **The environment `nami-reviewer`.** Settings → Environments → New environment. Under **Deployment branches and tags**, choose **Selected branches and tags** and add `main`; no required reviewers. Add the secret `NAMI_REVIEWER_PRIVATE_KEY`: the full contents of the app's private key. Delete the downloaded file.
3. **The Anthropic federation.** In the Claude Console, as an admin: Settings → Workload identity → **Connect workload** → **GitHub Actions**. Keep the issuer the wizard fills in (`https://token.actions.githubusercontent.com`), and set:

   | Field | Value |
   |---|---|
   | Audience | `https://api.anthropic.com` |
   | Subject (`subject_prefix`, no trailing `*`, so an exact match) | `repo:namicfd/nami:environment:claude-review` |
   | Claims | `ref` = `refs/heads/main`, `job_workflow_ref` = `namicfd/nami/.github/workflows/review.yml@refs/heads/main` |
   | Service account | a new one, such as `nami-review`, in one workspace that has a monthly spend limit |
   | Scope and token lifetime | the wizard's defaults, `workspace:developer` and 600 seconds |

   Note the rule's ID (`fdrl_…`), the service account's ID (`svac_…`) and the organization ID.
4. **The environment `claude-review`.** Limited to `main` like the first, with no reviewers and no secrets. It holds three environment variables, which are IDs, not secrets: `ANTHROPIC_FEDERATION_RULE_ID`, `ANTHROPIC_ORGANIZATION_ID` and `ANTHROPIC_SERVICE_ACCOUNT_ID`.
5. **The repository variables.** `NAMI_REVIEWER_CLIENT_ID`, the app's client ID; and `NAMI_REVIEWER_LOGIN`, `nami-reviewer[bot]`, which also makes the ratchet count the app (step 10). Setting the login before #98 merges changes nothing until the app posts.
6. **Approve #98.**

With `gh`, as the maintainer, for points 2, 4 and 5:

```sh
for environment in nami-reviewer claude-review; do
  gh api -X PUT "repos/$REPO/environments/$environment" \
    -F 'deployment_branch_policy[protected_branches]=false' \
    -F 'deployment_branch_policy[custom_branch_policies]=true'
  gh api -X POST "repos/$REPO/environments/$environment/deployment-branch-policies" \
    -f name=main -f type=branch
done
gh secret set NAMI_REVIEWER_PRIVATE_KEY --repo "$REPO" --env nami-reviewer < nami-reviewer.private-key.pem
rm nami-reviewer.private-key.pem
gh variable set ANTHROPIC_FEDERATION_RULE_ID --repo "$REPO" --env claude-review --body 'fdrl_…'
gh variable set ANTHROPIC_ORGANIZATION_ID --repo "$REPO" --env claude-review --body '<organization ID>'
gh variable set ANTHROPIC_SERVICE_ACCOUNT_ID --repo "$REPO" --env claude-review --body 'svac_…'
gh variable set NAMI_REVIEWER_CLIENT_ID --repo "$REPO" --body "$(gh api apps/nami-reviewer --jq .client_id)"
gh variable set NAMI_REVIEWER_LOGIN --repo "$REPO" --body 'nami-reviewer[bot]'
```

### Check it

The first PR by `nami-agent[bot]` after #98 merges is the test. Once `ci` passes, the `review` workflow's three jobs run, and `nami-reviewer[bot]` reviews it.

- **If the first agent PR after setup stays BLOCKED after the reviewer approves, the app's permissions are wrong.**
- If `review (model)` fails at the token exchange, its log prints the token's `sub`, `ref` and `job_workflow_ref`: the rule's subject and claims must equal them.
- A missing variable or secret is named in the failed job's log, with a pointer here.

### Rotate or revoke

- **The app's key:** generate a new key in the app's settings, replace the secret, delete the file, then delete the old key there. If it leaks, delete it first; tokens already issued expire within an hour. Then check the reviews, approvals and pushes by `nami-reviewer[bot]`.
- **The Anthropic access:** there is no key to rotate. To cut it off, disable the federation rule or the service account in the Console. The model job fails, and uploads nothing, if an answer ever contains the identity token or GitHub's request token.
