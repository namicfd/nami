#!/usr/bin/env bash
# Acts as one of nami's GitHub Apps and mints short-lived tokens for it:
# - `nami-agent` (the default): agents push, open PRs and comment as `nami-agent[bot]`, never
#   as the maintainer (ADR-0008). Code-owner review only works if the author of a PR is not the
#   code owner.
# - `nami-ci` (`NAMI_AGENT_APP=ci`): CI jobs file the issues that agents act on as
#   `nami-ci[bot]` (ADR-0023). It never reads the agents' key or falls back to it, and `env`
#   refuses it: its jobs mint one token and use it, and make no commits as the agents.
# In CI (CI or GITHUB_ACTIONS is `true`), only the CI app may act: the agents' key never
# reaches CI. In CI and for the CI app, every token is narrowed and `jwt` is refused.
#
# Usage:
#   xtask/agent-token.sh [token]             print an installation token (valid for 1 hour)
#   xtask/agent-token.sh env                 print shell exports that make git and gh act as the bot
#   xtask/agent-token.sh git-credential get  git credential helper (installed by `env`)
#   xtask/agent-token.sh jwt                 print the app's own JWT (valid for 9 minutes; not
#                                            in CI or for the CI app)
#   xtask/agent-token.sh request             print the token request's body; asks GitHub nothing
#
# In an agent runner:
#   eval "$(xtask/agent-token.sh env)"
#   claude -p "..."
#
# After `env`, every git and gh command in that shell and its children acts as the bot:
#   - commits are authored and committed by nami-agent[bot];
#   - git fetches and pushes over HTTPS with a fresh token each time, also for `git@github.com:`
#     remotes, through the credential helper;
#   - gh uses GH_TOKEN, which expires after an hour. Run `env` again for longer sessions.
#
# Configuration, from the environment:
#   NAMI_AGENT_APP          which app to act as: `agent` (default) or `ci`
#   NAMI_AGENT_KEY          the agents' app's private key
#                           (default: ~/.config/nami/nami-agent.pem)
#   NAMI_AGENT_CLIENT_ID    the agents' app's client ID (default: nami-agent's)
#   NAMI_CI_KEY             the CI app's private key (no default: CI writes it to a file)
#   NAMI_CI_CLIENT_ID       the CI app's client ID (no default: a repository variable)
#   NAMI_AGENT_REPO         owner/name of the repository (default: namicfd/nami)
#   NAMI_AGENT_PERMISSIONS  a JSON object that narrows the token to fewer permissions than the
#                           app has, such as '{"issues":"write"}'. Unset or empty, the token
#                           has all of the app's permissions. That is refused in CI (CI or
#                           GITHUB_ACTIONS is `true`) and for the CI app anywhere (ADR-0023,
#                           rule 2). `{}` narrows nothing and is always refused.
#
# Needs bash, openssl, curl and jq.
set -euo pipefail

fail() {
  echo "agent-token: $*" >&2
  exit 1
}

readonly api="https://api.github.com"
readonly repo="${NAMI_AGENT_REPO:-namicfd/nami}"
readonly permissions="${NAMI_AGENT_PERMISSIONS:-}"
readonly app="${NAMI_AGENT_APP:-agent}"

# True inside a CI run: GitHub Actions sets both variables to `true`.
in_ci() {
  [[ "${CI:-}" == "true" || "${GITHUB_ACTIONS:-}" == "true" ]]
}

# True when the token must be narrowed and no JWT is printed: inside a CI run, and for the CI
# app wherever it runs.
must_narrow() {
  in_ci || [[ "$app" == "ci" ]]
}

case "$app" in
  agent)
    ! in_ci || fail "the agents' app never acts in CI, and its key stays on the agents'" \
      "machines (ADR-0023); a CI job uses NAMI_AGENT_APP=ci"
    readonly key="${NAMI_AGENT_KEY:-$HOME/.config/nami/nami-agent.pem}"
    readonly client_id="${NAMI_AGENT_CLIENT_ID:-Iv23liAQwForBGxIuQZv}"
    ;;
  ci)
    readonly key="${NAMI_CI_KEY:-}"
    readonly client_id="${NAMI_CI_CLIENT_ID:-}"
    [[ -n "$key" ]] || fail "NAMI_CI_KEY is not set; it names the file with the CI app's key"
    [[ -n "$client_id" ]] ||
      fail "NAMI_CI_CLIENT_ID is not set; see docs/operations/github-setup.md, step 8"
    ;;
  *) fail "unknown app '$app' in NAMI_AGENT_APP; use agent or ci" ;;
esac
# The bot's user ID (not the app ID) comes from `gh api 'users/nami-agent%5Bbot%5D' --jq .id`.
readonly bot_name="nami-agent[bot]"
readonly bot_email="360897210+nami-agent[bot]@users.noreply.github.com"

base64url() {
  openssl base64 -A | tr '+/' '-_' | tr -d '='
}

# The app's JWT, signed with its private key (RS256). GitHub allows at most 10 minutes of
# lifetime. `iat` is backdated by 60 seconds to tolerate clock drift.
jwt() {
  [[ -r "$key" ]] || fail "cannot read the $app app's private key at $key"
  local now header payload signature
  now=$(date +%s)
  header=$(printf '{"alg":"RS256","typ":"JWT"}' | base64url)
  payload=$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' $((now - 60)) $((now + 540)) "$client_id" |
    base64url)
  signature=$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -binary -sign "$key" |
    base64url)
  printf '%s.%s.%s\n' "$header" "$payload" "$signature"
}

