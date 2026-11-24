# ADR-0009: Hosting and merge gate

- Status: Accepted
- Date: 2026-11-24
- Deciders: maintainer

## Context

- **Merge queue availability.** GitHub's native merge queue is available only for repos owned by an organisation: public repos, or private repos on Enterprise Cloud. Personal-account repos do not get it.
- **Runner cost.** Actions minutes (including macOS) are free for public repos. Private repos pay, and macOS minutes are billed at 10× the Linux rate.
- **Lessons from other swarms.** Lenient gating at swarm scale produced a red `main` that did not compile (Cursor's FastRender). Anthropic's C compiler saw frequent regressions until CI was added. In a swarm, `main` is shared state: every agent rebases onto it, so a broken `main` stalls or misleads all of them at once.
- **Throughput.** The throughput of a serialised queue depends on how long the blocking gate takes, not on how many checks it contains.

## Decision

### Hosting

1. nami is a **public repo under a free GitHub organisation**.
2. Agents act through a **GitHub App** with least privilege: branch pushes and PR creation. It cannot bypass rulesets.

### Ruleset on `main`

- PRs only, linear history.
- Merge queue required.
- Required checks = the blocking gate.
- Code-owner review (ADR-0008).

### Blocking gate

The target is about 5 minutes, on Linux x86_64. Every check must pass on the rebased result:

- `cargo fmt --check`, tidy, clippy, dylint, build (`--locked`, `release` and `release-checked`);
- `cargo nextest run` (unit and property tests) plus `cargo test --doc`;
- the fast oracle tier: invariants, symmetry, same-seed determinism, coarse convergence, exact Riemann at low resolution;
- `cargo mutants --in-diff` on the PR's changes, **capped at a fixed time budget**. It is advisory until it reliably fits the budget, then blocking with zero surviving mutants.

### No retries for test failures

- Retries are off (`nextest` `retries = 0`).
- A test that fails intermittently is a bug: the code is deterministic by ADR-0005. A human quarantines it.
- Only infrastructure failures (lost runner, network failure during fetch) are retried, automatically and once.

### Batching

The merge queue tests PRs in groups and bisects only when a group fails. The initial maximum group size is 5.

### Post-merge checks (non-blocking)

These run on `main` or nightly:

- the macOS arm64 vs. Linux x86_64 determinism hash comparison;
- full high-resolution convergence studies;
- full mutation testing;
- `bolero` fuzzing and the seeded PDE simulation;
- Gungraun benchmarks.

When a post-merge check fails, a bot **bisects to the merge that caused it, reverts it automatically and opens an issue**. Nobody fixes forward on a broken `main`.

### Local mirror of the gate

- `just check` mirrors the cheap part of the gate locally (under 60 seconds).
- Local pre-commit hooks are a convenience only; agents can skip them with `--no-verify`. The server-side gate is authoritative.

## Enforcement

- GitHub rulesets and merge-queue settings live in `.github/`. They are documented and code-owned, and changes to them go through an ADR.
- A CI workflow owns the auto-revert bot.
- `just check` and the CI blocking job run the same commands, defined once.

## Consequences

- `main` stays green, and a regression found after merge is reverted mechanically.
- With a 5-minute gate and groups of 5, the queue handles about 60 merges per hour, more than the pilot swarm (ADR-0012) will produce.
- If the gate grows past its time budget, checks move to post-merge through an ADR amendment. We do not add retries.
- The code is public from the first commit.

## Alternatives considered

- **Minimal gates plus automatic retries.** Rejected after discussion. Retries hide nondeterminism bugs and buy little throughput when tests are deterministic. Fixing forward fails in a swarm because a broken `main` is shared by every agent.
- **Fully strict, with all slow checks blocking.** Rejected. Every merge would wait for the slowest check.
- **A private repo on Enterprise Cloud.** Rejected because of cost.
- **A local serial merge script with GitHub as a mirror.** Rejected. We would have to build human-review routing and cross-platform runners ourselves, and the queue would stop whenever the machine is off.

## Evidence

- Merge-queue availability: [GitHub docs](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue).
- FastRender with a red `main` and 34 compile errors: [The Register, 2026-01-26](https://www.theregister.com/2026/01/26/cursor_opinion/).
- C compiler regressions before CI: [Anthropic](https://www.anthropic.com/engineering/building-c-compiler).
- 27.67% of simulated merges of agent PRs conflicted: [arXiv 2604.03551](https://arxiv.org/abs/2604.03551).
- `cargo-mutants --in-diff`: [mutants.rs](https://mutants.rs/in-diff.html).
