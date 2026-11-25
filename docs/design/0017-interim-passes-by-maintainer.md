# ADR-0017: The maintainer sets `passes` until CI can

- Status: Accepted
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0011 (point 2, "Only CI may set `passes`")

## Context

ADR-0011 says only CI sets a task's `passes` flag, once the task's acceptance tests pass on `main`. That job does not exist yet, and it needs two decisions:

- **How acceptance names map to test runs.** `<crate>::<module or test file>::<test>` has to become a nextest filter.
- **How the bot commits to `main`.** The `main` ruleset (ADR-0009) blocks direct pushes, and agents have no bypass. CI would need its own identity with a bypass, or its own PR for every flip.

Until then, nothing can set `passes`, so no dependent task ever becomes ready. The first plan, `advection-1d`, is a chain of four tasks.

## Decision

1. **Until the CI job exists, the maintainer sets `passes` by hand,** after checking that the task's acceptance tests pass on `main`.
2. **The flip is a PR of its own.** `cargo xtask task check-pr` fails it, as it fails every change that sets `passes`. The maintainer merges it with the admin bypass. The override is explicit and visible in the PR history.
3. **The PR changes only `tasks.json` files.** The bypass also skips every other required check, so a flip PR must carry no other change.
4. **Agents never set `passes`.** They have no bypass, so `check-pr` stops them.
5. **This ADR is superseded** by the one that introduces the CI job.

## Enforcement

- `check-pr` in the `gate` job rejects any change that sets `passes`.
- The ruleset grants the bypass to the repository-admin role only.
- Review: a flip PR that touches anything besides `tasks.json` is rejected.

## Consequences

- Dependent tasks can start as soon as the maintainer flips their dependencies. Each flip costs a human a minute.
- Until a flip, a merged task stays claimed (`docs/exec-plans/README.md`), so nobody does it twice.

## Alternatives considered

- **Let `check-pr` accept flips from human-authored PRs.** Rejected. It depends on the PR author's identity, and the bot can push to any PR branch.
- **Build the CI job now.** Deferred. The bypass identity for CI needs the maintainer's decision, and phase 1 has a low merge rate.

## Evidence

- ADR-0011 and ADR-0009.
- The first review of PR #2, which found that nothing could set `passes`.
