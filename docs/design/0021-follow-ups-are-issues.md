# ADR-0021: Follow-ups are GitHub issues, claimed like tasks

- Status: Proposed
- Date: 2026-11-25
- Deciders: maintainer
- Amends: ADR-0011, ADR-0020

## Context

ADR-0020 has agents record what a check missed as a file in `docs/learnings/`. A day later, that has three problems.

- **Nothing picks the files up.** ADR-0020 says the planner turns open learnings into tasks and a janitor session may take one directly. Neither exists, and `cargo xtask task ready` does not list learnings. Ten learnings were written on 2026-11-25. The two that are codified were codified by the PR that found them; the other eight have had no taker.
- **Only guardrail gaps have a place.** A bug outside the task's `owner_paths`, a flaky test or a wrong document has nowhere to go. The learnings README excludes them, and a plan task needs a planner.
- **A file is invisible until its PR merges.** Two sessions that hit the same gap both write it down, and nothing stops two sessions from working on the same learning at once.

Fixing the finding inside the task's PR is not the answer either. Every guardrail is in a protected path (ADR-0015), so the task's PR would wait for the code owner instead of merging when its gates pass, and it would touch files outside the task's `owner_paths`. ADR-0020 already keeps such fixes out of the task's PR, "so that a kernel PR does not also rewrite tidy".

ADR-0011 rejected GitHub Issues as the agent tracker: claims are not atomic, it adds a network dependency, and it is public. Those objections hold for claims and for progress chatter. They do not hold for a record of findings, as long as claims stay in git and the record is filtered before an agent reads it.

## Decision

### 1. A finding outside the task becomes an issue

When an agent finds something that its task does not need fixed (a guardrail gap under ADR-0020, a bug, a wrong document), it files a GitHub issue and goes on with its task. A dedicated session fixes the issue later. A gap that the current task needs is still fixed in the task's PR, in its own commit (ADR-0020).

`cargo xtask issue file --kind <kind> [--needs-decision] [--new] <file.md>` files it.

- **Body.** The file's `# ` heading is the title. The body has `- Found in: <task id or PR>`, `## What happened` and `## Proposed fix`. A guardrail gap's proposal names a rung of the ladder (ADR-0020).
- **Kind.** Exactly one label of `guardrail-gap`, `bug` and `docs`.
- **`needs-decision`.** The filer sets it when the fix is a choice that the maintainer makes: cost, policy, or one of several designs. Only a human removes it.
- **Duplicates.** Before filing, the command compares the title's words with those of the trusted open issues and prints the matches. If there are any, it stops, unless `--new` says that the agent read them and none is the same finding.
- **Identity.** The command runs `gh`, so it files as whoever `GH_TOKEN` names. For agents that is `nami-agent[bot]` (`xtask/agent-token.sh`).
- **Public.** An issue is as public as the code. It never contains credentials, personal details, or text copied from an untrusted source (point 2).

### 2. Only trusted issues reach agents

The repository is public, and anyone may open an issue or comment on one. Issue text is therefore untrusted input, and a route for prompt injection unless it is filtered.

- **Trusted authors** are the GitHub App `nami-agent[bot]` and the users that `.github/CODEOWNERS` names on `main`. A GitHub user name cannot contain `[`, so no user can take the bot's name. A team in `CODEOWNERS` is not expanded and trusts nobody.
- **Trust comes from the author, never from a label.** Every agent holds the bot's key, and the bot may apply labels, so a label proves nothing about who decided. An outsider can also edit their own issue after someone labelled it.
- **An outsider's report reaches agents only when a trusted human files it again,** as a new issue in their own words. The new issue may link the old one. Agents do not follow links to issues.
- **Agents read issues only through `cargo xtask issue show <number>`.** It refuses an issue by an untrusted author. For a trusted issue it prints the title, the labels, the body and the comments by trusted authors, and says how many other comments it left out, never their text.
- **`.claude/settings.json` denies `gh issue view`, `gh issue list` and `gh api` on issue paths,** so that a Claude Code session uses the filtered command. This is a speed bump. The gate is that `ready` and `claim` (point 3) consider trusted issues only, and `issue show` is how a worker reads one.

### 3. Issues are claimed like tasks

