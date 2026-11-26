#!/usr/bin/env bash
# Acts as one of nami's GitHub Apps and mints short-lived tokens for it (ADR-0008):
# - `nami-agent` (the default): agents push, open PRs and comment as `nami-agent[bot]`, never
#   as the maintainer. Code-owner review only works if the author of a PR is not the code owner.
# - `nami-reviewer` (`NAMI_AGENT_APP=reviewer`): the AI review is posted as `nami-reviewer[bot]`,
#   an identity that authoring sessions do not hold (pending ADR-0022, #55).
#
# Usage:
#   xtask/agent-token.sh [token]             print an installation token (valid for 1 hour)
#   xtask/agent-token.sh env                 print exports that make git and gh act as the bot
#   xtask/agent-token.sh git-credential get  git credential helper (installed by `env`)
#   xtask/agent-token.sh jwt                 print the app's own JWT (valid for 9 minutes)
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
#   NAMI_AGENT_APP           which app to act as: `agent` (default) or `reviewer`
#   NAMI_AGENT_KEY           the agents' app's private key
#                            (default: ~/.config/nami/nami-agent.pem)
#   NAMI_AGENT_CLIENT_ID     the agents' app's client ID (default: nami-agent's)
#   NAMI_REVIEWER_KEY        the reviewer app's private key
#                            (default: ~/.config/nami/nami-reviewer.pem)
#   NAMI_REVIEWER_CLIENT_ID  the reviewer app's client ID (no default: set it once the app
#                            exists)
#   NAMI_AGENT_REPO          owner/name of the repository (default: namicfd/nami)
#
# The reviewer app posts the AI review that the ratchet would count (pending: #55, #57).
# It only reads, reviews and resolves threads: `env` and `git-credential` refuse it, and it
# never reads the agents' key or falls back to it. Its key lives where authoring sessions cannot
# read it (docs/operations/github-setup.md, step 5b).
#
# Needs bash, openssl, curl and jq.
set -euo pipefail

fail() {
  echo "agent-token: $*" >&2
  exit 1
}

readonly api="https://api.github.com"
readonly repo="${NAMI_AGENT_REPO:-namicfd/nami}"
readonly app="${NAMI_AGENT_APP:-agent}"
case "$app" in
  agent)
    readonly key="${NAMI_AGENT_KEY:-$HOME/.config/nami/nami-agent.pem}"
    readonly client_id="${NAMI_AGENT_CLIENT_ID:-Iv23liAQwForBGxIuQZv}"
    ;;
  reviewer)
    readonly key="${NAMI_REVIEWER_KEY:-$HOME/.config/nami/nami-reviewer.pem}"
    readonly client_id="${NAMI_REVIEWER_CLIENT_ID:-}"
    [[ -n "$client_id" ]] ||
      fail "NAMI_REVIEWER_CLIENT_ID is not set; see docs/operations/github-setup.md, step 5b"
    ;;
  *) fail "unknown app '$app' in NAMI_AGENT_APP; use agent or reviewer" ;;
esac
# The bot's user ID (not the app ID) comes from `gh api 'users/nami-agent%5Bbot%5D' --jq .id`.
readonly bot_name="nami-agent[bot]"
readonly bot_email="360897210+nami-agent[bot]@users.noreply.github.com"

# The reviewer app reviews; it never commits or pushes.
agent_only() {
  [[ "$app" == "agent" ]] ||
    fail "the reviewer app does not commit or push; '$1' is for the agents' app"
}

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

# An installation token, scoped to this one repository.
token() {
  local app_jwt installation
  app_jwt=$(jwt)
  installation=$(github GET "/repos/$repo/installation" "$app_jwt" | jq -r '.id')
  [[ "$installation" =~ ^[0-9]+$ ]] || fail "unexpected installation ID '$installation'"
  github POST "/app/installations/$installation/access_tokens" "$app_jwt" \
    "{\"repositories\":[\"${repo#*/}\"]}" | jq -re '.token'
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
  env) agent_only env && env_exports ;;
  git-credential) agent_only git-credential && git_credential "${2:-}" ;;
  jwt) jwt ;;
  *) fail "unknown command '$1'; use token, env, git-credential or jwt" ;;
esac