# github METHOD PATH BEARER [BODY]: one API call. The bearer goes through stdin, not the
# command line, so other processes cannot read it from the process list.
github() {
  local response data=()
  if [[ -n "${4:-}" ]]; then
    data=(--data "$4")
  fi
  # `${data[@]+...}` keeps an empty array legal under `set -u` in bash 3.2 (macOS).
  if ! response=$(printf 'Authorization: Bearer %s\n' "$3" |
    curl --silent --show-error --fail-with-body --request "$1" --header @- \
      --header "Accept: application/vnd.github+json" \
      --header "X-GitHub-Api-Version: 2022-11-28" \
      ${data[@]+"${data[@]}"} "$api$2"); then
    fail "GitHub API $1 $2 failed: $(jq -r '.message // .' <<<"$response" 2>/dev/null ||
      printf '%s' "$response")"
  fi
  printf '%s' "$response"
}

# The body of the token request: this one repository, and the permissions if any are named.
# CI and the CI app must name them, so that no job holds a token with all of an app's
# permissions.
token_request() {
  local request
  request=$(jq -cn --arg name "${repo#*/}" '{repositories: [$name]}')
  if [[ -z "$permissions" ]]; then
    ! must_narrow || fail "CI and the CI app must narrow the token: set" \
      "NAMI_AGENT_PERMISSIONS to the permissions the job needs, such as" \
      "'{\"issues\":\"write\"}' (ADR-0023, rule 2)"
    printf '%s' "$request"
    return 0
  fi
  request=$(jq -c --argjson permissions "$permissions" \
    'if ($permissions | type) == "object" then . + {permissions: $permissions} else error end' \
    <<<"$request" 2>/dev/null) ||
    fail "NAMI_AGENT_PERMISSIONS must be a JSON object, such as '{\"issues\":\"write\"}'"
  [[ $(jq '.permissions | length' <<<"$request") -gt 0 ]] ||
    fail "NAMI_AGENT_PERMISSIONS is {}, which narrows nothing; name the permissions" \
      "the token needs, such as '{\"issues\":\"write\"}'"
  printf '%s' "$request"
}

# An installation token, scoped to this one repository.
token() {
  local app_jwt installation request
  request=$(token_request)
  app_jwt=$(jwt)
  installation=$(github GET "/repos/$repo/installation" "$app_jwt" | jq -r '.id')
  [[ "$installation" =~ ^[0-9]+$ ]] || fail "unexpected installation ID '$installation'"
  github POST "/app/installations/$installation/access_tokens" "$app_jwt" "$request" |
    jq -re '.token'
}

# The git credential helper protocol: answer `get` for https://github.com, ignore the rest.
# Tokens are minted on demand and never stored, so `store` and `erase` have nothing to do.
git_credential() {
  [[ "${1:-}" == "get" ]] || return 0
  local line host="" protocol=""
  while IFS= read -r line && [[ -n "$line" ]]; do
    case "$line" in
      host=*) host=${line#host=} ;;
      protocol=*) protocol=${line#protocol=} ;;
    esac
  done
  [[ "$protocol" == "https" && "$host" == "github.com" ]] || return 0
  printf 'username=x-access-token\npassword=%s\n' "$(token)"
}

# Exports for a shell whose git and gh act as the bot. The empty `credential.helper` first
# clears the helpers from the user's own config (for example osxkeychain), so the maintainer's
# credentials are never used.
env_exports() {
  [[ "$app" == "agent" ]] ||
    fail "'env' makes git commit as nami-agent[bot], so it is for the agents' app only"
  local self gh_token
  self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
  gh_token=$(token)
  cat <<EOF
export GH_TOKEN='$gh_token'
export GIT_AUTHOR_NAME='$bot_name' GIT_COMMITTER_NAME='$bot_name'
export GIT_AUTHOR_EMAIL='$bot_email' GIT_COMMITTER_EMAIL='$bot_email'
export GIT_CONFIG_COUNT=3
export GIT_CONFIG_KEY_0='credential.helper' GIT_CONFIG_VALUE_0=''
export GIT_CONFIG_KEY_1='credential.helper' GIT_CONFIG_VALUE_1='!"$self" git-credential'
export GIT_CONFIG_KEY_2='url.https://github.com/.insteadOf' GIT_CONFIG_VALUE_2='git@github.com:'
EOF
}

case "${1:-token}" in
  token) token ;;
  env) env_exports ;;
  git-credential) git_credential "${2:-}" ;;
  jwt)
    # The JWT mints tokens with every permission of the app, so it never leaves the script
    # where tokens must be narrowed.
    ! must_narrow || fail "'jwt' is refused in CI and for the CI app: a JWT can mint a token" \
      "with every permission of the app (ADR-0023, rule 2); use 'token' with" \
      "NAMI_AGENT_PERMISSIONS"
    jwt
    ;;
  request) token_request && echo ;;
  *) fail "unknown command '$1'; use token, env, git-credential, jwt or request" ;;
esac
