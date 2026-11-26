# GitHub setup

How `namicfd/nami` is configured on GitHub, and how to set up the same thing again for another repository. The reasons are in the ADRs:

- ADR-0008: the human review model and the agents' identity;
- ADR-0009: hosting and the merge gate;
- ADR-0015: protected paths gated at review.

Every command below was run against `namicfd/nami` on 2026-11-25.

## What lives where

| In the repository | Only on GitHub, or on the agents' machines |
|---|---|
| `.github/workflows/`, `.github/actions/setup/` | Repository and Actions settings (step 1) |
| `.github/CODEOWNERS` | The ruleset (step 3) |
| `xtask/agent-token.sh` | The GitHub App and its installation (step 5) |
| `.claude/settings.json` (`env`): the bot's git identity (step 6) | The app's private key on each machine, at `$NAMI_AGENT_KEY` or `~/.config/nami/nami-agent.pem` |
| `.github/workflows/queue.yml`: the jobs that act as the bot in CI | The `nami-agent` environment and its secret `NAMI_AGENT_PRIVATE_KEY` (step 8) |

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

- The ruleset requires two status checks, `gate` and `lints (UI tests)`. These are job names in `.github/workflows/ci.yml`.
- That workflow must keep its `merge_group:` trigger. Without it, the queue waits for checks that never run.
- Push `main` and let CI run once, so that both checks exist when the ruleset refers to them. After step 3, nobody without bypass rights can push to `main` directly.

## 3. Ruleset on the default branch

Go to Settings → Rules → Rulesets → New ruleset → New branch ruleset, and set the following:

- **Name:** `main`, with enforcement **Active**.
- **Target:** the default branch.
- **Rules:** as in the table below.

| Rule | Setting | Why |
|---|---|---|
| Restrict deletions; block force pushes | on | `main` is shared state for the whole swarm. |
| Require linear history | on | Each commit on `main` is one PR that passed the gate, so bisect and `git revert` stay simple (ADR-0009). |
| Require a pull request | approvals: 0 | PRs that touch no owned path merge without a human (ADR-0008). |
| | code-owner review: on | Human sign-off on protected paths (ADR-0015). |
| | dismiss stale approvals; approve the last push: on | An agent cannot push again after it has been approved. |
| | conversation resolution: on | Blocking AI-review comments must be resolved. |
| | merge methods: squash | |
| Required status checks | `gate`, `lints (UI tests)`; "up to date" is off | The merge queue tests the combined result anyway. |
| Merge queue | squash; build 5; group of 1–5; wait 5 min; all green; timeout 30 min | ADR-0009. |
| Bypass | Repository admin, **pull requests only** | The maintainer can merge PRs past the rules: their own, since GitHub never allows approving your own PR, and `tasks.json` flips (ADR-0017). Nobody can push to `main` directly, not even with the maintainer's credentials. In an emergency, open a PR and merge it with the bypass. |

To check the live ruleset against this table, print it with `gh api "repos/$REPO/rulesets"`, then `gh api "repos/$REPO/rulesets/<id>"`.

## 4. CODEOWNERS

- **Owner handle.** `.github/CODEOWNERS` names the maintainer (`@cxkoda`). In another repository, replace the handle.
- **Write access.** The owner needs write access. Otherwise GitHub silently ignores the line.
- **Syntax check.** Open the file on GitHub. Syntax errors are shown above its contents.
- **Keep in sync.** The list must match the protected paths in `AGENTS.md`.

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
   | Actions, Checks | read | Read CI results. |
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

`xtask/agent-token.sh` alone prints one token, and `jwt` prints the app's own JWT for debugging.

## 7. Issue labels

Agents file what they find outside their task as issues, and work on the ones that `cargo xtask task ready` lists (ADR-0021). The labels are the kinds and the maintainer's flag. `--force` updates a label that exists already.

