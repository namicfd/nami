# ADR-0011: Task tracking

- Status: Accepted (amended by ADR-0017)
- Date: 2026-11-24
- Deciders: maintainer

## Context

Parallel agents need three things from task tracking: a list of work that is ready to start, a way to claim a task atomically, and a dependency graph. The options as of 2026-11:

- **Beads (v1.3.4)** requires Dolt. Concurrent agents need a running `dolt sql-server`, and the project has churned (it moved repos, and its SQLite backend was removed in 2026-03).
- **Claude Code's shared task list** is local-machine state in `~/.claude/tasks/`, outside git.
- **GitHub Issues** cannot be claimed atomically, depends on the network and rate limits, and would put swarm chatter in a public repo (ADR-0009).
- **The C compiler** claimed tasks with lock files pushed through git. Git rejects a non-fast-forward push, so the first push wins and claims are atomic for free.

Anthropic's long-running-agent pattern keeps a JSON feature list where agents may only flip `passes`. They chose JSON because models tamper with it less than with Markdown.

## Decision

1. **Plans** live in `docs/exec-plans/active/<plan-slug>/`:
   - `plan.md`: a short design note linking the ADRs and equations it relies on;
   - `tasks.json`: the task list.

   Finished plans move to `docs/exec-plans/completed/`.
2. **`tasks.json` schema** (initial):
   ```json
   {
     "tasks": [
       {
         "id": "euler1d-hllc",
         "title": "HLLC Riemann solver for 1D Euler",
         "depends_on": ["euler1d-exact-reference"],
         "owner_paths": ["crates/kernels/src/riemann/hllc.rs"],
         "acceptance": ["reference::euler1d::toro_tests_1_to_5", "reference::euler1d::hllc_convergence"],
         "passes": false
       }
     ]
   }
   ```
   - `owner_paths` declares which files a task may touch. The planner keeps concurrently ready tasks disjoint, which avoids merge conflicts.
   - `acceptance` lists reference test names, never prose.
   - **Only CI may set `passes` to `true`**, after the acceptance tests are green on `main`.
3. **Claims** are files `claims/<task-id>` on a dedicated `claims` branch. Each holds the agent id, the session id and a UTC timestamp. Claiming means pushing that file, so a second claim fails as a non-fast-forward push. A claim older than its timeout (initially 2 hours) can be released by the runner.
4. **CLI:** `cargo xtask task ready | claim <id> | release <id> | status`. It has no dependencies beyond std and git, and an agent may implement it.
5. **GitHub Issues** is for human input only: bug reports, feature requests and the issues opened by the auto-revert bot. The planner turns issues into plan tasks, so agents have exactly one source of truth for work.
6. **Phase 1 (ADR-0010):** the interactive lead may use Claude Code's built-in task list for the session, but plans and `tasks.json` are still written for anything that spans sessions.

## Enforcement

- A CI job validates `tasks.json` against the schema and rejects PRs from agents that set `passes` to `true`. Only the CI bot may flip it.
- A tidy or CI check rejects PRs that touch files outside the claimed task's `owner_paths`. It is advisory at first, then blocking.

## Consequences

- No extra dependencies; plans and status are versioned and reviewable next to the code; it works with any agent vendor and offline.
- We own a small tool, and the dependency graph is only as rich as we make it.

## Alternatives considered

- **Beads.** Rejected. It needs an external stateful Dolt server and the project has churned.
- **GitHub Issues as the agent tracker.** Rejected. Claims are not atomic, it adds network and rate-limit dependencies, and it is public.
- **Claude Code's task list only.** Rejected. It is local-machine state outside git, and it is not vendor-agnostic.

## Evidence

- Beads storage and claiming: [gastownhall/beads](https://github.com/gastownhall/beads).
- C compiler lock files: [Anthropic](https://www.anthropic.com/engineering/building-c-compiler).
- JSON feature list with `passes` flags: [Anthropic, effective harnesses](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents).
