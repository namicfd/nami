# ADR-20261128-1856: ADR IDs are UTC minutes, and the index is generated

- Status: Proposed
- Date: 2026-11-28
- Deciders: maintainer
- Amends: ADR-0000 (points 1, 3 and 6, and its enforcement), ADR-0014 (point 3: a second
  `#[expect]`)
- Summary: A new ADR's ID is the UTC minute it was started, `ADR-YYYYMMDD-HHMM`, and
  `cargo xtask adr new <slug>` writes it; 0000 to 0043 keep their numbers. Every `ADR-`
  reference resolves, the clash check fails a same-minute clash, and `docs/design/README.md` is
  generated from each ADR's header.

## Context

ADR-0000 names an ADR `NNNN-short-title.md`. Every agent took "the highest number plus one" on
its own branch, so parallel branches kept taking the same number. Git merges two files with one
number without a conflict, because their names differ; tidy's `adr-number` rule then failed the
second PR in the merge queue, or a reader noticed the clash in the directory listing.

- ADR-0018 was renumbered twice on 2026-11-25 for this reason.
- PR #45 wrote its architecture ADRs as 0022 to 0031, while ADR-0022 and ADR-0023 were taken
  on other branches. It renumbered them to 0034 to 0043 (ADR-0022, the note on its number).
- PR #80 renumbered its ADR as well, and the advisory clash comment of #52 only warned.

Each renumbering touched the file name, the title, the index row and every reference, on a
branch that was otherwise done, and cost a review round. The index itself was a second source
of conflicts: every PR that added an ADR appended a row to `docs/design/README.md`, and every
PR that amended one edited the amended ADR's row.

The maintainer decided the scheme on 2026-11-28, after a freeze in which every open PR with an
ADR merged, so that the change lands in one go.

## Decision

1. **The ID of a new ADR is the UTC minute it was started:** `ADR-YYYYMMDD-HHMM`, such as
   ADR-20261128-1856, this ADR. Its file is `docs/design/YYYYMMDD-HHMM-<slug>.md`, where the
   slug is lower-case words joined by single dashes, and its title is
   `# ADR-YYYYMMDD-HHMM: <title>`. This replaces the file name of ADR-0000, point 1. The minute
   must exist in the calendar, from 2026 on. An agent finishes an ADR locally, with no round
   trip to GitHub.
2. **`cargo xtask adr new <slug>` writes a new ADR** from the template, with the current UTC
   minute, or the next minute that no ADR in the tree has, and regenerates the index. Agents
   never type an ID. The ID stays when the ADR is finished later: it names when the ADR was
   started, not when it merged.
3. **ADRs 0000 to 0043 keep their numbers.** About 950 references cite them, and renumbering
   would gain nothing. No new ADR gets a number, and the unused numbers (0016, 0024 to 0031) stay
   unused. Every parser accepts both forms. Numbers sort before minutes, and minutes by time.
   ADR-0000 point 3's `Superseded by ADR-NNNN` takes either form.
