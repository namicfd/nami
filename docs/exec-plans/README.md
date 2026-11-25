# Execution plans

Work is planned here as task lists (ADR-0011). An agent's task comes from a plan in this directory and nowhere else. GitHub Issues are for human input; the planner turns them into tasks.

```
docs/exec-plans/
  active/<plan>/plan.md      design note, plus one `### <task-id>` section per task
  active/<plan>/tasks.json   the task list that tools read
  completed/<plan>/          plans whose tasks all pass
```

## `tasks.json`

```json
{
  "tasks": [
    {
      "id": "advection1d-upwind",
      "title": "First-order upwind flux for linear advection",
      "depends_on": ["advection1d-reference"],
      "owner_paths": ["crates/kernels/src/advection.rs"],
      "acceptance": ["advection1d::upwind_is_first_order"],
      "passes": false
    }
  ]
}
```

The fields are exactly these six, in this order:

| Field | Meaning |
|---|---|
| `id` | Lowercase letters, digits and dashes. Unique across all plans. |
| `title` | One line. |
| `depends_on` | Task IDs that must pass before this one can start. The graph has no cycles. |
| `owner_paths` | The files and directories the task may touch, as clean relative paths. |
| `acceptance` | Names of the tests that must pass on `main`. Test names, never prose. |
| `passes` | `false` until CI sees the acceptance tests pass on `main`. **Only CI sets it.** |

The details of a task live in its `### <task-id>` section in `plan.md`:

- what to build;
- the interfaces to use or provide;
- the equations and their sources;
- what is out of scope.

The JSON holds only what tools read. That gives agents little reason to edit it, and JSON is tampered with less than Markdown.

## Rules

`cargo xtask tidy` (and therefore `just check` and CI) checks every plan:

- **Files and schema:** each plan directory has both files; the schema is exact; every task has its section in `plan.md`.
- **IDs and dependencies:**
  - IDs are unique;
  - dependencies exist, and none is the task itself;
  - there are no cycles;
  - a passing task depends only on passing tasks.
- **Disjoint owner paths:** two unfinished tasks that may run at the same time own disjoint paths. Neither depends on the other, so the PRs would otherwise conflict. Make one depend on the other, or split the path. A directory overlaps everything inside it.
- **Completed plans:** a plan under `completed/` has no unfinished task.

In pull requests and the merge queue, `cargo xtask task check-pr` fails if the change sets `passes` on any task. On a branch named `task/<id>`, it also lists changed files outside that task's `owner_paths`. The task's own plan directory is exempt, so the task can note findings there. That list is advisory for now, and becomes blocking later (ADR-0011).

## Commands

```sh
cargo xtask task status          # every active task: done, claimed, ready or blocked
cargo xtask task ready           # tasks that can start now: `id<TAB>title`, one per line
cargo xtask task claim <id>      # take a ready task
cargo xtask task release <id>    # give it back (done, abandoned or stale)
cargo xtask task validate        # the plan checks on their own
```

`just task <command>` is the same.

Claims are files `claims/<task-id>` on the `claims` branch.
- **Claiming:** a claim pushes a commit to that branch. Git accepts only fast-forward pushes, so when two agents claim the same task at once, exactly one push wins.
- **Holder:** the claim file records `$NAMI_AGENT_ID` (default: git's `user.name`) and `$NAMI_SESSION_ID`.
- **Age:** `status` shows how old each claim is.
- **Stale claims:** nothing releases them automatically yet. A human or the runner (ADR-0010, phase 2) releases claims older than about two hours.

## Writing a plan

- Start with the references (ADR-0007). A reference task comes before the feature it tests, and a different session writes it.
- Keep tasks small: one PR each, reviewable in minutes.
- Give every task acceptance tests that a no-op or half-done implementation fails.
- Order tasks that touch the same file. If many tasks would touch one file (`lib.rs` module lists, for example), add an early scaffold task that creates the modules.
