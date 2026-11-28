# ADR-0032: Agents run in a sandbox

- Status: Proposed
- Date: 2026-11-28
- Deciders: maintainer
- Amends: ADR-0010

## Context

ADR-0010 puts the sandbox in phase 2: "Docker/OrbStack on the Mac, or Linux VMs/runners. This is decided when phase 2 starts." Until then, phase 1 sessions ran as the maintainer's own user, with all of `$HOME` in reach. The rules said agents act only as `nami-agent[bot]`, and `.claude/settings.json` makes git do so, but nothing kept the maintainer's credentials out of reach.

That was a real gap. On 2026-11-26 a shell heredoc ended early, and bash ran runbook text as commands (#69). One of them was `gh variable set`, which most likely ran with the maintainer's stored gh login.

PR #72 hides one credential: the gh login, through `GH_CONFIG_DIR` and `DBUS_SESSION_BUS_ADDRESS` in the settings, plus a session-start check. Hiding credentials one at a time is brittle:

- **The check cannot stop a session.** Claude Code ignores `continue: false` from a SessionStart hook (#75).
- **Other launchers do not get the variables** (#71).
- **Much else stays in reach** (#78):
  - the SSH key and agent (an `ssh://` remote escapes the `insteadOf` rewrite);
  - writes to the main checkout's `.git/hooks` and `.git/config`, which later run or apply as the maintainer;
  - the orchestrator's state, with its keypair;
  - other processes' environments;
  - the user session bus and its keyring.

Each of these needs its own hiding, and the next one to turn up would too.

## Decision

### 1. In phase 1, every agent session runs in a sandbox

On the maintainer's machine, every Claude Code session in a nami checkout runs inside `nami-claude`, interactive sessions and the orchestrator's agents alike. `nami-claude` is a wrapper that starts `claude` under bubblewrap (`bwrap`):

- **What the session sees:**
  - an empty `$HOME`;
  - its own PID, IPC and UTS namespaces;
  - `/usr` and `/etc`, read-only.
- **What is mounted back:**
  - the checkout and its common git dir, read-write;
  - that git dir's `hooks` and `config`, read-only on top;
  - the Rust toolchains and the compile cache;
  - Claude Code's login and session files, with the user-level settings file read-only;
  - the Claude Code install, read-only, with the auto-updater off;
  - the `nami-agent` key, read-only.
- **What is removed:**
  - the SSH agent, `GH_TOKEN` and `GITHUB_TOKEN` are unset;
  - the session bus is disabled.
- **Not mounted:**
  - the SSH key, the gh login and `~/.gitconfig`;
  - the orchestrator's state;
  - other checkouts;
  - the rest of `$HOME` and of `/run/user`.

The orchestrator (Paseo) launches agents through a provider that runs the wrapper. A shell alias does the same for terminal sessions.

### 2. A tripwire denies sessions outside the sandbox

The wrapper creates the marker `/run/nami-agent-sandbox`. A session outside the sandbox cannot create it, because `/run` belongs to root.

A `PreToolUse` hook with no matcher, in the user-level `~/.claude/settings.json`, denies every tool call when both of these hold:

- the project is a checkout of `namicfd/nami`;
- the marker is missing.

The reason it gives says how to restart inside the sandbox. In any other project the hook allows the call at once. Inside the sandbox, both the settings file and the tripwire's script (in `~/.local/share/nami-sandbox/`, with the wrapper) are mounted read-only, so a session can neither remove its tripwire nor change it. This holds although `~/.claude` itself is mounted read-write.

### 3. The repository does not depend on it

The wrapper, the tripwire and the provider live on the maintainer's machine, outside the repository.

- A clone elsewhere, or a contributor's machine, behaves as before.
- The repository's own guardrails work the same inside and outside the sandbox:
  - the settings `env` that makes git act as the bot;
  - `xtask/agent-token.sh`;
  - the hooks of ADR-0019.

### 4. It stops mistakes, not attackers

The sandbox runs as the same uid, on a shared network.

- **Still reachable:** localhost services and abstract sockets.
- **Inside by design:** the bot key and the Claude Code login.

So the sandbox keeps an honest mistake, such as a heredoc run as commands or a missing `GH_TOKEN` prefix, away from the maintainer's credentials. It is not a boundary against deliberate misuse. In particular, the `nami-agent` key is in reach by design: ADR-0023 keeps it on the agents' machines, never in CI. What limits its misuse is on GitHub, not on this machine: the `main` ruleset and the merge queue (ADR-0009), and code-owner review of protected paths (ADR-0015). ADR-0023 keeps the CI apps' keys (`nami-ci`, `nami-reviewer`) in CI only, so they are out of agents' reach.

### 5. Phase 2 is unchanged in shape

ADR-0010's phase-2 runner still needs a real boundary. That means a separate user or rootless podman, with a token broker in place of a key file in reach. This ADR does not decide which; ADR-0010 leaves that decision to the start of phase 2.

## Enforcement

- **On the maintainer's machine:** the tripwire (point 2) denies every tool call of a nami session that runs outside the sandbox.
- **In the repository:** none. By point 3, nothing in the repository can check a setup that lives outside it.
- **Tests:** the setup was tested by hand on 2026-11-28, and the results are recorded in #78. Re-run that list after any change to the wrapper.

## Consequences

- **#72's approach is not pursued.** By the maintainer's decision of 2026-11-28, PR #72 and issue #71 were closed, and #75 was closed as redundant, ahead of this ADR's acceptance. Two parts of #72 are kept:
  - the rule that agents write files with the Write tool, never with a heredoc (enforced by #74);
  - its notes on the order in which gh looks for a token.
- **Worktrees live inside the checkout** (`.claude/worktrees/`). A worktree made elsewhere under `$HOME` lands on the sandbox's tmpfs and vanishes with the session.
- **Git cannot write the common config.** For example, `git worktree add -b` cannot record an upstream, and `git push -u` cannot either. Push with an explicit remote and branch.
- **Stale paths fail loudly.** An `xtask` binary compiled with another worktree's path in it (#30) now fails, instead of silently running that worktree's rules.
- **The cost is small.** `just check dylint` ran in 42 s in a fresh worktree, with 105 of 107 compiles served from the shared cache.
- **It is Linux only.** bubblewrap does not exist on macOS, which ADR-0010 assumed for the maintainer's machine. A macOS machine has neither the sandbox nor the tripwire, so nothing there would stop an unsandboxed nami session. Agent sessions in nami are therefore not run on macOS until it has an equivalent sandbox and tripwire (a separate user, or a VM), which a later ADR would record.

## Alternatives considered

- **Hide each credential in turn (#72).** Rejected: each newly found credential needs its own fix, and the check could not stop a session (#75).
- **A separate user for agents.** Deferred to phase 2. It is a stronger boundary, but the toolchains, the compile cache and the orchestrator's integration all need shared ownership, which is more set-up than phase 1 warrants.
- **Rootless podman or a VM.** Deferred to phase 2 for the same reason. They also make the orchestrator's live view of the session harder to keep.
- **Claude Code's own sandbox.** Rejected for now: it confines the shell tool's commands, not the whole session. The goal is that nothing in the session can reach the credentials.

## Evidence

- #69: the heredoc that ran `gh variable set` as the maintainer.
- #71 and #75: the gaps in hiding the gh login alone.
- #78: what was in reach, the sandbox's set-up and its test results.
- bubblewrap: [containers/bubblewrap](https://github.com/containers/bubblewrap).