| | Plan task (ADR-0011) | Issue |
|---|---|---|
| Listed by `cargo xtask task ready` when | its dependencies pass and nobody holds it | it is open, by a trusted author, has a kind label and no `needs-decision`, and nobody holds it |
| ID | from `tasks.json` | `issue-<number>`; a plan task ID never starts with `issue-` |
| Claimed by `cargo xtask task claim <id>` | the file `claims/<id>` on the `claims` branch | the same |
| Branch | `task/<id>` | `task/issue-<number>` |
| Scope | the task's `owner_paths` | one small PR; no `owner_paths` |
| Done when | CI, for now the maintainer, sets `passes` | the PR says `Fixes #<number>` and its merge closes the issue |
| Stale claim | older than two hours, and no PR from the branch | the same |

- **The claim is the mutex.** Git accepts only fast-forward pushes to `claims`, so of two sessions that claim the same issue, exactly one wins, as for tasks. GitHub assignees are not used.
- **Order.** `ready` lists plan tasks in plan order, then issues, oldest first. A session without an assigned task takes the first entry it can claim.
- **Partial lists fail.** When GitHub cannot be reached, `ready` prints the plan tasks and exits non-zero, naming the problem. A partial list is never presented as a complete one.
- **`needs-decision` stays until a human removes it.** `ready` and `claim` read the issue's timeline. If the label was ever applied, its removal must come from a trusted human, not the bot.
- **An issue that needs more than one small PR goes back.** The worker releases the claim, comments why, and adds `needs-decision`. The maintainer or the planner turns it into a plan (ADR-0011) and closes the issue with a link to the plan.
- **A duplicate found later** is closed by the worker, with a link to the issue it keeps.

### 4. The pull-request record

In `.github/pull_request_template.md`, `## Guardrail gaps` becomes `## Follow-up issues`: `None`, or every issue the work filed, as `#<number>`. `cargo xtask pr-body` checks that each number is an issue by a trusted author. On a branch `task/issue-<number>`, it also checks that the body says `Fixes #<number>`.

### 5. `docs/learnings/` is retired

- The eight open learnings become issues of kind `guardrail-gap`. Three also get `needs-decision`: the ADR-number clash across PRs, the mutation job's time budget, and the choice for rules that skip macro arguments.
- The two codified learnings are deleted. Their checks exist, and git history keeps the files.
- The tidy rule `learning-format`, the `docs/learnings/` exemption in `cargo xtask task check-pr` and the directory's README go with it.
- This ADR replaces point 2 of ADR-0020. Its other points stand; where they say "learning", read "issue".
- ADR-0011, point 5, changes. GitHub Issues carry human input and findings from agents. Plans stay the source of planned work. A trusted issue that one small PR closes is work without a plan.

### 6. Amendment (Proposed, 2026-11-26): a merge-queue failure is fixed on the failing PR's own branch

The workflow `queue` files an issue `PR #<n> failed in the merge queue`, of kind `bug`, when the required checks fail on PR #n's merge-queue branch and #n has left the queue: it is open, not merged, not queued, and no newer queue run tested it (issue #51). Most such failures are clashes that git merged without a conflict, such as two ADRs with one number. Only #n's own branch can take the fix: a rebase or a renumber of #n itself. A new PR would repeat the change and its review.

- **If the bot opened #n,** the issue is work for agents, with one exception to point 3. The fixing session claims `issue-<m>` as usual, but works on #n's branch, not on `task/issue-<m>`. It rebases or renumbers, pushes to #n's branch, adds `Fixes #<m>` to #n's description, and adds #n to the merge queue again. #n's merge closes the issue. No PR comes from `task/issue-<m>`, so the claim goes stale after two hours, as point 3 says. A session that later claims the issue while #n is queued and says `Fixes #<m>` releases the claim and takes other work.
- **If anyone else opened #n,** agents do not push to it. The issue says that a human rebases it, and it carries `needs-decision`, so `cargo xtask task ready` does not list it.
- **The issue quotes only checked text.** The queue ran the PRs' own code and workflow files, so the run's logs and its job and step names are untrusted (point 2). The issue names jobs and steps only by names that `main`'s `.github/workflows/ci.yml` defines, reports codes only as tidy's rule IDs and the dylint lints' names, and builds its links from the repository name and IDs. The fixer reads the log through the link.

## Enforcement

