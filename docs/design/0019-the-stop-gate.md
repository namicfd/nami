# ADR-0019: The stop hook is a gate, and sessions start with working tools

- Status: Proposed
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0006, ADR-0015

## Context

ADR-0015 keeps two Claude Code hooks: a format-and-tidy pass after each edit, and `just check` when an agent stops. `AGENTS.md` told agents "You cannot finish while it fails". A review on 2026-11-25 tested the stop hook and found that it was not a gate:

- **It skipped committed work.** The hook ran only when `git status --porcelain` showed changes. An agent that commits before stopping, which is the documented workflow, was never checked.
- **The second stop always passed.** When the payload said `stop_hook_active`, the hook returned success at once, to avoid trapping an agent. A failing tree was therefore reported once and then let through.
- **It did not run the recursion lint.** The hook ran `just check`. ADR-0006 puts dylint in the stop hook, but the lint only ran in `just check-full`, so recursive code passed the hook. "No recursion" is the first non-negotiable.

Two smaller problems slowed every session down:

- **`just` and `cargo` were not on `PATH`** in the agent's shell on the maintainer's machine. The hooks set `PATH` for themselves, but the documented `just check` failed with "command not found".
- **Tool versions were pinned only in the CI action.** Nothing compared the local `just`, `cargo-nextest`, `cargo-deny`, `cargo-mutants` and `cargo-dylint` with the pinned ones.

## Decision

### 1. When the stop gate runs

The hook fingerprints the working state: the git tree of every tracked file and every untracked file that is not ignored, computed with a scratch index so that the real index is untouched. It skips the gate only when that fingerprint equals:

- the tree of the merge base with `main` (nothing differs from `main`); on the branch `main` itself the base is `origin/main`, so that commits made directly on `main` are still checked; or
- the fingerprint of the last run that passed (nothing changed since).

Otherwise it runs. Committing no longer hides work, and an unchanged green tree is not checked twice. If git cannot produce a fingerprint, the gate runs.

### 2. What it runs

`just check dylint`: the fast gate plus the compiler-level lints. `just check` itself stays the under-60-second command for iterating (ADR-0009).

### 3. Three attempts, then a loud failure

- A failing gate blocks the stop and returns the tail of the output to the agent, up to **three times in a row** per session.
- On the next failure the hook stops blocking. It tells the human, in a non-blocking hook error, that the gate still fails, that the work is not done and that CI will reject it.
- A passing run resets the count, and so does a stop that needs no gate. The count is kept per session, so it means failed stops in a row.

The limit exists because an agent can face a failure it cannot fix, for example a broken toolchain, and a hook that never lets go wastes the session's budget (ADR-0012).

### 4. Sessions start with working tools

A `SessionStart` hook:

- adds `$HOME/.cargo/bin` to the session's `PATH` when it is missing, by appending one `export` line to the file that `CLAUDE_ENV_FILE` names;
- runs `cargo xtask doctor` and prints what it finds into the agent's context.

`cargo xtask doctor` compares each installed tool with the version pinned in `.github/actions/setup/action.yml`, and checks that the nightly pinned in `lints/rust-toolchain.toml` is installed. The CI action stays the only place where versions are written down.

### 5. The edit hook covers more than Rust

After an edit, the hook runs every tidy rule that can be decided from the one file. Besides `.rs` files that now includes crate manifests, ADRs, learnings and `TIGER_STYLE.md`. An agent learns about a banned dependency or a malformed ADR when it writes it, not when it stops.

Edits made through the shell are not seen by this hook. The stop gate catches them.

## Enforcement

- `.claude/settings.json`: the `SessionStart`, `PostToolUse`, `Stop` and `SubagentStop` hooks, all calling `cargo xtask hook <event>`.
- `xtask/src/hook.rs`, with unit tests for the decisions (run or skip, block or give up) and an integration test for the fingerprint on a temporary repository.
- `xtask/src/doctor.rs`, with a test that parses the real CI action, so that a change in its format fails a test.
- Hooks remain defence in depth. The server-side gate is authoritative (ADR-0009).

## Consequences

- `AGENTS.md` can say truthfully what happens at stop: the gate runs unless the tree equals `main` or the last green run, and it blocks three times.
- A stop after real work costs the gate's run time: about 2 seconds on a warm tree today, and about half a minute in a fresh worktree, where the lint library is built once. Under load, a cold run in a fresh clone took 114 seconds; the hook's timeout is 600 seconds.
- A session that only reads a branch with someone else's failing commits is blocked three times for work it did not do. This is accepted: the alternative is guessing whose changes they are.
- The fingerprint writes loose objects into the git object store. `git gc` removes them.
- The state lives in `target/stop-gate/`, which is ignored by git and local to each worktree.

## Alternatives considered

- **Run the gate at every stop.** Rejected. Most stops in an interactive session follow a question, not a change.
- **Compare with `HEAD` only (the old behaviour).** Rejected. It is the hole described above.
- **Never give up.** Rejected. The session would burn its budget on a failure it may not be able to fix, and the research report notes that Claude Code overrides a stop hook after repeated blocks in any case.
- **Put dylint into `just check`.** Rejected. It would add a nightly toolchain and a cold build of the lint library to the command agents run most. The stop gate and CI are enough.
- **Set `PATH` in `.claude/settings.json`.** Rejected. The `env` block cannot expand `$HOME`, so the value would be one machine's path.
- **A second file with tool versions.** Rejected. Two lists drift. The doctor reads the CI action.

## Evidence

Review of 2026-11-25, run against a scratch clone:

| Case | Old hook |
|---|---|
| Dirty tree with a directly recursive function | exit 0: `just check` passes, the recursion lint did not run |
| Dirty tree with a `while` loop, first stop | exit 2 (blocked) |
| The same tree, second stop (`stop_hook_active`) | exit 0 |
| Clean tree after a commit | skipped (`has_changes` read `git status --porcelain`) |

The new hook, run by hand against a scratch clone with one session id:

| Case | New hook |
|---|---|
| Clean tree at the base | exit 0, gate not run |
| A directly recursive function, committed, tree clean | exit 2, "blocked stop 1 of 3": `just check` passes and `cargo dylint` fails |
| The same tree, second and third stop | exit 2, "2 of 3" and "3 of 3" |
| The same tree, fourth stop | exit 1, "STOP GATE STILL FAILING", count cleared |
| The function removed, committed | exit 0, gate run, fingerprint stored |
| The same tree again | exit 0, gate not run |
| A new untracked file | gate run again |

- `command -v just` and `command -v cargo` printed nothing in the agent's shell. Both are in `$HOME/.cargo/bin`.
- Claude Code hooks: exit code 2 blocks and returns stderr to the agent; `SessionStart` hooks can extend the session's environment through `CLAUDE_ENV_FILE` ([hooks guide](https://code.claude.com/docs/en/hooks-guide)).