```sh
gh label create guardrail-gap  --repo "$REPO" --force --color d93f0b --description "A check should have caught it (ADR-0020)"
gh label create bug            --repo "$REPO" --force --color b60205 --description "Something is broken"
gh label create docs           --repo "$REPO" --force --color 0075ca --description "A document is wrong or missing"
gh label create needs-decision --repo "$REPO" --force --color fbca04 --description "Waits for the maintainer; only a code owner removes it"
```

Which issues agents act on depends on who opened them, never on labels: only the bot and the users in `.github/CODEOWNERS`. To hand an outsider's report to the agents, file it again in your own words, with one kind label.

## 8. The bot in CI

Workflow tokens act as `github-actions[bot]`, whom agents do not trust (ADR-0021). A job whose output agents must act on therefore acts as the bot, through the same `xtask/agent-token.sh` as an agent's session. Today one job does: `file queue failure` in `.github/workflows/queue.yml`. When the required checks fail on a merge-queue branch and the PR has really left the queue, it files an issue of kind `bug` for that PR, so that `cargo xtask task ready` lists it (issue #51, ADR-0021, point 6). Until the steps below are done, that job fails with an error that points here, and nothing is filed.

ADR-0023 sets the rules for every CI job that holds a bot's key. Read it before you add one, such as the CI-hosted reviewer (#59). This job follows them, so it is safe to give the key:

- it runs on `workflow_run`, which takes the workflow file and xtask from `main`, never from a PR;
- the key sits in an environment that only `main` may use, so a PR that edits a workflow to read it is refused;
- the key is in the environment of one step only, which writes it to a file. The next step mints the token from the file and deletes it before xtask starts, and a last step that always runs deletes it if anything failed in between;
- it files only for runs of `.github/workflows/ci.yml`, so a workflow that a PR adds under the name `ci` triggers nothing;
- `NAMI_AGENT_PERMISSIONS` narrows its token to `issues: write`, and `actions`, `contents` and `pull_requests` read, and the token expires within an hour.

The other job in that workflow, `adr clashes (advisory)`, comments with the workflow's own token and needs no setup.

### Set it up

Unlike the steps above, these have not been run against `namicfd/nami` yet. When you run them, remove this sentence.

1. Go to Settings → Environments. Open `nami-agent` if it is there, or create it with **New environment**. GitHub creates an environment by itself, with no rules, the first time a workflow job names it, so it may exist already.
2. Under **Deployment branches and tags**, choose **Selected branches and tags** and add the rule `main`. Check this rule even if the environment existed already: an environment that GitHub created by itself lets every branch use its secrets. Add no required reviewers: the job runs unattended.
3. Under **Environment secrets**, add `NAMI_AGENT_PRIVATE_KEY` with the full contents of a private key of the app (step 5), from `-----BEGIN` to the last line. Generate a separate key for CI, so that it can be revoked without touching the agents' machines.

With `gh`, from a machine that holds the CI key:

```sh
gh api -X PUT "repos/$REPO/environments/nami-agent" --input - <<'EOF'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
EOF
gh api -X POST "repos/$REPO/environments/nami-agent/deployment-branch-policies" \
  -f name=main -f type=branch
gh secret set NAMI_AGENT_PRIVATE_KEY --repo "$REPO" --env nami-agent < nami-agent-ci.pem
```

### Check it

```sh
gh api "repos/$REPO/environments/nami-agent" --jq '.deployment_branch_policy'
gh api "repos/$REPO/environments/nami-agent/deployment-branch-policies" --jq '.branch_policies[].name'
gh secret list --repo "$REPO" --env nami-agent
```

The first failed merge-queue run after that should leave an issue `PR #<n> failed in the merge queue (run <id>)` by `nami-agent[bot]`, labelled `bug`. To rotate the CI key, generate a new one in the app's settings, replace the secret, then delete the old key there.

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
2. **Unowned path.** A PR touching only unowned paths, for example `crates/kernels/src/`, must enter the queue once `gate` and `lints (UI tests)` pass, with no review.
3. **Direct push.** A direct `git push origin main` by the bot must be rejected.