4. **A clash is moved, not renumbered.** Two ADRs started in the same minute on two branches
   have one ID. The PR whose clash is with `main`, or with an older open PR, moves its ADR to a
   free minute: it renames the file, changes the title and every reference, and regenerates the
   index. The older of two open PRs keeps its ID, so the two never both move.
   `cargo xtask queue clashes`, which only warned (#52), fails the PR that moves, and its comment
   names the next free minute.
5. **Every `ADR-` reference resolves.** In every text file of the repository that tidy reads,
   `ADR-` followed by a digit names an ADR in `docs/design/`. A reference to an ADR elsewhere, in
   a closed PR or another repository, drops the `ADR-` prefix. Placeholders such as `ADR-NNNN`
   have no digit and are no reference.
6. **`docs/design/README.md` is generated** by `cargo xtask adr index --write`, the way
   `docs/RULES.md` is (ADR-0018, point 6). This replaces ADR-0000, point 6. Each row comes from
   one ADR's header:
   - the title, after `# ADR-<id>: `;
   - the text of its `- Status:` line;
   - the text of a new header line, `- Summary:`, which every ADR has: the one line the index
     shows;
   - an Amended by column, from every other ADR whose `- Amends` line names it outside
     parentheses, marked with that ADR's status unless it is accepted. A parenthesis says what
     is amended, and an ADR cited inside one is context.

   Two PRs that add an ADR each then change no common line, and an amendment needs no edit of
   the ADR it amends.
7. **This PR adds the `- Summary:` line to every existing ADR,** with the text of its row in the
   old index, unchanged. It is a mechanical change applied to every ADR and recorded here, like
   ADR-0015's rename, so ADR-0000 point 4 holds. ADR-0000's status line notes this amendment.
   ADR-0042 cited the closed PR #4's ADR numbered 0016 in two places; those lose the prefix
   (point 5).
8. **`adr new` reads the clock,** which clippy bans in the workspace (`std::time::SystemTime::now`,
   ADR-0005), because nami's results must not depend on it. An ADR's ID is the one value in
   xtask that is the time, so the one function that reads it carries an
   `#[expect(clippy::disallowed_methods, reason = "...")]`, and the `#[expect]` budget goes from
   1 to 2. This amends ADR-0014, point 3, which made the allocator's `#[expect]` the only one.

## Enforcement

- **Tidy, source rules** (the edit hook runs them on an ADR as it is saved):
  - `adr-number`, now on IDs: no two ADR files share an ID, and the title repeats the file's ID.
    A legacy number that shares its first digits with a minute is no clash.
  - `adr-index`: the ADR is listed in the index; the message says to regenerate it.
- **Tidy, workspace rules** (`just check`, CI):
  - `adr-id`: every file in `docs/design/` but the index is named after a valid minute with a
    well-formed slug, or carries one of the legacy numbers.
  - `adr-reference`: point 5, over the sources, documents and configuration that tidy walks,
    below `docs/` and in the root `.claude/` without its worktrees, chosen by extension. Tidy's
    fixtures are not read, because they cite missing ADRs on purpose, and neither are scratch
    files such as the `pr.diff` that CI writes for cargo-mutants, which quote old text.
  - `adr-summary`: every ADR has a `- Summary:` line.
  - `adr-index-doc`: the index is what `cargo xtask adr index --write` generates. A foreign gate
    skips it, like `rules-doc`, because another revision's generator writes another index.
- **The `adr clashes` job** in `.github/workflows/queue.yml` runs `cargo xtask queue clashes`
  on every PR and fails the one that moves. It is not a required check: it runs on pull requests
  only, not in the merge queue, where tidy's `adr-number` fails the clash instead.
- **Clippy and tidy's `expect-budget`** hold the clock read to the one function, and the budget
  to 2.
- CODEOWNERS on `docs/design/` (ADR-0008).

## Consequences

- An ADR is finished on its branch. A clash needs two ADRs started in the same minute, and is
  fixed by moving one of them a minute.
- IDs are longer: thirteen characters instead of four. They are copied, not typed, and their
  order is the order in which the ADRs were started.
- The index never conflicts, and its Amended by column cannot go stale: it is computed. Its
  Status column is the ADR's own status line, so a status line that names its amendments, such
  as ADR-0003's `(amended by ADR-0013)`, repeats what the column says.
- A test that needs an ADR ID which does not exist builds the string at run time, because
  `adr-reference` reads xtask's sources too. The rule's message says how to cite an ADR outside
  this repository.

### Known gaps

- **Two open PRs can clash before either check runs again.** The check runs when a PR changes.
  If the older PR's ADR appears after the newer PR's last run, the newer one learns of the clash
  only from tidy in the merge queue, where ADR-0021 point 6 has it fixed on its own branch.
- **`adr clashes` gates nothing by itself.** It is not required, and a fork's PR skips it.
  Tidy's `adr-number` in the merge queue is the gate.
- **A wrong clock gives a wrong ID.** Any free minute is a valid ID, so the ADR still works; only
  its order in the index is off.
- **An `Amends` line that names an ADR as context outside parentheses** lists it as amended. The
  parser cannot tell; write context in parentheses.
- **Prose that names an ADR without the prefix**, such as the index's "0013", is not checked.

## Alternatives considered

- **A number handed out by GitHub,** the issue or PR number. Rejected by the maintainer: it
  costs a round trip and a CI run, and an agent should finish an ADR locally.
- **Date and a random suffix.** Rejected: a random part is easy to mistype, and a same-minute
  clash is rare and caught.
- **The day and the slug as the ID.** Rejected: every citation would carry the slug, and two
  ADRs on one day are common.
- **Keep numbers, and make the clash warning of #52 stronger.** Rejected: it detects the clash,
  but the renumbering it asks for is the cost that this ADR removes.
- **Renumber the existing ADRs.** Rejected: about 950 references for no gain.
- **Keep the index by hand, with one row per PR.** Rejected: appended rows conflict, and the
  amendments in the status column went stale.
- **Read the time from `date -u` in a subprocess,** to keep the `#[expect]` budget at 1.
  Rejected: it reads the clock that the ban is about by another route, which AGENTS.md calls
  working around a rule. If the maintainer prefers it to a second `#[expect]`, it is a ten-line
  change in `xtask/src/adr.rs`.

## Evidence

- ADR-0018, point 7: "This ADR was renumbered twice on 2026-11-25 for that reason, and the second
  clash was found by reading the directory listing, not by a check."
- ADR-0022, its note on the number: #45 used 0022 to 0031 and renumbered to 0034 to 0043.
- A scan of the tree on 2026-11-28: about 950 references to ADRs 0000 to 0043. Three named no
  ADR: two in ADR-0042, to the closed PR #4's ADR numbered 0016, and one in a test string of
  `xtask/src/queue.rs`, which the new clash tests replace.
- The old index's one-line column moved into the `- Summary:` lines unchanged, and the
  generated index shows it as before.
