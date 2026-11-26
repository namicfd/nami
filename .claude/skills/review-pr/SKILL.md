---
name: review-pr
description: AI review of a nami pull request (ADR-0008). Runs the tigerstyle-reviewer and numerics-verifier subagents on the PR in parallel, checks their findings, and posts one review as the reviewer app nami-reviewer[bot] (or nami-agent[bot] where no reviewer key exists) with every blocking finding as an inline thread that must be resolved before merge. Use with a PR number, in a session that did not write the PR.
---

# Review a pull request

Usage: `/review-pr <number>`.

This review is the AI half of ADR-0008. It never approves, merges or pushes. Only the PR's author fixes things, and only a human approves protected paths.

- **Fresh session.** Run it in a session that did not write the change. A reviewer that shares the author's context tends to agree with it.
- **Blocking findings** become inline review threads. The `main` ruleset requires every conversation to be resolved before merge, so they block the merge queue until they are fixed.

## 1. Act as the reviewer app

Every `gh` call in this skill runs as the reviewer app, `nami-reviewer[bot]`, never as the maintainer. Only its reviews and resolutions count for the `ratchet` check (ADR-0022); the authoring agents act as `nami-agent[bot]` and must not be able to stand in for it. Shell state does not carry over between commands, so prefix each one:

```sh
GH_TOKEN=$(NAMI_AGENT_APP=reviewer xtask/agent-token.sh) gh ...
```

The reviewer key is only on the reviewer's account (`docs/operations/github-setup.md`, step 5b). If the command fails because there is no reviewer key or client ID, this session may not be a reviewer session:

- Say so in your report.
- You may still review as `nami-agent[bot]` with `GH_TOKEN=$(xtask/agent-token.sh)`, for the record, but the `ratchet` check will not count that review, so a rule-path PR then needs the code owner.
- Never fall back to the user's account, and never copy the reviewer key into an authoring session.

## 2. Collect the PR

```sh
GH_TOKEN=$(NAMI_AGENT_APP=reviewer xtask/agent-token.sh) gh pr view <n> --json number,title,body,headRefName,headRefOid,baseRefOid,files,isDraft
git fetch --quiet origin "pull/<n>/head:refs/nami/review/<n>"
git worktree add --detach "$TMPDIR/nami-review-<n>" "refs/nami/review/<n>"
```

- If the PR is a draft, stop. Drafts are not reviewed.
- If the branch is `task/<id>`, find the task's `### <id>` section in `docs/exec-plans/active/*/plan.md` (in the worktree) and its `owner_paths` in `tasks.json`.

## 3. Run both reviewers in parallel

Start `tigerstyle-reviewer` and `numerics-verifier` in the same message, so they run concurrently. Give each one:
- the PR number and title, and the PR description;
- the worktree path, and the base commit (`baseRefOid`);
- the task section and owner paths, if any;
- the list of changed files.

Do not pass on your own opinion of the change. Each reviewer forms its own.

Run `numerics-verifier` whenever any of these changed:
- files under `crates/`;
- reference data;
- `tools/`.

Otherwise skip it, and say so in the review body.

## 4. Check the findings

Each reviewer ends with a `FINDINGS … END` block. Before posting anything:

1. **Verify each finding yourself.** Open the file at that line and confirm the problem is real and correctly described. Drop findings that do not hold up, and say in the review body how many you dropped.
2. **Merge duplicates** that both reviewers report.
3. **Place each blocking finding on a line of the diff.** GitHub only accepts inline comments on lines inside the PR's hunks (`gh pr diff <n>`). If a finding points elsewhere, attach it to the nearest changed line in the same file, or else to the first changed line of the PR, and name the real location in the comment.

## 5. Post one review

Write the review as JSON, then post it:

```json
{
  "commit_id": "<headRefOid>",
  "event": "COMMENT",
  "body": "<summary>",
  "comments": [
    { "path": "crates/kernels/src/flux.rs", "line": 42, "side": "RIGHT",
      "body": "**Blocking** (numerics-verifier): …" }
  ]
}
```

```sh
GH_TOKEN=$(NAMI_AGENT_APP=reviewer xtask/agent-token.sh) gh api "repos/{owner}/{repo}/pulls/<n>/reviews" --method POST --input review.json
```

- **Event:** always `COMMENT`; never `APPROVE` or `REQUEST_CHANGES`. The open threads are what block.
- **Each comment:** starts with `**Blocking**` and names the reviewer, then says what is wrong, why it matters, and what would fix it.
- **The body** has:
  - one line on what was reviewed;
  - which reviewers ran;
  - the suggestions, as a list, which are non-blocking and not threads;
  - how many findings were dropped as unconfirmed.

  With no blocking findings, the body says so plainly.

## 6. Re-review after fixes

When the author has pushed fixes, run this skill again on the same PR. Before posting the new review:

- List the bot's unresolved threads:

  ```sh
  GH_TOKEN=$(NAMI_AGENT_APP=reviewer xtask/agent-token.sh) gh api graphql -f query='query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){reviewThreads(first:100){nodes{id isResolved comments(first:1){nodes{author{login} path line body}}}}}}}' -F o=namicfd -F r=nami -F n=<n>
  ```
- **Fixed in the new head:** resolve the thread, as the reviewer app (step 1), with `gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}' -F id=<thread id>`.
- **Not fixed:** leave it open, and do not post it again.
- **Not the reviewer's:** never resolve threads opened by humans or by other reviewers. The `ratchet` check needs every thread resolved by the reviewer app, so a human's open thread keeps a rule-path PR with the code owner.

## 7. Clean up

```sh
git worktree remove --force "$TMPDIR/nami-review-<n>"
git update-ref -d "refs/nami/review/<n>"
```

Report to the user, in a few lines:
- the PR;
- the number of blocking findings and suggestions;
- a link to the review.
