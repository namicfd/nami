# ADR-0022: Guardrail rule changes merge on proof

- Status: Proposed
- Date: 2026-11-27
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

Four rounds of review of the ratchet itself (#57) then showed where a mechanical proof stops. **Every honest loosening found that week edited existing rule code, mostly together with the tests that pinned it:**

- #31's reworked lint;
- round 3: a changed limit with its unit test edited to match, a private table of exceptions, a guard at the top of `check_path_start`, an exception inlined into its allowed list, a condition added in `visit_path`;
- round 4: a new `Visit` override that skipped a directory, a first-statement guard in `check_tolerance`, a threshold scaled where it is used, `function-length` lifted from the nami crates, `runtime-dependency` spared for `crates/kernels`.

Mutation testing cannot see such a change: it shows that the head's tests pin the head's behaviour, and an edited test pins the edit. Each round added a hold list to the dialect, and the next round found a form the list missed. The independent AI review caught every one of them.

So the maintainer decided (2026-11-26) to sort rule changes into two classes: **additive changes merge on the ratchet's proof alone; edits to existing rule code merge only with the ratchet plus an approval by the CI-hosted reviewer app (#59) on the head commit.** Until that app exists, an edit needs a code owner's approval, as today.

## Decision

### 1. Trusted core and rule paths

The guardrail tooling is split in two.

- Code that decides *what a rule reports on given input* is a **rule path**.
- Code that decides *which files are checked, which kind a file is, how results become exit codes, or how CI and hooks run* is the **trusted core**.
- So is the code of every rule that has no case the ratchet can run.

**Rule paths** merge on proof (points 2 and 3). The list is `RULE_PATHS` in `xtask/src/ratchet/scope.rs`. `.github/CODEOWNERS` leaves exactly these paths without an owner, and a test holds the two lists equal.

| Path | What it is |
|---|---|
| `xtask/src/tidy/registry.rs` | The rule, ban and planned tables, written as plain literals so that the base can read them (point 3). |
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

### 2. Two classes: additive and edit

The ratchet sorts each rule-path diff into one class (`xtask/src/ratchet/class.rs`), reading both trees as data. Rust items are compared token for token, with doc comments left out.

**Additive** means that every change in the rule paths is one of these:

- **(a) New items, new files and modules for a new rule.** A new top-level item of a Rust file, new or existing: a function, a struct, enum, union or trait, a lint or lint pass declared with `declare_lint!`, `declare_tool_lint!` or `declare_lint_pass!`, a test module; in a new file also an import without a glob, and in an existing file a file module `mod name;` for a new file. A new item:
  - carries, on itself and on everything inside it (an `impl` member, a nested function), only the attributes `doc`, `derive`, `must_use`, `inline`, `expect` and `test`, so no `export_name`, `link_section`, `no_mangle`, `used` or other `unsafe` attribute;
  - holds no `unsafe` code at any depth: no `unsafe impl`, `unsafe trait`, `unsafe fn` or `unsafe` block, and no `extern` block;
  - names nothing, a test or test module among it, that reaches the environment, the clock, a process, a thread, the network or the file system (`env`, `option_env`, `time`, `SystemTime`, `Instant`, `process`, `Command`, `thread`, `net`, `fs`, `io`, `workspace_root`, and the `Path` methods that ask the file system, such as `exists` and `metadata`);
  - uses none of `include!`, `include_str!`, `include_bytes!`, `file!`, `env!` and `option_env!`, a test among them. A test that pins its file, as it compiles (`include_str!(file!())`) or as it runs (`std::fs::read_to_string`), kills every mutant of the file without testing a rule (#57's review, rounds 5 and 6);
  - holds no `impl`, trait, module or `macro_rules!` nested inside it, and no constant or static at any depth, associated constants included: like a top-level constant, which is an edit, no mutant tests its value;
  - in an existing file, has a name that is no identifier of the base's file, so that it shadows nothing the existing code names: a prelude item such as `drop`, an import, a glob.

  **A new `impl` is additive only** when it is a top-level item of the same file that defines its new self type; the self type is a single-segment path naming that type; the `impl` has no type or const parameters and no `where` clause; and its lifetime parameters, if any, have no bounds and appear only in the trait path and in method signatures, as in `impl<'tcx> LateLintPass<'tcx> for NewPass` and `impl<'ast> Visit<'ast> for NewVisitor`. The trait may be any trait: an `impl` whose self type is a plain new type of the same file cannot change which method a call on an existing type resolves to. Anything else is an edit: an `impl` of an existing type, an inherent `impl` in another file, a blanket or generic `impl` (`impl<T>`, even when its parameter shadows the new type's name, or one with a `where` clause), and a lifetime used in an associated type, a body or a macro (#57's review, round 5). So a new lint, and a new tidy rule with its own visitor, are fully additive.
- **(b) One dispatch call per new item, at the existing dispatch sites.** A statement inserted at the top level of an existing function's body, in one of these forms:
  - `f(args);`, `module::f(args);` or `Type::f(args);`, calling a function new in the file that the path resolves to, or a function of a type new there;
  - `place.extend(call);`, where `call` calls a new function whose return type is exactly `Vec<Violation>`, and `place` is a report list of the function the call is in, read with `syn` from the base: a parameter of type `&mut Vec<Violation>`, a `self` field declared `Vec<Violation>` in the method's own type, a local declared `Vec<Violation>` (or untyped in a function that returns `Vec<Violation>` and ends in it) that the function returns, or a report field of a returned struct literal, as `rust_source::check` returns `Report { violations: visitor.violations }`. So a dispatch call can only add reports. On any other place, such as an allow-list, an exemption set or a list of enforced lints, it is an edit (#57's review, round 6: `names.extend(vendored(&root))` in `workspace_crates` would have let a nami crate depend on any crate). A place whose type cannot be read so is an edit too;
  - `place.register_lints(&[NEW, ..]);` with new lints only, or the base's registration with new lints appended;
  - `place.register_late_lint_pass(Box::new(|_| Box::new(NewPass)));`, or the early form, for a new lint pass.

  Every argument is a literal or a shared reference to a path or a field of one: no `&mut`, no value that may be a reborrowed `&mut`, no method call, block or closure. So the new code gets no way to change the state that the existing rules read. Each new item is dispatched at most once.
- **(c) New registry rows, and rows that the data checks prove only tighten:** every field that a change touches is one that the rule-set check compares, or one that only documents (point 3).
- **(d) New fixtures and UI cases,** and new items inside an existing test module, whose existing items stay as they are, if they use none of the macros above that read the source tree or the environment. Existing fixtures and UI cases stay byte for byte.
- **(e) Documentation:** the documentation-only fields of a registry row, and doc comments anywhere.
- **(f) The regenerated `docs/RULES.md`.**

**Edit** is everything else in the rule paths:

- a changed or removed function, method, constant, static, type, `impl`, trait, import, macro or test;
- a new member of an existing `impl`, or a new item inside an existing inline module other than the test module;
- an `impl` that is not a plain `impl` of a type new in the same file (above);
- a new constant or static at any depth, associated constants included (mutation testing makes no mutant for its value), a new macro or inline module, an import into an existing module;
- `unsafe` code, an `extern` block, or an attribute outside the allowed ones, anywhere in a new item;
- an `extend` on a place that is not a report list of its function, or with a call that does not return `Vec<Violation>`;
- a registry row change that no data check proves, such as a new planned row or a changed `mechanism`;
- a changed or deleted fixture, UI case or other file.

**What each class needs.**

- **Additive: it merges on the ratchet's proof alone.** Every check of point 3 holds, and no request for changes or thread by the reviewer app is open (point 6). The app need not have reviewed the head, nor exist: an unreviewed additive diff passes. **So additive rule-path PRs merge on the ratchet alone as soon as this ADR's PR (#55) merges, even before the CI-hosted reviewer (#59) exists.**
- **Edit:** every check of point 3 still runs, and its findings still fail the PR. In addition, the review gate needs **an `APPROVE` by the reviewer app on the head commit**, as its latest decision, with every one of its threads resolved by it. Without that, the verdict is "no verdict: edit to existing rule code needs the reviewer app's approval or a code owner's". A code owner's approval of the head commit settles it, as for everything else the ratchet cannot prove. With the app's approval, the approval also stands in for a changed hunk without a mutant of its own, because a test edited together with the code it tests proves nothing about the edit.

Both verdict lines of the shadow summary name the class (`ratchet: pass (additive)`, `ratchet: no verdict (edit), ..`), and the summary lists each change that makes a diff an edit.

### 3. The proof: `cargo xtask ratchet`

The ratchet runs in two parts (point 5).

- **Observe** runs the head's tools on the base's cases and records everything they print.
- **Judge** runs the base's code on the base's corpus, on the head's source read as data, and on those records.

The checks run when the head changes anything under `xtask/` or `lints/`, or a rule path.

| Check | Fails when |
|---|---|
| **Rule set** (read from the head's source with `syn`; no head code runs) | Any of these:<ul><li>a base rule id is missing from the head's `RULES`, or changes kind;</li><li>a base ban row is missing, moves to the other ban table (a path ban and a macro ban ban different things), bans less, changes its scope to anything but `Everywhere`, or has an allowed entry that no base entry covers (allowed lists only shrink);</li><li>a base `check_*` call site in a rule module is gone, keyed by where it is and how it calls: `(Type::method, self.check_x)` or `(function, check_x)`;</li><li>a base lint at `Deny` or `Forbid` is not declared exactly once at such a level, or is not registered in `register_lints`;</li><li>a rule with code in a rule path has no base case.</li></ul>A rule-path file outside the dialect (below) gives no verdict. |
| **Tidy corpus** | For each base fixture, at each placement, the head's `tidy --file` misses a `(rule, line)` that the base's tidy reports there (firing fixture), or reports one that the base does not (passing fixture). |
| **Lint corpus** | The base's UI cases are compiled with the base's library, in the judging job, and with the head's library, in the observing job under a random package name. A `(lint, line, column, level)` of the base is missing in the head's output. The base's run must first reproduce the base's `.stderr` locations. A run whose exit code disagrees with its error diagnostics (0 beside an error, or an error code without one) gives no verdict, here and end to end. |
| **End to end** | The head's `cargo xtask tidy` misses a firing case planted in a scratch copy of the head tree (regular files only), or the head's `cargo xtask dylint` misses a UI case planted as an example with a random name in a crate drawn from the head tree. This catches changes to the walk and to the wrapper. |
| **Mutants** | `cargo mutants --in-diff` over the rule-path diff, per workspace, misses a mutant. The judge reads the listing, `outcomes.json`, `missed.txt` and the exit code. All four must agree, or there is no verdict. A mutant that times out is neither caught nor missed, so any timeout is no verdict, naming the mutants. The summary prints the mutant count. Coverage counts per changed hunk of a rule-path Rust file: each hunk must contain a mutant that starts on one of its lines, or be comments only, or lie in the test module, or be made of dispatch calls, which the new rule's base case shows to run. The registry's tables are read as data, so their hunks need none. Any other hunk, a pure deletion among them, is unproven; for an edit, the reviewer app's approval stands in (point 2). |
| **Review** | See point 6. |

**Registry rows.** Rows are matched by table and id, and every field that a change touches (all of a new or removed row's) must be one that the rule-set check compares or one that only documents:

- **compared:** `id` and `kind` in `RULES`; `id`, `segment`, `allowed` and `scope` in `PATH_BANS`; `id`, `names`, `allowed` and `scope` in `MACRO_BANS`; none in `PLANNED`;
- **documentation only,** printed only in tidy's messages and `docs/RULES.md`: `message`, `summary`, `reference`, `description`, and `detail`, the "What it sees and misses" column (#66).

So a change to the tables alone in those fields (rows added, scopes widened, allowed lists shrunk, messages reworded) is additive and passes with no mutant at all. A change to any other field, such as a planned row added or removed or its `mechanism` changed, is an edit.

**The dialect.** The rule-set check reads Rust as data, which is sound only if the code the compiler builds is the code that was read. So the rule paths are held to a dialect of Rust defined by what it allows (`xtask/src/ratchet/dialect.rs`). A file outside it gives no verdict, so the code owner decides; it never passes. The dialect makes the readings sound; whether existing code stayed as it was is the class's question (point 2). In every rule-path Rust file, no raw identifier (`r#name`) appears anywhere. Beyond that:

- **The registry.** The tables `RULES`, `PATH_BANS`, `MACRO_BANS` and `PLANNED`, each once, written as `&[..]` of struct literals of the row type (of `planned(..)` calls for `PLANNED`). Every value is a plain string literal, `&[..]` of them, or a variant written with its full type path, `BanScope::Everywhere` or `RuleKind::Source`, of a variant the ratchet knows. The tables carry doc comments only, and nothing inside them carries an attribute. The enums, the row types and their code are rule code like any other: a change there is an edit.
- **A rule module.** Attributes only on items, fields and variants, and only `doc`, `derive`, `must_use`, `inline` and `expect`. `#[cfg(test)]` only on an inline `mod tests`, which the readers skip. No other module, no item macro, and no macro but the standard expression macros (`assert!`, `format!`, `vec!` and their like; `concat!` of string literals only). No glob or renamed import, and no local `impl`. A table and the `registry` module are named or imported only as the registry's own (`super::registry`, `crate::tidy::registry`), and nothing else is bound to their names. Every `check_*` name is the definition of a top-level function or of a method in an inherent `impl`, or a call `self.check_*(..)` or `check_*(..)` that resolves to such a definition in the same file; the ratchet counts the names, macro bodies included.
- **The lint library.** Exactly the files that `lints/src/lib.rs` declares with `mod`, and no other `.rs` file below `lints/src/`. No `#[path]`, no `include!` and no `macro_rules!`; `#[cfg(test)]` only on an inline `mod tests`. Each lint is declared at the top of a file with `declare_lint!` or `declare_tool_lint!`, in the form `pub NAME, Level, "description"` with doc comments only; no other macro whose name says `lint` declares one. Nothing else binds a declared name, and the registration is `register_lints(&[NAME, ..])` in `lib.rs`: a statement of its own in the body of the top-level function `register_lints`, with no `return`, `?` or macro in a statement before it. Any other `register_lints` call gives no verdict.

Today's rule paths are in the dialect without changes.

**Placements.** Each case is planted at its virtual path, and at paths drawn from a seed of the base's corpus and the PR's head commit, with a fixed, public salt. The same pair always gets the same placements, so re-running the check cannot draw new ones. The fixture's `// path:` header is replaced by a random comment on the same line, so that rule code cannot recognise a case by it. The paths drawn are:

- one existing file of the head tree, overwritten, in the same scope as the virtual path;
- one new file with a random name in an existing directory, in the same scope;
- for a firing case, one new file with a random name in each scope class of the head tree: each crate (`crates/<name>`, `crates/kernels` among them), each crate's integration tests (`crates/<name>/tests`), `xtask`, `lints`, `tools` and `docs`.

A drawn path counts only if:

- tidy's walk reads it;
- the base's own tidy reports the same `(rule, line)` pairs there.

The judge draws the placements again from the recorded seed, after checking that it is the seed this base and head give, and requires a run at each drawn placement and at no other. It recomputes the base's reports at each placement itself.

**One head tree.** The observer and the judge read the same files. Both give no verdict for a head with a symbolic link anywhere, so the observer's scratch copy, made of regular files only, holds every file of the head, and the judge, which reads every path component without following links, reads what the compiler compiles. The observer records the head tree's fingerprint, and the judge gives no verdict when its own checkout differs.

**Output and exit code.** Each finding is one stderr line, `place: error[ratchet-<check>]: what; fix`. A Markdown summary goes to stdout. It names the commits judged, the class and each edit, the corpus fingerprint, the seed and the mutant counts, and its last line is the verdict. In shadow mode it also shows the verdict that the PR would get with the rule paths unowned, so that the evidence for the flip is the verdict the flip would apply.

- `0`, `ratchet: pass (<class>)`: every check holds. Also `pass (<class>), outside the rule paths`, the neutral outcome: CODEOWNERS decides on the other paths. The ratchet still checked the rule paths.
- `1`, `ratchet: fail (<class>)`: at least one finding.
- `2`, `ratchet: no verdict` covers these cases:
  - bad arguments;
  - a head xtask that does not build;
  - a tool that cannot be started;
  - a record that is missing, killed, inconsistent or unreadable;
  - a base corpus that is empty or differs from the one observed, or a head tree that differs from the one observed;
  - a head with a symbolic link, or a source form the `syn` reading cannot vouch for;
  - a pull request whose head moved on from the commit judged;
  - something unproven (a hunk without a mutant, a skipped check, an edit without the reviewer app's approval) when no code owner reviews the PR.

  When the base's CODEOWNERS owns a changed path, the owner reviews anyway, and the unproven items become notes. So do they when a user whom the base's CODEOWNERS names has approved the PR's head commit: a rule-only PR that the ratchet cannot vouch for then needs the owner's approval, never the admin bypass.

### 4. Threat model

- **Sound against honest mistakes by agents,** which is what #31 and the reviews of #57 showed, within what each class rests on.
- **The `syn` checks are sound for files in the dialect** (point 3): for those, the rule ids, ban rows, scopes, allowed lists, the presence of each `check_*` call site, and the lints' levels and registrations that the base reads from the head's source are what the compiler builds, and no output of the head's code is involved. A rule-path file outside the dialect gives no verdict.
- **An additive diff rests on the corpus, end to end and the data checks.** The existing rule code is the base's, token for token, and the new code can neither shadow it nor reach the state it reads (point 2). So the base's rules report what they reported, and the corpus and end to end show it at every placement; the data checks show that the tables only tighten; and that the new rules fire as their cases say, the new cases (which land in the base first) and mutation testing of the new code show. **Residual risk:**
  - new code that reaches existing state through a path the class does not model, such as interior mutability behind a shared reference, a panic, or a loop that does not end; the corpus, end to end and the sampled audits after merge (ADR-0008) are what stand against these, since an additive diff needs no review;
  - a new rule that fires on too little, or a new case that is weak. That makes the guardrails less strict than they could be, never less strict than the base;
  - a new test that pins its file through an existing helper that reads the tree without naming any of the names above, which would make mutants of the new code look caught. A null-mutant check (apply a change that alters no behaviour, such as an appended comment, and require the suite to pass) would close this; it is not built.
- **An edit rests on the independent reviewer.** The checks still catch what they can: a removed row or call, a lowered lint, a case that no longer fires at a placement, a missed mutant. But how existing rule code applies its rules is code, which the `syn` checks do not read, and the corpus shows it only at the paths where it plants a case. A narrowing that no placement reaches, written with its tests edited along, passes every check; that was each exploit of #57's reviews. The reviewer app's approval of the head commit is what vouches for it. **Residual risk:** a narrowing that the reviewer also misses. The sampled audits after merge (ADR-0008) and the corpus, which only grows, stand against it.
- **The review gate** counts only the bot that `NAMI_REVIEWER_LOGIN` names, matched by account type and exact login, never the authoring app and never a user account with a look-alike name. The review runs in CI (#59), where no authoring session holds the reviewer app's key (point 6).
- **Defence in depth only** against head code written to deceive. Such code can recognise inputs, behave differently when observed, or lie in the records it prints, which the judge reads as data. The placements (point 3) are hardening against code that special-cases the planted cases, not proof. The reviewer app (point 6), sampled audits after merge (ADR-0008) and the trusted core, which deceptive code would have to get past, back the ratchet here.

### 5. Isolation in CI

`.github/workflows/guardrails.yml` has two jobs.

- **`ratchet (head)`** checks the base out next to the PR (the test merge's first parent, or the merge group's base). It builds the base's binary under `${RUNNER_TEMP}`, outside every cached path, and runs `cargo xtask ratchet observe`. That builds and runs the head's code. The job uploads only the JSON record. Its steps get no token: checkout uses a read-only one and does not persist it.
- **`ratchet`**, the required check, runs no head code:
  - It checks out the base and the PR as data.
  - It installs the toolchains and the pinned `cargo-dylint` itself, not from a cache that the head job could write.
  - It builds the base's binary under `${RUNNER_TEMP}`.
  - It runs `cargo xtask ratchet judge` with the PR number, the PR's head commit, the commits judged and the reviewer app's login.
  - It reads the base's corpus from its own checkout, so the head job cannot change it. The judge compares the corpus fingerprint with the one in the record. A mismatch or an empty corpus is no verdict.

Both jobs run on `pull_request` and `merge_group`. `ratchet` runs `if: always()` and is never skipped by a condition, because a skipped job counts as passed. A base without a ratchet skips with a note, as `base gate` does.

**After a review.** A review arrives after the push, so `.github/workflows/ratchet-review.yml` runs only the judge, on `pull_request_review` and on `pull_request_review_thread`. The record names the commit observed (GitHub's test merge) and the PR head. The review run takes the record of the latest `guardrails` run for the PR head, waiting for it if needed, fetches that commit by SHA, checks that its second parent is the PR head and its first parent an ancestor of the base branch, and judges exactly that commit against that parent; the judge refuses a record of another commit or head. If the record cannot be fetched, or the commit is gone or does not check out, the job gives no verdict (exit 2) and says to re-run the `guardrails` workflow for the head, or to push. It never reuses an earlier conclusion: a green one would hide a request for changes or a dismissed approval that came after it. Only a base branch without a ratchet skips, as in `guardrails.yml`. The record is kept for 90 days, the longest GitHub keeps an artifact of a public repository. Only events that can change the gate run under the name `ratchet`: the reviewer app's reviews and thread resolutions, reviews by accounts with write access, dismissals, and threads unresolved, which only the PR's author and writers can do. Any other account's review runs under another name and changes nothing, so an outsider can neither turn `ratchet` red nor cancel a run that counts. No review can start or cancel `ratchet (head)`.

In a merge group, the judge diffs the group's head against the group's parent commit (`merge_group.base_sha`), and the summary names that range, so that what was judged can be checked. The reviews and approvals are those of the PR named in the queue's ref, and its head must be the commit in that ref.

### 6. The review gate

The reviewer app is the bot that the repository variable `NAMI_REVIEWER_LOGIN` names (`nami-reviewer[bot]`, a GitHub App separate from the agents' `nami-agent[bot]`). For every rule-path PR, additive or edit, the check fails unless all of these hold:

- no request for changes by the app stands: a later comment or approval by the app withdraws one, but a dismissed request, which any writer can dismiss, the authoring app included, stands until the app approves; a dismissed review whose earlier state is not known counts as a dismissed request;
- the PR was not opened by that app;
- every review thread that the app opened is resolved, and resolved by that app.

**For an additive diff, that is all** (maintainer's decision: additive diffs merge on the ratchet's proof alone). The app need not have reviewed the head commit, and need not be set up; the summary notes whether it reviewed.

**For an edit** (point 2), the app's latest decision must in addition be an `APPROVE` of the head commit. A comment does not decide, so a later comment leaves the approval standing; a later request for changes, a dismissal of the approval, or an approval of an earlier commit does not count. Without the approval, a code owner's approval of the head commit stands in.

Everyone else's threads are left to the ruleset, which requires every conversation resolved before merge; they are never a ratchet finding, whoever resolved them. A review of an earlier commit does not count, not even for fixes: the reviewer re-reviews the new head and resolves the fixed threads then. The judge passes the head commit it judged, and gives no verdict when the PR's head has moved on by the time the reviews are read.

**Identity.** An app is matched by its account type and exact login with `[bot]`, never by a login alone, so a user account named `nami-reviewer` is not the reviewer. GitHub logins cannot contain `[`, so only an app has such a login. A code owner's approval counts by that owner's latest review that decided something: a later request for changes, or the dismissal of the approval, cancels it, and any owner's standing request for changes blocks.

**Where the review runs.** The gate counts the reviews that the bot posts, whatever posts them. The reviewer is to run in CI (#59), from a workflow that holds the app's key as a secret and runs no PR code, so that no authoring session can post as it. The gate does not depend on that. Until the app exists and the variable is set, an additive PR merges on the ratchet alone, and an edit needs a code owner's approval of its head commit, as today.

**A requirement on the CI-hosted reviewer (#59):** it must be able to submit `APPROVE`, not only `COMMENT`. An edit merges only on the app's approval, and only the app's approval withdraws a request for changes by it that a writer dismissed.

### 7. Staging

The ratchet lands in shadow mode first: the core moves, then `ratchet observe` and `ratchet judge` with both CI jobs, not required, with every guardrail path still owned. The CI-hosted reviewer lands separately (#59). The policy flip comes last and alone: this ADR, the owner-less CODEOWNERS lines with their last-match rule, and making `ratchet` a required check. It merges right after the ratchet (#57), without a period of shadow evidence (maintainer's decision), once `ratchet` is a required check (`docs/operations/github-setup.md`, step 9). **Additive rule-path PRs merge on the ratchet alone as soon as the flip (#55) merges, even before the CI-hosted reviewer (#59) exists.** Until #59 exists, an edit still needs a code owner's approval of its head commit.

### 8. A PR that touches both, deliberate loosenings, and the residual risks

- A PR that changes rule paths and owned core paths needs the owner's approval, and the ratchet still runs and must pass.
- A deliberate loosening, approved with an ADR, cannot pass. The maintainer merges it with the ruleset's admin bypass, as for `passes` flips (ADR-0017).
- **Residual risks**, which the AI review and the sampled audits (ADR-0008) must cover:
  - **Rules without cases.** The ratchet proves "not looser on any case written down", not "not looser". A shape that no case shows can go silent. The corpus only grows: a reviewer's probe that finds a loosening becomes a case.
  - **Rule code that special-cases its inputs.** Code that recognises a planted case by its content, or behaves differently under observation, can pass. The placements make this harder, not impossible.
  - **An edit that the reviewer misses** (point 4).

## Enforcement

- `cargo xtask ratchet` (`xtask/src/ratchet.rs`, `xtask/src/ratchet/`), with unit tests and `xtask/tests/ratchet.rs`. That test runs it against fake heads:
  - a loosened rule, at drawn placements too;
  - a rule that fires on correct code;
  - a walk that skips `crates/`;
  - a narrowed ban scope, a wider allowed list, a removed rule, a ban moved to the other table, and tables or calls hidden behind `#[cfg]` or a macro;
  - a deleted `check_*` call;
  - a dropped, lowered and twice-declared lint, and a dylint run whose exit code disagrees with its output;
  - changes outside the rule paths;
  - mutation results that are missing, empty or timed out, and a changed hunk without a mutant of its own;
  - an additive diff that passes with no review by the app and with no app set up, and fails on its open request for changes or thread;
  - an edit that gives "no verdict (edit)" while the reviewer app has only commented, and passes once it approves the head or a code owner does;
  - a ban narrowed in code to spare a crate outside its case's scope;
  - a registry change that only rewords a message, and one that changes a field no data check compares;
  - reviews by the wrong app or by a user named like it, threads resolved by the wrong account, a head that moved, and other people's threads left to the ruleset;
  - a code owner's approval of the head commit standing in for what is unproven, and cancelled by a later request for changes;
  - a corpus changed in the head job, a head changed between the jobs, and a head with a symbolic link;
  - the head build;
  - the command line, including `observe` then `judge`.
- The class (`xtask/src/ratchet/class.rs`): a test for each exploit of rounds 3, 4 and 5 of #57's review, each an edit; tests that what the dialect used to hold to the base (a changed or new constant, a new read of the environment, a new `impl` of an existing type, a changed function that applies a ban row, the registry's forms outside its tables) is an edit; tests that only plain `impl`s of a type new in the same file are additive, that nested items and the macros that read the source tree are edits, and that dispatch calls resolve by path; and tests that a new tidy rule with its dispatch call and its own visitor, and a new lint with its `impl LateLintPass`, are additive, while look-alike dispatch calls are edits.
- The review gate (`xtask/src/ratchet/review.rs`): a test that an approval counts only as the reviewer's latest decision on the head commit.
- The required `ratchet` check (`.github/workflows/guardrails.yml`, and `.github/workflows/ratchet-review.yml` for reviews; ruleset per `docs/operations/github-setup.md`, step 9; the reviewer in CI, #59).
- `.github/CODEOWNERS`. `xtask/src/pr.rs` applies GitHub's last-match rule and refuses owner-less lines outside `RULE_PATHS`. Its tests hold the owner-less lines equal to `RULE_PATHS`, and the file equal to what `flipped_codeowners` (`xtask/src/ratchet/scope.rs`) makes of its owned lines, which is the file the "after ADR-0022" verdict in shadow mode assumes.
- The dialect (`xtask/src/ratchet/dialect.rs`): a test per form that hid a loosening in the tables or the readings in the reviews of #57, each giving no verdict; a test of which registry fields count as covered; and a test that today's rule paths are in it.
- The test `every_rule_in_a_rule_path_has_a_case` keeps `main` free of rule-path rules without a case.
- **No mechanism:** whether the corpus is rich enough, and whether an edit is right. The reviewer app and the sampled audits judge that.

## Consequences

- **An additive PR merges on the ratchet's proof alone,** with no review needed: new fixtures and UI cases, a new ban row with its fixtures, a new tidy rule as new functions and types with one dispatch call and its own visitor, a new lint with its pass and `impl LateLintPass`, reworded documentation. An open request for changes or thread by the reviewer app still blocks it.
- **A data-only change to the registry's tables needs no mutant** (maintainer's decision on #57). The rule-set check reads the tables as data and proves that they only tighten. That holds only for the fields that check compares and those that only document (point 3); a change to any other field is an edit.
- **An edit to existing rule code waits for the reviewer app's approval of its head commit,** or a code owner's: a changed constant, function, `impl` or test, a fixture changed or deleted, a reworked lint like #31's. Its checks still run, and still fail it on a finding.
- **The dialect no longer holds code to the base.** Rounds 3 and 4 of #57's review added hold lists for constants, reads of the environment and the functions that apply a ban row; each round found a form they missed. The class replaces them: any change to an existing item is an edit.
- **Most new rules still need the owner for now.** A new tidy rule is tagged in `docs/TIGER_STYLE.md`, which is core (#53).
- **Until the reviewer app exists, an edit needs a code owner's approval of its head commit**, as today, but never the admin bypass. An additive PR does not wait for the app.
- **Time.** Measured on the maintainer's 32-core machine with warm caches:
  - the ratchet without mutants takes about 30 s for a tidy change and a minute for a lint change, with three placements per case;
  - mutation testing of #34's diff (35 mutants) took about 2 minutes, and of #31's (122 lint mutants) about 6.

  CI runners are slower. The head job has 120 minutes and the judging job 60, which includes installing `cargo-dylint` (several minutes). If lint PRs routinely run long, the mutants move to a sharded job.
- The workspace rules and the lint library's manifests stay with the owner, because nothing in the corpus covers them (#54).
- A head tool that crashes on a base case gives no verdict, so a flaky crash shows as a red ratchet.

## Alternatives considered

- **Keep every rule change with the owner (ADR-0020).** Rejected for rule paths. It spends the maintainer's attention on the changes the corpus can check, and #31 showed that reading the diff misses what running the cases finds.
- **Prove edits mechanically too: hold more of the existing code to the base, round by round.** Rejected after rounds 3 and 4 of #57's review. Each hold list (constants, ban-applying functions by name, the entry point) was followed by a narrowing in a function it did not name, and mutation testing cannot see a loosening whose test is edited along. The independent reviewer caught all of them.
- **Treat every rule-path change as an edit.** Rejected. A new case, a new ban row or a new lint in its own file cannot make an existing rule report less, and the corpus and data checks prove that much; sending them to review would keep the bottleneck this ADR removes.
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

- **With the base's own corpus, no intermediate commit is caught.** The shapes that went silent were not written down; this is the residual risk in point 8.
- **Once the probes are written down,** each round's regressions are found at the lines the reviewers found by hand, and the merged lint passes.
- **Mutation testing of the merged lint** leaves 21 of 122 mutants missed (92 caught, 9 unviable), for example `replace is_float_mul_add -> bool with true`. Under this ADR, #31 would have needed more cases.
- **#31 is an edit:** it reworked the existing lint's functions. Under the two classes it also needs the reviewer app's approval, and the review is what caught its regressions.

**PR #55's exploits replayed** against the first version (head = this branch with one change, `--no-review --no-mutants`):

| Exploit | Before | Then |
|---|---|---|
| Assertion density's threshold lowered to 5/3 | pass | The code is in `tidy/assertions.rs`, core and owned. The verdict is `pass, outside the rule paths`, with the code owner reviewing. |
| `direct-libm` scope `Everywhere` → `NamiCrates` | pass | fail: rule set, "narrows its scope" |
| An allowed entry added to `debug-assert-placement` | pass (by reading) | fail: rule set, "is allowed in [\"crates/solver\"] as well", plus corpus and end-to-end findings |
| `check_float_equality` call deleted | pass (by reading) | fail: rule set, "`check_body` no longer calls `check_float_equality`", plus 30 corpus and end-to-end findings |
| Base fixtures deleted in the head job before observing | pass (by reading) | no verdict: "the observations do not cover the base's corpus" |

**#57's exploits of rounds 3 and 4, under the two classes.** Each edits existing rule code, most with the head's own unit test edited along, and each passed every mechanical check when it was found, mutants on (round 3's ones until the hold lists of that round, which round 4 then got past). Each is now an edit, so without the reviewer app's or a code owner's approval of the head it gives `no verdict (edit)`, naming the change. `xtask/src/ratchet/class.rs` has a test for each.

| Round | Exploit | The edit the class names |
|---|---|---|
| 3 | `MAGIC_TOLERANCE_MAX` from `1.0e-6` to `1.0e-9`, with the unit test edited to match | the item `const MAGIC_TOLERANCE_MAX` changed |
| 3 | `crates/solver` added to `direct-libm`'s exceptions through a private `PATH_BAN_ROWS` | the new item `const PATH_BAN_ROWS` is a constant or static |
| 3 | `if self.path.starts_with("crates/solver") { return; }` at the top of `check_path_start` | `Visitor::check_path_start` changed beyond a dispatch call |
| 3 | an exception inlined into `check_path_start`'s allowed list for `direct-libm` | the same |
| 3 | a condition on the path added in `visit_path` | `<Visitor as Visit>::visit_path` changed beyond a dispatch call |
| 4 | a new `visit_item` override that skips `crates/solver/src/io` | the existing `<Visitor as Visit>` has the new member `fn visit_item` |
| 4 | `report_unsafe` spares `crates/solver/src/ffi` | `Visitor::report_unsafe` changed beyond a dispatch call |
| 4 | a first-statement guard in `check_tolerance` | `Visitor::check_tolerance` changed beyond a dispatch call |
| 4 | `is_tolerance` scaled by `1.0e-3`, inline or through a `let`, with the unit test edited | `is_tolerance` changed, and the test module's test changed |
| 4 | `&& !self.scope.is_nami()` on `function-length` in `check_body` | `Visitor::check_body` changed beyond a dispatch call |
| 4 | `runtime-dependency` spared for `crates/kernels` in `check_manifest` | `check_manifest` changed beyond a dispatch call |

**#57's exploits of round 5** went at the class itself. The first version of the class took an `impl` for additive when the last segment of its self type was a new type of any changed file. Both exploits passed as additive; both are edits now, each with a test in `class.rs` (`a_new_type_elsewhere_does_not_vouch_for_an_impl`, `only_plain_impls_of_new_types_are_additive`):

| Exploit | Then | Now |
|---|---|---|
| a new `struct Visitor` with its rule in `text.rs`, and in `rust_source.rs` an inherent `impl Visitor<'_> { fn visit_file(..) }` that skips a file starting with `#!`, which turned every rule off there (honest-plausible) | additive | edit: the new `impl Visitor<'_>` is not an `impl` of a type new in its file |
| `trait Quiet { fn visit_file(..) {} } struct Quieted; impl<Quieted> Quiet for Quieted {}`, a blanket `impl` whose parameter shadows the new struct (deliberate) | additive | edit: an `impl` with generics |

**#57's exploits of round 6** went at the dispatch calls and the tests. Each passed as additive; each is an edit now, with a test in `class.rs` (`extend_adds_only_reports`, `new_tests_may_not_read_the_file_at_run_time`):

| Exploit | Then | Now |
|---|---|---|
| `names.extend(vendored(&root));` in `manifest::workspace_crates`, with a new `fn vendored` naming `rayon`, so `runtime-dependency` let a nami crate depend on it (honest-plausible) | additive | edit: `names` is no report list |
| `bindings.names.extend(rebound(&pattern, &self));` in `FloatEqualityFinder::unbind`, which turned `float-eq-in-eq-fn` off after any untyped `let` (deliberate) | additive | edit: `bindings.names` is no report list |
| `lints.extend(..)` in `manifest::enforced_lints` (`doc-tag`), and `parameters.extend(..)` in `float_reduction::is_selection` (the argmin exemption) | additive | edit: neither is a report list |
| a new test in `rust_source.rs` that asserts the length of `std::fs::read_to_string` of the file itself, so every mutant of the file fails it (deliberate) | additive | edit: the new test names `fs` |

**#34 replayed with mutation testing** (first version): pass, 35 mutants, 33 caught and 2 unviable.

**A bug the first replay found in the ratchet itself.** With one target directory for both lint libraries, cargo considered the scratch package fresh after the library changed. It replayed the base's diagnostics for the head, so the lint corpus passed a loosening head. Each side now builds in its own target directory, with the library's fingerprint in `DYLINT_RUSTFLAGS` (ADR-0018, point 9).
