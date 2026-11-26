# ADR-0022: Guardrail rule changes merge on proof

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer
- Amends: ADR-0008, ADR-0015, ADR-0020

## Context

Every path under `xtask/` and `lints/` is code-owned (ADR-0008, ADR-0015). So every PR that adds or tightens a tidy rule or a dylint lint waits for the maintainer, even when it only tightens. ADR-0020 rejected merging tightening-only PRs without review: "whether a change to `xtask/` only tightens cannot be decided mechanically". The housekeeping wave of 2026-11-26 (PRs #24, #26, #29, #31, #34) showed both sides of that.

- **Review cannot judge "only tightens" from the diff.** In #31, the reworked `nami_float_reduction` went silent on shapes that the old lint reported, three review rounds in a row: a sum in a `match` scrutinee, in a tuple field, in an `if let` initialiser; then an accumulator update inside a condition or a guard. Each round, the reviewers found these by writing probes and running both lints by hand.
- **A program can check what review checked.** Each of those probes is a case: an input, and what the base reports on it. Run the base's cases through the PR's tools, and the question "does it still report this?" has a mechanical answer.
- **The maintainer is the bottleneck for the safest kind of change.** A rule that fires on more code, with its fixtures, is what ADR-0020 asks agents to write.

The first version of this ADR's mechanism was reviewed on PR #55. Seven findings showed that it proved less than it claimed:

- Rules without cases sat in rule paths: assertion density, the `#[expect]` budget's token walk, and file classification.
- Changes to data (ban scopes, allowed lists) and pure deletions made no mutants.
- Cases sat at fixed paths that rule code could recognise.
- The PR's code ran in the same job as the judge, before the base's corpus was read.
- A missing `missed.txt` counted as nothing missed.
- The PR's own author could satisfy the review gate.

This version answers each of these.

## Decision

### 1. Trusted core and rule paths

The guardrail tooling is split in two.

- Code that decides *what a rule reports on given input* is a **rule path**.
- Code that decides *which files are checked, which kind a file is, how results become exit codes, or how CI and hooks run* is the **trusted core**.
- So is the code of every rule that has no case the ratchet can run.

**Rule paths** merge on proof (point 2). The list is `RULE_PATHS` in `xtask/src/ratchet/scope.rs`. `.github/CODEOWNERS` leaves exactly these paths without an owner, and a test holds the two lists equal.

| Path | What it is |
|---|---|
| `xtask/src/tidy/registry.rs` | The rule, ban and planned tables, written as plain literals so that the base can read them (point 2). |
| `xtask/src/tidy/rust_source.rs`, `tokens.rs`, `text.rs`, `manifest.rs`, `docs.rs`, `style_guide.rs`, `lints.rs` | The source rules: what each one reports on a file. |
| `xtask/fixtures/tidy/` | The tidy corpus. |
| `lints/src/` | The dylint lints, their declarations and their registration. |
| `lints/ui/` | The lint corpus. |
| `docs/RULES.md` | Generated from the registry. |

**Every rule whose code is in a rule path has a base case.** A rule's id appears as a string literal in the module that reports it. The ratchet looks for each registered id in the rule-path modules of both trees. If one appears there and has no base `<id>.fires.fixture`, that is a finding. A core test keeps `main` free of such rules.

**The trusted core** stays code-owned and always needs the maintainer:

- `.github/` (workflows, CODEOWNERS, the setup action), `.claude/` (hooks, settings, agents, the `review-pr` skill), every `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `deny.toml`, `clippy.toml`, `rustfmt.toml`, `.cargo/`, `.config/`, the `justfile`;
- in `xtask/`:
  - the root lookup (`lib.rs`) and the command line (`main.rs`, `tidy/cli.rs`);
  - in `tidy.rs`: the file walk (`is_walked`), file kinds (`ADR_DIRECTORY`, `is_adr_file_name`, `STYLE_GUIDE`, moved there from `docs.rs` and `style_guide.rs`), and scopes;
  - the fixture harness (`tidy/fixtures.rs`, `tidy/scratch.rs`);
  - the hooks, claims, tasks and issues (`hook.rs`, `task/`, `issue/`), `pr.rs`, `doctor.rs`, `json.rs` and `agent-token.sh`;
  - the dylint wrapper (`dylint.rs`), the `#[expect]` budget, and the ratchet itself (`ratchet.rs`, `ratchet/`);
- in `lints/`: the manifest, the lockfile and the pinned nightly;
- the workspace rules, which have no cases:
  - `tidy/assertions.rs` holds assertion density's threshold, its cut-off and its own counting walk, moved out of `rust_source.rs`;
  - `tidy/workspace.rs` holds the `#[expect]` budget, with its own token walk instead of `tokens.rs`'s, and snapshot drafts;
  - `tidy/rules_doc.rs`, and the plan rules in `task/plan.rs`;
- `docs/TIGER_STYLE.md`, `docs/design/`, `docs/operations/`, `AGENTS.md` and `CLAUDE.md`.

### 2. The proof: `cargo xtask ratchet`

The ratchet runs in two parts (point 4).

- **Observe** runs the head's tools on the base's cases and records everything they print.
- **Judge** runs the base's code on the base's corpus, on the head's source read as data, and on those records.

The checks run when the head changes anything under `xtask/` or `lints/`, or a rule path.

| Check | Fails when |
|---|---|
| **Rule set** (read from the head's source with `syn`; no head code runs) | Any of these:<ul><li>a base rule id is missing from the head's `RULES`, or changes kind;</li><li>a base ban row is missing, moves to the other ban table (a path ban and a macro ban ban different things), bans less, changes its scope to anything but `Everywhere`, or has an allowed entry that no base entry covers (allowed lists only shrink);</li><li>a base `(function, check_* callee)` call site in a rule module is gone;</li><li>a base lint at `Deny` or `Forbid` is not declared exactly once at such a level, or is not registered in `register_lints`;</li><li>a rule with code in a rule path has no base case.</li></ul>A table that is not written as plain literals gives no verdict, and so does a form that could hide code from this reading: a `#[cfg]` or `#[cfg_attr]` other than `#[cfg(test)]`, an item macro, an expression macro outside a short list (`assert!`, `format!`, `vec!` and the like), a binding or import that could shadow a table, in the registry, the rule modules or the lint library (where only `declare_lint!`-style item macros are allowed). Calls in test code do not count as call sites. A ban row must name its scope, and a scope the ratchet does not know gives no verdict. |
| **Tidy corpus** | For each base fixture, at each placement, the head's `tidy --file` misses a `(rule, line)` that the base's tidy reports there (firing fixture), or reports one that the base does not (passing fixture). |
| **Lint corpus** | The base's UI cases are compiled with the base's library, in the judging job, and with the head's library, in the observing job under a random package name. A `(lint, line, column, level)` of the base is missing in the head's output. The base's run must first reproduce the base's `.stderr` locations. A run whose exit code disagrees with its error diagnostics (0 beside an error, or an error code without one) gives no verdict, here and end to end. |
| **End to end** | The head's `cargo xtask tidy` misses a firing case planted in a scratch copy of the head tree (regular files only), or the head's `cargo xtask dylint` misses a UI case planted as an example with a random name in a crate drawn from the head tree. This catches changes to the walk and to the wrapper. It does not prove which files a rule applies to; point 3 hardens that. |
| **Mutants** | `cargo mutants --in-diff` over the rule-path diff, per workspace, misses a mutant. The judge reads the listing, `outcomes.json`, `missed.txt` and the exit code. All four must agree, or there is no verdict. A mutant that times out is neither caught nor missed, so any timeout is no verdict, naming the mutants. The summary prints the mutant count. A changed rule-path source that yields no mutant is unproven: a changed constant or a deleted line makes none. |
| **Review** | See point 5. |

**Placements.** Each case is planted at its virtual path and at up to two paths drawn from a seed of the head's content (the fingerprint of the head tree with a fixed, public salt). The same head always gets the same placements, so re-running the check cannot draw new ones until they miss; only a change to the head's content draws again, and that change is in the diff:

- one existing file of the head tree, overwritten;
- one new file with a random name in an existing directory.

A drawn path counts only if:

- it is in the same scope as the virtual path;
- tidy's walk reads it;
- the base's own tidy reports the same `(rule, line)` pairs there.

The judge recomputes the base's reports at each placement itself.

**One head tree.** The observer and the judge read the same files. Both give no verdict for a head with a symbolic link anywhere, so the observer's scratch copy, made of regular files only, holds every file of the head, and the judge, which reads every path component without following links, reads what the compiler compiles. The observer records the head tree's fingerprint, and the judge gives no verdict when its own checkout differs.

**Output and exit code.** Each finding is one stderr line, `place: error[ratchet-<check>]: what; fix`. A Markdown summary goes to stdout. It names the commits judged, the corpus fingerprint, the seed and the mutant counts, and its last line is the verdict. In shadow mode it also shows the verdict that the PR would get with the rule paths unowned, so that the evidence for the flip is the verdict the flip would apply.

- `0`, `ratchet: pass`: every check holds. Also `pass, outside the rule paths`, the neutral outcome: CODEOWNERS decides on the other paths. The ratchet still checked the rule paths.
- `1`, `ratchet: fail`: at least one finding.
- `2`, `ratchet: no verdict` covers these cases:
  - bad arguments;
  - a head xtask that does not build;
  - a tool that cannot be started;
  - a record that is missing, killed, inconsistent or unreadable;
  - a base corpus that is empty or differs from the one observed, or a head tree that differs from the one observed;
  - a head with a symbolic link, or a source form the `syn` reading cannot vouch for;
  - a pull request whose head moved on from the commit judged;
  - something unproven (no mutant, no reviewer app, a skipped check) when no code owner reviews the PR.

  When the base's CODEOWNERS owns a changed path, the owner reviews anyway, and the unproven items become notes. So do they when a user whom the base's CODEOWNERS names has approved the PR's head commit: a rule-only PR that the ratchet cannot vouch for then needs the owner's approval, never the admin bypass.

### 3. Threat model

- **Sound against honest mistakes by agents**, which is what #31 showed: a rule that goes silent on cases written down, a deleted check, a narrowed table, a walk that skips a directory, tests that do not pin the change.
- **Sound against any loosening that can be seen without running head code**: rule ids, ban rows, scopes, allowed lists, `check_*` call sites, lint declarations and registrations. The base reads these from the head's source, so no output of the head's code is involved.
- **The review gate** counts only the bot that `NAMI_REVIEWER_LOGIN` names, matched by account type and exact login, never the authoring app and never a user account with a look-alike name. The review runs in CI (pending #59), where no authoring session holds the reviewer app's key (point 5).
- **Defence in depth only** against head code written to deceive. Such code can recognise inputs, behave differently when observed, or lie in the records it prints, which the judge reads as data. The placements (point 2) are hardening against code that special-cases the planted cases, not proof. The reviewer app (point 5), sampled audits after merge (ADR-0008) and the trusted core, which deceptive code would have to get past, back the ratchet here.

### 4. Isolation in CI

`.github/workflows/guardrails.yml` has two jobs.

- **`ratchet (head)`** checks the base out next to the PR (the test merge's first parent, or the merge group's base). It builds the base's binary under `${RUNNER_TEMP}`, outside every cached path, and runs `cargo xtask ratchet observe`. That builds and runs the head's code. The job uploads only the JSON record. Its steps get no token: checkout uses a read-only one and does not persist it.
- **`ratchet`**, the required check, runs no head code:
  - It checks out the base and the PR as data.
  - It installs the toolchains and the pinned `cargo-dylint` itself, not from a cache that the head job could write.
  - It builds the base's binary under `${RUNNER_TEMP}`.
  - It runs `cargo xtask ratchet judge` with the PR number, the PR's head commit, the commits judged and the reviewer app's login.
  - It reads the base's corpus from its own checkout, so the head job cannot change it. The judge compares the corpus fingerprint with the one in the record. A mismatch or an empty corpus is no verdict.

Both jobs run on `pull_request` and `merge_group`. A review arrives after the push, so `pull_request_review` runs only the judge, as a job also named `ratchet`, in `.github/workflows/ratchet-review.yml`: it reuses the record of the latest run for the same head commit, waiting for it if needed, and cancels only other review runs. No review can start or cancel `ratchet (head)`. `ratchet` runs `if: always()` and is never skipped by a condition, because a skipped job counts as passed. A base without a ratchet skips with a note, as `base gate` does.

In a merge group, the judge diffs the group's head against the group's parent commit (`merge_group.base_sha`), and the summary names that range, so that what was judged can be checked. The reviews and approvals are those of the PR named in the queue's ref, and its head must be the commit in that ref.

### 5. The review gate

The check passes when all of these hold:

- the reviewer app, the bot that the repository variable `NAMI_REVIEWER_LOGIN` names (`nami-reviewer[bot]`, a GitHub App separate from the agents' `nami-agent[bot]`), has reviewed the PR's head commit, and its latest review there is neither dismissed nor a request for changes;
- the PR was not opened by that app;
- every review thread that the app opened is resolved, and resolved by that app.

Everyone else's threads are left to the ruleset, which requires every conversation resolved before merge; they are never a ratchet finding, whoever resolved them. A review of an earlier commit does not count, not even for fixes: the reviewer re-reviews the new head and resolves the fixed threads then. The judge passes the head commit it judged, and gives no verdict when the PR's head has moved on by the time the reviews are read.

**Identity.** An app is matched by its account type and exact login with `[bot]`, never by a login alone, so a user account named `nami-reviewer` is not the reviewer. GitHub logins cannot contain `[`, so only an app has such a login. A code owner's approval counts by that owner's latest review that decided something: a later request for changes, or the dismissal of the approval, cancels it, and any owner's standing request for changes blocks.

**Where the review runs.** The gate counts the reviews that the bot posts, whatever posts them. The reviewer is to run in CI (pending #59), from a workflow that holds the app's key as a secret and runs no PR code, so that no authoring session can post as it. The gate does not depend on that: until the app exists and the variable is set, the review gate gives no verdict, so a rule-path PR needs a code owner's approval of its head commit, as today: the safe default.

### 6. Staging

The ratchet lands in shadow mode first: the core moves, then `ratchet observe` and `ratchet judge` with both CI jobs, not required, with every guardrail path still owned. The CI-hosted reviewer lands separately (#59). The policy flip comes last and alone: this ADR, the owner-less CODEOWNERS lines with their last-match rule, and making `ratchet` a required check. It merges only after the ratchet has given correct verdicts on real PRs in shadow mode.

### 7. A PR that touches both, deliberate loosenings, and the residual risks

- A PR that changes rule paths and owned core paths needs the owner's approval, and the ratchet still runs and must pass.
- A deliberate loosening, approved with an ADR, cannot pass. The maintainer merges it with the ruleset's admin bypass, as for `passes` flips (ADR-0017).
- **Residual risks**, which the AI review and the sampled audits (ADR-0008) must cover:
  - **Rules without cases.** The ratchet proves "not looser on any case written down", not "not looser". A shape that no case shows can go silent. The corpus only grows: a reviewer's probe that finds a loosening becomes a case.
  - **Rule code that special-cases its inputs.** Code that recognises a planted case by its content, or behaves differently under observation, can pass. The placements make this harder, not impossible.

## Enforcement

- `cargo xtask ratchet` (`xtask/src/ratchet.rs`, `xtask/src/ratchet/`), with unit tests and `xtask/tests/ratchet.rs`. That test runs it against fake heads:
  - a loosened rule, at drawn placements too;
  - a rule that fires on correct code;
  - a walk that skips `crates/`;
  - a narrowed ban scope, a wider allowed list, a removed rule, a ban moved to the other table, and tables or calls hidden behind `#[cfg]` or a macro;
  - a deleted `check_*` call;
  - a dropped, lowered and twice-declared lint, and a dylint run whose exit code disagrees with its output;
  - changes outside the rule paths;
  - mutation results that are missing, empty or timed out;
  - reviews by the wrong app or by a user named like it, threads resolved by the wrong account, a head that moved, and other people's threads left to the ruleset;
  - a code owner's approval of the head commit standing in for what is unproven, and cancelled by a later request for changes;
  - a corpus changed in the head job, a head changed between the jobs, and a head with a symbolic link;
  - the head build;
  - the command line, including `observe` then `judge`.
- The required `ratchet` check (`.github/workflows/guardrails.yml`, and `.github/workflows/ratchet-review.yml` for reviews; ruleset per `docs/operations/github-setup.md`, step 9; the reviewer in CI, #59).
- `.github/CODEOWNERS`. `xtask/src/pr.rs` refuses owner-less lines outside `RULE_PATHS`, and its tests hold the two equal.
- The test `every_rule_in_a_rule_path_has_a_case` keeps `main` free of rule-path rules without a case.
- **No mechanism:** whether the corpus is rich enough. The reviewer app and the sampled audits judge that.

## Consequences

- A PR that adds fixtures and UI cases, or tightens a rule's code without touching its documentation, merges when its gates pass, the reviewer app has reviewed it, and mutation testing pins it.
- **A pure data change in a rule path yields no mutant, so it needs the owner unless the owner reviews anyway.** This includes a new ban row with its fixtures. A refinement would accept a zero-mutant diff that the `syn` checks fully cover; it is not built.
- **Most new rules still need the owner for now.** A new tidy rule is tagged in `docs/TIGER_STYLE.md`, which is core (#53).
- **Until the reviewer app exists, every rule-path-only PR needs a code owner's approval of its head commit**, as today, but never the admin bypass.
- **Time.** Measured on the maintainer's 32-core machine with warm caches:
  - the ratchet without mutants takes about 30 s for a tidy change and a minute for a lint change, with three placements per case;
  - mutation testing of #34's diff (35 mutants) took about 2 minutes, and of #31's (122 lint mutants) about 6.

  CI runners are slower. The head job has 120 minutes and the judging job 60, which includes installing `cargo-dylint` (several minutes). If lint PRs routinely run long, the mutants move to a sharded job.
- The workspace rules and the lint library's manifests stay with the owner, because nothing in the corpus covers them (#54).
- A head tool that crashes on a base case gives no verdict, so a flaky crash shows as a red ratchet.

## Alternatives considered

- **Keep every rule change with the owner (ADR-0020).** Rejected for rule paths. It spends the maintainer's attention on the changes the corpus can check, and #31 showed that reading the diff misses what running the cases finds.
- **Run the PR's own ratchet, or judge in the job that runs the PR's code.** Rejected. A PR could weaken or tamper with the check that judges it (PR #55's review).
- **Ban tables in core.** Rejected. A new ban is the cheapest guardrail (ADR-0018), and the base can read the tables as data.
- **Generate cases from each ban row's scope and allowed boundary.** Not built. Reading the rows with `syn` and requiring them only to narrow covers the same changes more directly.
- **The authoring bot as reviewer.** Rejected (PR #55's review): the author could approve itself.
- **Compare the head's output on the whole tree with the base's (the base gate).** Kept as advisory. It sees only the code that exists, not the cases written for rules.
- **A label such as `tightens`.** Rejected. The bot can set labels, and workflow tokens are read-only (ADR-0020).
- **Parse the lint names out of the `.stderr` files.** Rejected. rustc names a lint only at its first diagnostic in a file, so the base's library is run instead and checked against the `.stderr` files.

## Evidence

**#31 replayed.**

- **Base:** this branch's tree with #31's five files as they were before #31 (`4810c28`).
- **Head:** the base plus #31's files at an intermediate commit, merged three-way where `main` had moved since #31 branched.
- **Run:** `--no-review --no-mutants`, so a run without findings ends in "no verdict" rather than "pass".

| Head | Base corpus as written | Base corpus plus the reviewers' probes |
|---|---|---|
| `339145d` (review round 2) | no finding | fail: 32 lint-corpus and 32 end-to-end findings |
| `64c0b9f` (round 3) | no finding | fail: 23 and 23, e.g. `float_reduction_probes.rs:207:19`, an accumulator update inside a compared block |
| `9c92acc` (round 4) | no finding | fail: 14 and 14 |
| `e5fb6e7` (merged) | no finding | no finding |

"The reviewers' probes" is the UI file that #31 finally merged with, added to the base as a case, with the `.stderr` that the base's library produces.

- **With the base's own corpus, no intermediate commit is caught.** The shapes that went silent were not written down; this is the residual risk in point 7.
- **Once the probes are written down,** each round's regressions are found at the lines the reviewers found by hand, and the merged lint passes.
- **Mutation testing of the merged lint** leaves 21 of 122 mutants missed (92 caught, 9 unviable), for example `replace is_float_mul_add -> bool with true`. Under this ADR, #31 would have needed more cases.

**PR #55's exploits replayed** against this version (head = this branch with one change, `--no-review --no-mutants`):

| Exploit | Before | Now |
|---|---|---|
| Assertion density's threshold lowered to 5/3 | pass | The code is in `tidy/assertions.rs`, core and owned. The verdict is `pass, outside the rule paths`, with the code owner reviewing. |
| `direct-libm` scope `Everywhere` → `NamiCrates` | pass | fail: rule set, "narrows its scope" |
| An allowed entry added to `debug-assert-placement` | pass (by reading) | fail: rule set, "is allowed in [\"crates/solver\"] as well", plus corpus and end-to-end findings |
| `check_float_equality` call deleted | pass (by reading) | fail: rule set, "`check_body` no longer calls `check_float_equality`", plus 30 corpus and end-to-end findings |
| Base fixtures deleted in the head job before observing | pass (by reading) | no verdict: "the observations do not cover the base's corpus" |

**#34 replayed with mutation testing** (first version): pass, 35 mutants, 33 caught and 2 unviable.

**A bug the first replay found in the ratchet itself.** With one target directory for both lint libraries, cargo considered the scratch package fresh after the library changed. It replayed the base's diagnostics for the head, so the lint corpus passed a loosening head. Each side now builds in its own target directory, with the library's fingerprint in `DYLINT_RUSTFLAGS` (ADR-0018, point 9).
