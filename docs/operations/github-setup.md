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
| | The app's private key, at `~/.config/nami/nami-agent.pem` |

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
| Bypass | Repository admin, **always** | The maintainer can merge their own PRs, since GitHub never allows approving your own PR, and can push in an emergency. Use `pull_request` mode instead to allow PR merges only. |

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
5. **Generate a private key** and store it on every machine that runs agents. Never put it in a repository.

   ```sh
   mkdir -p ~/.config/nami
   mv ~/Downloads/<app>.<date>.private-key.pem ~/.config/nami/nami-agent.pem
   chmod 600 ~/.config/nami/nami-agent.pem
   ```

6. **Install App** → the organisation → **Only select repositories** → this repository.
7. Give the app **no** bypass in the ruleset, and do **not** make it a code owner.

### Point the script at the app

`xtask/agent-token.sh` has defaults for `nami-agent` on `namicfd/nami`. For another app or repository, change the defaults at its top, or set the environment variables. The values are public:

```sh
gh api apps/<app-slug> --jq .client_id              # NAMI_AGENT_CLIENT_ID
gh api 'users/<app-slug>%5Bbot%5D' --jq .id         # the number in bot_email
# NAMI_AGENT_REPO=<owner>/<name>; NAMI_AGENT_KEY=<path to the .pem>
```

The bot's commit email is `<bot user id>+<app-slug>[bot]@users.noreply.github.com`. GitHub uses that address to attribute commits to the bot.

### Rotate or revoke the key

- **To rotate:** generate a new key in the app's settings, replace the file, then delete the old key there.
- **If a key leaks:** delete it in the app's settings first. Tokens already issued expire within an hour.

## 6. Running something as the bot

```sh
eval "$(xtask/agent-token.sh env)"   # this shell and its children now act as nami-agent[bot]
claude -p "..."
```

- **Commits.** They are authored and committed by the bot.
- **git.** It fetches and pushes over HTTPS with a fresh token for each operation:
  - `git@github.com:` remotes are rewritten to HTTPS;
  - the user's own credential helpers, such as the macOS keychain, are switched off in that shell.

  This needs git 2.31 or newer for `GIT_CONFIG_COUNT`.
- **gh.** It uses `GH_TOKEN`, which expires after an hour. Run `env` again for longer sessions.
- **Other commands.** `xtask/agent-token.sh` alone prints one token, and `jwt` prints the app's own JWT for debugging.

## 7. Check that it works

```sh
GH_TOKEN=$(xtask/agent-token.sh) gh api installation/repositories --jq '[.repositories[].full_name]'
(eval "$(xtask/agent-token.sh env)"; gh auth status; git var GIT_AUTHOR_IDENT)
gh api "repos/$REPO/rulesets" --jq '.[] | {name, enforcement}'
```

Then try the flow end to end:

1. **Protected path.** As the bot, push a branch that touches a protected path (any path listed in `.github/CODEOWNERS`) and open a PR. It must show "Review required" from the code owner, and must not enter the queue until that review is given.
2. **Unowned path.** A PR touching only unowned paths, for example `crates/kernels/src/`, must enter the queue once `gate` and `lints (UI tests)` pass, with no review.
3. **Direct push.** A direct `git push origin main` by the bot must be rejected.
