# Execution plans

Work that spans more than one session lives here, as plans with task lists (ADR-0011).

- `active/<plan-slug>/plan.md`: a short design note that links the ADRs and equations it relies on.
- `active/<plan-slug>/tasks.json`: the task list, in the schema of ADR-0011. A task names the files
  it may touch (`owner_paths`) and the tests that prove it (`acceptance`). Only CI sets `passes`.
- `completed/`: finished plans, moved here unchanged.

Agents take their work from an active `tasks.json` and touch only that task's `owner_paths`
(`AGENTS.md`, Workflow). Unfinished work is recorded here as a task, never as a marker in the code.

Tidy ties this directory to the rule registry: every planned mechanism in
`xtask/src/tidy/registry.rs` must have a task with the same id in an active plan (rule
`planned-task`, see `docs/RULES.md`).