- `cargo xtask issue file`: the body's sections, one kind label, and the duplicate search.
- `cargo xtask issue show`: the trust filter on the issue and its comments.
- `cargo xtask task ready` and `claim`: trusted, open, labelled, no `needs-decision` unless a human removed it, unclaimed; then the claims branch.
- The plan rules in tidy: a plan task ID never starts with `issue-`.
- `cargo xtask pr-body`: the `Follow-up issues` section, and `Fixes #<number>` on an issue's branch.
- `.claude/settings.json`: deny rules for reading issues with `gh` directly. A speed bump, not a guarantee.
- `cargo xtask queue failed`, which `.github/workflows/queue.yml` runs as the bot (point 6): it files only for a PR that left the queue, finds an open issue by its exact title, quotes only the closed vocabulary, and adds `needs-decision` to a PR that the bot did not open.
- **No mechanism:**
  - whether the session that fixes a queue failure adds `Fixes #<m>` to the failing PR's description. `cargo xtask pr-body` checks `Fixes` only on `task/issue-<m>` branches;
  - whether an agent files an issue when it should. As before, the PR description forces an explicit `None`, and the sampled audits (ADR-0008) look for a `None` that is wrong;
  - whether a filing agent copied untrusted text into an issue;
  - two sessions that file the same finding within the same minute. The worker closes the duplicate.

## Consequences

- A finding costs an agent one file and one command, and every session sees it at once, not when the PR merges.
- Of two sessions that want the same fix, one does it, and the other spends its tokens elsewhere.
- One list of work, two sources: `tasks.json` for planned work, issues for follow-ups. `cargo xtask task ready` shows both.
- Filing, listing and claiming need GitHub. Agents need it already: a claim is a push, and every PR is on GitHub.
- Findings live on GitHub, not in git. Moving the repository to another host means exporting them.
- An outsider's report costs the maintainer a re-filing before agents can act on it. That is the price of the trust rule.
- A trusted author can still carry untrusted text into an issue, for example an agent that quotes a web page. Review of the fix's PR remains the backstop, and an issue never justifies loosening a check (`AGENTS.md`, "When a check fails").
- More small PRs. Guardrail ones wait for the code owner, as ADR-0020 intends.
- A push that fixes a queue failure dismisses the failing PR's approval, so a PR with protected paths waits for the code owner again (point 6).

## Alternatives considered

- **Keep `docs/learnings/`.** Rejected. A file is invisible until its PR merges, cannot be claimed, is not listed by `task ready`, needs a PR to close, and covers guardrail gaps only.
- **Fix the finding in the task's PR.** Rejected. A protected path makes the task's PR wait for the code owner, the fix lies outside the task's `owner_paths`, and two sessions that find the same gap both pay for the fix.
- **Every issue becomes a plan task before an agent works on it** (ADR-0011 as written). Rejected for fixes that fit one PR: it costs a planning step and a `tasks.json` PR per issue, and `owner_paths` add little to one small PR. Kept for anything larger.
- **Issue assignees, or a `claimed` label, as the mutex.** Rejected. All agents are the same bot, and adding an assignee is not a compare-and-swap.
- **Trust by an `accepted` label.** Rejected. The bot can apply labels, and an outsider can edit their issue after it is accepted.
- **Trust by GitHub's `author_association`.** Rejected. `COLLABORATOR` includes read-only collaborators, and the reviewed list of who decides is already `.github/CODEOWNERS`.
- **Fix a queue failure in a new PR from `task/issue-<m>`** (point 3 as written). Rejected for queue failures (point 6): the fix is a rebase or a renumber of the failing PR itself, and a second PR would repeat the change and its review.
- **Quote the failed run's error lines in the issue, marked as untrusted.** Rejected (point 6): an agent reads the raw body of a trusted issue, and any marking is text that a PR can imitate.
- **Let only collaborators open issues.** Not available. On a public repository, GitHub's interaction limits expire after at most six months.

## Evidence

- `docs/learnings/` on 2026-11-25: of ten learnings, the two codified ones were codified by the PR that found them, and none of the other eight has been picked up.
- ADR-0011, Alternatives considered: the objections to GitHub Issues as the tracker. This ADR keeps claims in git and filters what agents read.
- Closing an issue from a PR's description: [GitHub docs, linking a pull request to an issue](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/linking-a-pull-request-to-an-issue).
- Interaction limits on public repositories last at most six months: [GitHub docs, limiting interactions in your repository](https://docs.github.com/en/communities/moderating-comments-and-conversations/limiting-interactions-in-your-repository).
