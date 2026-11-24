# Orchestrating swarms of parallel autonomous AI coding agents on one codebase (as of Nov 2026)

Scope: case studies, coordination mechanics, role specialization, tests as oracle, and the human role. Written for tooling decisions on "nami", a Rust finite-volume PDE framework to be built mostly by agent swarms.

Notation: **[FACT]** is reported by the primary source (it is still the source's claim, not independently verified). **[OPINION]** is a judgement by the author or commenter. **[SECONDARY]** comes from an aggregator or third-party summary and is less reliable.

---

## 1. Case studies: what was tried, what worked, what failed, cost and throughput

### Takeaway
The best-documented large runs are Anthropic's C compiler (16 agents, about $20k, 100k LoC of Rust), Cursor's FastRender browser (about 2,000 concurrent agents at peak, over 1M LoC of Rust) and OpenAI's "harness engineering" product (about 1M LoC, 1,500 PRs, 3 to 7 engineers). In all three, success depended far more on the verification harness, task decomposition and repo legibility than on orchestration cleverness. Simple structures won. Heavyweight orchestrators (Gas Town) were expensive, hard to understand, and were later retired or refactored. The largest showcase (FastRender) was publicly criticized for not compiling.

### Cited Findings

**Anthropic: C compiler built by parallel Claudes (Nicholas Carlini, published Feb 5, 2026)**
- [FACT] 16 parallel Claude agents (Opus 4.6) ran for about two weeks across "nearly 2,000 Claude Code sessions", cost "just under $20,000", and used about 2B input and 140M output tokens. They produced a roughly 100,000-line Rust C compiler that builds Linux 6.9 on x86, ARM and RISC-V, plus QEMU, FFmpeg, SQLite, postgres and redis, and passes about 99% of most compiler test suites, including the GCC torture tests. It was a clean-room build with no internet and only the Rust std library — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] The harness was a bash `while true` loop that reran Claude Code with `--dangerously-skip-permissions` and a prompt file, starting a fresh session per task and logging by commit hash. The author recommends running it inside a container. Claude once ran `pkill -9 bash`, which killed itself and the loop — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] Failures and limits:
  - 16-bit x86 output exceeded the 32k limit (60kb+), so that phase calls out to GCC.
  - The assembler and linker are "still somewhat buggy", and the demo used GCC's.
  - Its generated code is "less efficient code than GCC with all optimizations _disabled_".
  - Code quality is "nowhere near the quality of what an expert Rust programmer might produce".
  - New features frequently broke existing functionality until CI was added late in the project.
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] The Linux kernel phase broke parallelism. Agents "would hit the same bug, fix that bug, and then overwrite each other's changes", and adding agents didn't help. The fix was to use GCC as an oracle: compile a random subset of kernel files with GCC and the rest with Claude's compiler, then delta-debug the failing combinations. Each agent could then chase a different bug — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] On model progression, Opus 4 was "barely capable", Opus 4.5 passed test suites but couldn't compile large real projects, and Opus 4.6 "nearly reached the limits of Opus's abilities" — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [OPINION] The author says the experiment "leaves me feeling uneasy" about deploying software nobody has personally verified, and that "it is easy to see tests pass and assume the job is done, when this is rarely the case" — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)

**Cursor: "Scaling long-running autonomous coding" (Wilson Lin, Jan 14, 2026)**
- [FACT] Hundreds of concurrent agents worked on one project for weeks, writing over 1M LoC and consuming "trillions of tokens". The browser-from-scratch run lasted about a week and produced over 1M lines across 1,000 files, with hundreds of workers pushing to one branch with "minimal conflicts". Other runs:
  - Solid-to-React migration inside Cursor's codebase: over 3 weeks, +266K/-193K lines, passing CI but "still needed careful review".
  - Java LSP: 7.4K commits, 550K LoC.
  - Windows 7 emulator: 14.6K commits, 1.2M LoC.
  - Excel clone: 12K commits, 1.6M LoC.
  
  — [Cursor blog](https://cursor.com/blog/scaling-agents)
- [FACT] From a Simon Willison interview with Wilson Lin (Jan 23, 2026):
  - About 2,000 agents ran concurrently at peak, producing "thousands of commits per hour" and about 30,000 commits in total.
  - About 300 agents ran per large machine, which worked because agents "spend a lot of time thinking".
  - Runs cannot be steered once started ("there's actually no way to steer it"). The longest runs lasted about a week.
  - No cost figure was disclosed.
  
  — [Simon Willison](https://simonwillison.net/2026/jan/23/fastrender)
- [FACT] Agents chose nearly all Cargo dependencies (Skia, HarfBuzz). They vendored and modified Taffy, which was not intended, and pulled in QuickJS as a stopgap. JS was feature-flagged off at the demo — [Simon Willison](https://simonwillison.net/2026/jan/23/fastrender)
- [FACT/SECONDARY] Critics report the repo did not compile ("could not compile 'fastrender' (lib) due to 34 previous errors") and that GitHub Actions on main were failing. Some developers did get it to build after fixes — [The Register opinion, Jan 26, 2026](https://www.theregister.com/2026/01/26/cursor_opinion/); [The Register news, Jan 22, 2026](https://www.theregister.com/2026/01/22/cursor_ai_wrote_a_browser/). Lin's own position is that a "stable rate of errors" fixed within a few commits was an acceptable trade-off for throughput — [Simon Willison](https://simonwillison.net/2026/jan/23/fastrender)

**OpenAI: "Harness engineering: leveraging Codex in an agent-first world" (Ryan Lopopolo, Feb 11, 2026)**
- [FACT] An internal product was built with "0 lines of manually-written code" over about five months, starting in late August 2025. It reached about 1M LoC and about 1,500 merged PRs. The team grew from 3 to 7 engineers, averaged about 3.5 PRs per engineer per day (rising as the team grew), and estimates it took about 1/10 the time of hand-writing. Single Codex runs worked on one task for "upwards of six hours" — [OpenAI](https://openai.com/index/harness-engineering/)
- [FACT] Before automating cleanup, the team spent every Friday (20% of the week) cleaning up "AI slop". It was replaced by recurring background Codex tasks that open small refactoring PRs, most of which can be reviewed in under a minute and automerged — [OpenAI](https://openai.com/index/harness-engineering/)

**Steve Yegge: Beads (Oct 2025) and Gas Town (Jan 2026, since retired)**
- [FACT] Timeline:
  - Oct 2025: Beads shipped.
  - Nov 2025: "Beads Blows Up" post, describing Beads as "brutally buggy, core holding".
  - Jan 1, 2026: Gas Town open-sourced.
  - Mar 2026: "Wasteland", a federated wanted-board of Gas Towns, launched (now retired).
  - Apr 2026: Gas Town v1.0 ("From Clown Show to v1.0"), then "Welcome to Gas City".
  
  Gas Town is "now retired" and succeeded by Gas City, an SDK of reusable parts that "can run hundreds of concurrent agents". The original could run "dozens of parallel agents". Yegge: "Beads outlived Gas Town. I still use it every day" — [yegge.ai/gastown](https://yegge.ai/gastown); [search summary of Yegge page](https://yegge.ai/gastown)
- [FACT] Gas Town itself was built in "17 days, 75k lines of code, 2000 commits" (Yegge quoted) — [Maggie Appleton](https://maggieappleton.com/gastown)
- [OPINION/SECONDARY] Cost and quality critiques:
  - Gas Town is "inefficiently burning through thousands of dollars a month in API costs" ([Maggie Appleton](https://maggieappleton.com/gastown)).
  - A critic said a team "burned a billion tokens in exchange for a pile of hot trash" (same).
  - HN user qcnguy (Jan 5, 2026): "isn't only vibe coded, it was vibe designed too… The number of overlapping and ad hoc concepts in this design is overwhelming" (same).
  - A secondary guide claims about $100/hour of parallel agent work, roughly 10x a normal Claude Code session. This is not from Yegge, so treat it with caution ([rywalker summary](https://rywalker.com/research/gastown); [echofold](https://echofold.ai/articles/gas-town-multi-agent-orchestrator-claude-code)).
  - An HN commenter on v1.0: "most of the complexity of gas town is absolutely not needed and probably detrimental" ([HN 47770124](https://news.ycombinator.com/item?id=47770124)).
- [FACT, from excerpt] The v0.x Beads design had problems with bidirectional sync, 3-way merges and race conditions, which were removed in v1.0 — [Yegge, "Gas Town: from Clown Show to v1.0", Apr 3, 2026](https://steve-yegge.medium.com/gas-town-from-clown-show-to-v1-0-c239d9a407ec) (full text not retrieved, so this comes via the search excerpt)

**Anthropic: "Effective harnesses for long-running agents" (Nov 26, 2025), single-agent and multi-session**
- [FACT] Pattern:
  - An initializer agent writes `init.sh`, a progress log and a JSON feature list (over 200 features for a claude.ai clone, each `passes:false`).
  - Coding agents then do one feature per session, may only flip `passes`, and it is "unacceptable to remove or edit tests".
  - JSON was chosen because models are less likely to clobber it than Markdown.
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [FACT] Failure modes it documents:
  - Declaring victory early or one-shotting.
  - A later agent seeing progress and stopping.
  - Leaving a broken or undocumented state.
  - Marking features done after only unit tests or curl, which missed end-to-end failures.
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)

**Codex cloud, Devin, Factory (thin, mostly secondary sources)**
- [SECONDARY] Codex cloud "best-of-N" runs the same task N times in parallel and lets you pick a winner. A practitioner blog recommends `--attempts 2` for non-trivial work and cites a limit of 6 concurrent threads per environment — [codex.danielvaughan.com](https://codex.danielvaughan.com/2026/04/01/codex-cloud-exec-best-of-n-attempts/)
- [SECONDARY] Devin reportedly added hierarchical orchestration in March 2026, with one instance coordinating others across isolated VMs. Factory's "Workstreams" (beta) splits a goal into areas with one agent per area. Reviewers say Codex and Devin do well on scoped single-service tasks but not on cross-service changes — [augmentcode](https://www.augmentcode.com/tools/devin-vs-codex-desktop-app); [macroscope](https://macroscope.com/content/best-coding-agent-orchestration-tools-2026)
- [SECONDARY] Codex moved to token-based billing in April 2026 — [Security Boulevard](https://securityboulevard.com/2026/06/8-cloud-coding-agents-that-open-prs-while-you-sleep/)

### Inferences
- Cost per useful LoC varies widely. The C compiler works out to about $0.20 per surviving line ($20k for 100k LoC) with a near-perfect oracle. Cursor's runs used "trillions" of tokens with no disclosed cost. For nami, a 16-agent, 2-week, roughly $20k envelope is the best-documented reference point for a well-oracled Rust project.
- Every successful case had a strong external oracle: GCC and torture tests, rendered-screenshot diffs against golden images plus rustc, or an end-to-end app with per-worktree observability. The Linux kernel phase shows that one shared failing target serializes the whole swarm unless the oracle can be split into independent failures.
- Rust is a recurring choice (C compiler, FastRender, video renderer), partly because "you can get a lot of verification just from compilation" (Lin). This favours nami's choice of language.
- Public showcases overstate readiness. Neither the C compiler nor FastRender is production quality, and the C compiler's code quality was explicitly below an expert's.

### Gaps
- No primary cost figure for Cursor's runs, Gas Town, Gas City, Devin, or Factory deployments.
- I could not retrieve the full text of Yegge's "Clown Show to v1.0" or "Welcome to Gas City". Concrete Refinery and merge lessons come only from secondary write-ups.
- Amp (Sourcegraph) and Google Jules: no reliable 2026 multi-agent case-study data found within the search budget.
- No independent audit of the C compiler's claims was found, beyond Anthropic's own post.

---

## 2. Coordination mechanics: claiming, isolation, merging, conflict avoidance

### Takeaway
The approaches that worked were "dumb" ones:
- Git as the synchronization substrate.
- One isolated clone, worktree or container per agent.
- Lightweight task claims (lock files, atomic tracker claims, a shared task list).
- Workers resolving their own merge conflicts.
- Planner-driven partitioning of work into non-overlapping file scopes.

Fine-grained locking and dedicated integrator agents created bottlenecks. Empirical studies show that textual conflicts are common (about 28% of agent PRs) and that semantic "passes alone, fails together" conflicts are a separate, under-measured risk.

### Cited Findings
- [FACT] C compiler setup:
  - A bare git repo serves as upstream. Each agent runs in its own Docker container, with upstream mounted at `/upstream` and cloned to `/workspace`.
  - An agent takes a "lock" on a task by writing a text file to `current_tasks/`. Git sync makes a second claimer pick something else.
  - The cycle is: work, pull, merge, push, remove lock.
  - "Merge conflicts are frequent, but Claude is smart enough to figure that out."
  - There was no orchestrator agent and no inter-agent messaging. Each agent picks "the next most obvious" problem.
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] Cursor tried three designs:
  1. A flat structure with a shared file and locks failed. Locks were held too long or never released, agents crashed while holding locks, and the system was brittle. "Twenty agents ran at the throughput of two or three."
  2. Optimistic concurrency control (writes fail if state changed since the last read) was more robust, but agents became risk-averse, avoided hard tasks and churned on small safe changes.
  3. A planner/worker/judge hierarchy worked best.
  
  A dedicated integrator role for quality control and conflict resolution "created more bottlenecks than it solved", and workers could handle conflicts themselves — [Cursor blog](https://cursor.com/blog/scaling-agents)
- [FACT] Lin says "most commits do not have merge conflicts" because planners split work into largely non-overlapping scopes and commits touch different files at different times. Work was later split into multiple streams, each with its own harness on a separate machine — [Simon Willison](https://simonwillison.net/2026/jan/23/fastrender)
- [FACT] OpenAI:
  - Each git worktree can boot its own app instance plus an ephemeral observability stack (logs and metrics queryable via LogQL and PromQL), and Chrome DevTools Protocol lets agents drive the UI.
  - Merge gates are minimal, PRs are short-lived, and flaky tests are usually rerun rather than blocking. The team calls this irresponsible at low throughput but "often the right tradeoff" at agent throughput.
  
  — [OpenAI](https://openai.com/index/harness-engineering/)
- [FACT] Gas Town uses a "Refinery", a merge-queue processor that "serializes merges so nothing collides", and a "Witness" patrol agent that watches workers ("polecats") — [yegge.ai/gastown](https://yegge.ai/gastown). [SECONDARY] A third-party write-up says the Refinery is a Bors-style bisecting merge queue, and that most of Gas Town's engineering effort went into the Refinery, Witness and Deacon rather than into the workers — [search summary citing augusteo.com](https://www.augusteo.com/blog/inside-gas-town/). [OPINION] Appleton notes the Refinery can "re-imagine" an implementation when conflicts get hairy — [Maggie Appleton](https://maggieappleton.com/gastown)
- [FACT] Beads (current README):
  - "Distributed graph issue tracker for AI agents, powered by Dolt."
  - Hash-based IDs (`bd-a1b2`) "prevent merge collisions in multi-agent/multi-branch workflows", with hierarchical IDs for epics and sub-tasks.
  - `bd ready` lists tasks with no open blockers. `bd update <id> --claim` atomically sets assignee and in_progress.
  - Dolt provides cell-level merge and branching. Embedded mode is single-writer; server mode (`dolt sql-server`) allows concurrent writers.
  - `.beads/issues.jsonl` is now only an export, "not the source of truth".
  - "Memory decay" summarizes old closed tasks. `bd prime` injects workflow context and memories.
  - About 29k stars, MIT licence.
  
  — [GitHub steveyegge/beads](https://github.com/steveyegge/beads)
- [FACT] Claude Code agent teams (experimental, behind `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`):
  - A lead plus teammates, each a separate Claude instance with its own context.
  - A shared task list (pending, in progress, completed, with dependencies and auto-unblocking). "Task claiming uses file locking to prevent race conditions."
  - JSON-file mailboxes at `~/.claude/teams/{team}/inboxes/{agent}.json`.
  - Hooks act as quality gates: `TeammateIdle`, `TaskCreated` and `TaskCompleted`, where exit code 2 blocks the action and sends feedback.
  - Limitations: no nested teams, one team per session, task status "can lag" and block dependents, and no resumption of in-process teammates.
  - The docs warn: "Two teammates editing the same file leads to overwrites. Break the work so each teammate owns a different set of files." Worktrees are documented as the manual parallel option.
  
  — [Claude Code docs: agent teams](https://code.claude.com/docs/en/agent-teams)
- [FACT] AgenticFlict (Ogenrwot and Businge, arXiv 2604.03551, Apr 2026, AIware 2026):
  - 142K+ agent PRs from 59K+ repos, of which 107K+ went through deterministic merge simulation.
  - 27.67% had merge conflicts (29K+ PRs), with 336K+ conflict regions.
  - Rates vary across agents, but per-agent figures were not in the abstract.
  
  — [arXiv 2604.03551](https://arxiv.org/abs/2604.03551). [SECONDARY] A blog claims Claude Code's rate is 25.93% and cross-agent conflicts run about 2x same-agent ones; I could not verify either — [search summary](https://codex.danielvaughan.com/2026/07/28/agent-pr-merge-conflicts-concurrent-coding-agents-codex-cli-worktree-isolation-coordination-defence/)
- [FACT] CooperBench (arXiv 2601.13295, ICLR 2026):
  - Two agents each implement features that are independent but could conflict.
  - The abstract reports an average 30% drop in success versus solo. The body reports GPT-5 and Sonnet 4.5 pairs at about 25%, roughly 50% below solo; the two figures may measure different things.
  - Agents spend up to 20% of their budget on communication, which cuts merge conflicts but does not raise success.
  - Agents rarely showed role division or negotiation spontaneously.
  
  — [arXiv 2601.13295](https://arxiv.org/html/2601.13295); [Stanford HAI](https://hai.stanford.edu/news/ai-coding-agents-fail-at-teamwork)
- [FACT/SECONDARY] "Passes Alone, Fails Together" (arXiv 2609.25396) benchmarks semantic conflicts, where patches pass individually but fail once merged. A separate SBES 2026 study of 14,960 conflicting merges found that resolution in agent-authored PRs is "overwhelmingly human-driven" — [arXiv 2609.25396](https://arxiv.org/pdf/2609.25396); [SBES 2026 paper](https://cbsoft.sbc.org.br/2026/data/papers/sbes/How%20AI%20Coding%20Agents%20Resolve%20Merge%20Conflicts%20An%20Empirical%20Study.pdf) (both from search snippets only; full text not read)

### Inferences
- A practical stack for nami:
  - Per-agent worktree or container.
  - A git-native or embedded tracker (Beads, or a plain `current_tasks/` lock directory) with atomic claims and a dependency graph.
  - Workers rebase and resolve their own conflicts.
  - A serializing merge queue that runs the full test gate on the rebased result (Bors / GitHub merge-queue style). This catches semantic conflicts that pass alone but fail together.
- Partition by file or module ownership. The FV architecture (mesh, reconstruction, Riemann solvers, time integrators, BCs, I/O) maps naturally onto disjoint crates or modules, which makes planner partitioning easy. Hot shared files (a central trait module, `lib.rs` re-exports, a Cargo workspace manifest) should get a single owner or be generated, to avoid repeated conflicts.
- Avoid elaborate inter-agent chat. CooperBench and Cursor both suggest that communication overhead does not buy success. Coordination through the task graph and the repo works better.

### Gaps
- No primary data on merge-queue throughput limits at swarm scale, such as how many PRs per hour a serialized Rust CI gate can absorb.
- Per-agent conflict-rate numbers in AgenticFlict were not verifiable from the abstract.
- I found no first-party description of Gas Town's Refinery bisection algorithm.

---

## 3. Role specialization: planner, worker, judge, reviewer, maintenance agents

### Takeaway
A shallow hierarchy works best: planners (recursive), stateless workers that don't talk to each other, and a judge or periodic fresh restart. Add recurring "janitor" agents for dedup, docs gardening, code-quality critique and performance. Dedicated integrator or merge agents and deep role taxonomies tend to become bottlenecks or add complexity. Agent-to-agent code review now does much of the review load at OpenAI.

### Cited Findings
- [FACT] Cursor:
  - Planners explore the codebase, create tasks, and can spawn sub-planners.
  - Workers complete tasks without coordinating with each other.
  - A judge decides at the end of each cycle whether to continue, and the next iteration starts fresh.
  
  — [Cursor blog](https://cursor.com/blog/scaling-agents)
- [FACT/OPINION] Cursor on models per role:
  - GPT-5.2 was better at extended autonomous work and at planning, even compared with GPT-5.1-Codex.
  - Opus 4.5 "tended to stop early and take shortcuts".
  - The team now picks a model per role.
  - "The harness and models matter, but the prompts matter more."
  
  — [Cursor blog](https://cursor.com/blog/scaling-agents)
- [FACT] Cursor's open problems: planners should wake when their tasks finish, agents sometimes run far too long, and "periodic fresh restarts are still needed to combat drift and tunnel vision" — [Cursor blog](https://cursor.com/blog/scaling-agents)
- [FACT] The C compiler used specialized parallel agents for:
  - Deduplicating repeated code.
  - Compiler performance.
  - Efficiency of the compiled output.
  - A "Rust developer"-style critic making structural and code-quality changes.
  - Documentation.
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] OpenAI:
  - Codex reviews its own changes, requests agent reviews locally and in the cloud, and loops until reviewers are satisfied (a "Ralph Wiggum Loop"). Review is "agent-to-agent"; humans may review but are not required to.
  - A recurring "doc-gardening" agent opens fix-up PRs for stale docs.
  - Background tasks scan for deviations from "golden principles", update quality grades, and open refactoring PRs.
  
  — [OpenAI](https://openai.com/index/harness-engineering/)
- [FACT] Gas Town roles:
  - Mayor: talks to the human and never writes code.
  - Polecats: workers with persistent identity and ephemeral sessions.
  - Witness: patrols the workers.
  - Refinery: merge queue.
  - Formulas: work templates.
  
  — [yegge.ai/gastown](https://yegge.ai/gastown); [Maggie Appleton](https://maggieappleton.com/gastown). [OPINION] "Giving each agent a single job means you can prompt them more precisely" — [Maggie Appleton](https://maggieappleton.com/gastown)
- [FACT] Claude Code agent teams support reusable role definitions (for example security-reviewer or test-runner) via subagent definition files. Parallel review splits lenses (security, performance, test coverage), and "competing hypotheses" debugging uses adversarial teammates. The docs recommend 3–5 teammates and 5–6 tasks per teammate; "Three focused teammates often outperform five scattered ones" — [Claude Code docs](https://code.claude.com/docs/en/agent-teams)
- [FACT] Anthropic's Nov 2025 harness post lists as open research whether a single general agent beats a multi-agent setup with specialized testing, QA or cleanup agents — [Anthropic Engineering](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)

### Inferences
Suggested roles for nami:
- **Planner:** decomposes the numerical-methods roadmap into crate- or module-scoped tasks with acceptance tests.
- **Workers:** implement one task each.
- **Merge-queue CI gate:** not an LLM.
- **Periodic janitors:** dedup and refactor, docs and AGENTS.md gardening, test-suite maintenance and speed, a performance and benchmark agent, and a "numerical analyst critic" reviewing stability, conservation and convergence claims.
- **Judge or restart cycle:** guards against drift.

Keep roles few and stateless, and keep state in the tracker and the repo.

### Gaps
- No controlled comparison of specialized versus generalist swarms on the same project was found. Anthropic flags this as an open question.
- I found no quantitative data on the false-positive or false-negative rates of AI review bots in swarm settings.

---

## 4. Tests as oracle: verifier quality, speed, determinism, output-noise reduction

### Takeaway
How good the verifier is sets the ceiling for the whole swarm. Carlini: "the task verifier is nearly perfect", otherwise Claude "will solve the wrong problem". Agent-oriented harnesses:
- Print very little and log details to grep-able files.
- Pre-compute summaries.
- Offer fast deterministic random-sample modes per agent.
- Protect tests from agent edits.
- Add CI to stop regressions.
- Split one monolithic failure into many independent ones, so agents can parallelize.

### Cited Findings
- [FACT] C compiler test-harness rules:
  - Print only a few lines and log details to files, with "ERROR" and the reason on the same line so it can be grepped.
  - Aggregate statistics are computed by the harness, not by Claude.
  - Progress output is infrequent, because of "time blindness".
  - A `--fast` option runs a 1% or 10% random sample, deterministic per agent but random across VMs.
  - Agents maintain extensive READMEs and progress files.
  - The author was "writing this test harness for Claude and not for myself".
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] Regressions were frequent until a CI pipeline was added late so that new commits couldn't break existing code. Parallelism only worked once there were many independent failing tests; one shared failure (the kernel) caused agents to collide until GCC was used as a differential oracle with delta debugging — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] FastRender's oracles were rustc compilation, screenshots compared to golden samples, progress diffs, and specs vendored as git submodules (WHATWG, CSSWG, TC39) that agents cited in comments. Vision models consumed the screenshots — [Simon Willison](https://simonwillison.net/2026/jan/23/fastrender)
- [FACT] OpenAI:
  - Custom linters and structural tests enforce a fixed layering (Types → Config → Repo → Service → Runtime → UI) and "taste invariants" (structured logging, naming, file-size limits).
  - Lint error messages contain remediation instructions for the agent.
  - The team enforces invariants, not implementations.
  - Linters and CI also verify the docs knowledge base is current and cross-linked.
  
  — [OpenAI](https://openai.com/index/harness-engineering/)
- [FACT] Anthropic's harness post:
  - JSON feature lists with `passes` flags; agents are forbidden from removing or editing tests.
  - End-to-end browser testing (Puppeteer MCP) substantially improved bug detection compared with unit tests or curl.
  - Each session starts by running `init.sh` and a basic end-to-end check.
  
  — [Anthropic Engineering](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
- [FACT] Claude Code hooks can enforce gates. A `TaskCompleted` hook exiting with code 2 blocks completion and returns feedback, which can be used to require tests to pass before a task is marked done — [Claude Code docs](https://code.claude.com/docs/en/agent-teams)
- [OPINION] "For autonomous systems, it is easy to see tests pass and assume the job is done, when this is rarely the case" — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)

### Inferences
For a finite-volume PDE framework, the equivalent of the "nearly perfect verifier" is a layered numerical oracle:
- Method-of-manufactured-solutions convergence-order tests, which assert the observed order within tolerance.
- Conservation checks to machine precision.
- Exact Riemann problem comparisons (Sod, Lax, etc.).
- Symmetry and Galilean-invariance tests.
- Differential tests against a reference implementation (for example an existing Python, C++ or Fortran FV code), analogous to GCC-as-oracle.
- Property tests on flux consistency.

Each should emit a one-line PASS/FAIL summary with grep-able `ERROR` lines and a deterministic `--fast` sampled mode. Test files and reference data should be write-protected from workers (CODEOWNERS or a hook). The many independent cases (schemes × limiters × BCs × dimensions) let the swarm parallelize the way the C compiler did on its torture tests.

### Gaps
- No primary quantitative data on how test-suite runtime affects swarm throughput (for example, PRs per hour versus CI minutes).
- I found no published case study of agent swarms on numerical or scientific-computing code specifically. Anthropic lists whether lessons generalize to scientific research as an open question.

---

## 5. Human role: review points, preventing drift, slop and architecture erosion, controlling cost

### Takeaway
In successful runs, humans moved from writing and reviewing code to designing:
- The harness, tests and specs.
- The architecture invariants, encoded as linters.
- The repo-resident documentation ("a map, not a 1,000-page manual").

Design and planning become the bottleneck. Drift and slop are handled by periodic fresh restarts, recurring cleanup agents and mechanical invariant enforcement. Cost runs away mainly through orchestration inefficiency and unsteerable long runs.

### Cited Findings
- [FACT] Carlini's role was to design the harness, tests, environment and feedback loops, and to write the initial spec (including an SSA IR design), but not the implementation — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [FACT] OpenAI:
  - Humans steer via prompts. Codex can take a feature end-to-end (reproduce, fix, video, PR, respond to feedback, merge) and "escalates to humans only when judgment is needed".
  - Anything not in the repo (Slack, Google Docs) is "effectively invisible" to agents.
  - A single large AGENTS.md failed: it crowded context, rotted and couldn't be mechanically checked. It was replaced by an AGENTS.md of about 100 lines that acts as a table of contents, with a structured `docs/` directory (design-docs, exec-plans with progress and decision logs, product-specs, references, quality and reliability) as the system of record.
  - Open question: how architectural coherence evolves over years.
  
  — [OpenAI](https://openai.com/index/harness-engineering/)
- [FACT] OpenAI spent 20% of the week on manual slop cleanup before automating garbage collection — [OpenAI](https://openai.com/index/harness-engineering/)
- [FACT] Cursor's Solid-to-React migration passed CI but "still needed careful review". Too little structure causes "conflicts, duplicated work, and drift"; too much causes fragility. Fresh restarts are needed against drift — [Cursor blog](https://cursor.com/blog/scaling-agents)
- [FACT] FastRender runs cannot be steered mid-run; the only option is to stop them. Agents vendored and modified a dependency (Taffy) unintentionally — [Simon Willison](https://simonwillison.net/2026/jan/23/fastrender)
- [OPINION] Yegge: "Gas Town churns through implementation plans so quickly that you have to do a LOT of design and planning." Appleton: humans must supply taste, product strategy and priorities, and "fast generation can skip design" — [Maggie Appleton](https://maggieappleton.com/gastown)
- [OPINION/SECONDARY] Yegge's AI Engineer 2026 talk "Software Factories: Escape from Trash Mountain" (per summary) argues that an orchestrator must "allocate intelligence" because tokens are "fuel" consumed by deliberation and extra workers — [ai.engineer](https://ai.engineer/code/2026/talks/steve-yegge)
- [FACT] Claude Code docs:
  - Token cost scales linearly with teammates.
  - "Letting a team run unattended for too long increases the risk of wasted effort."
  - The lead may start implementing itself or declare the work done early.
  - Teammates may stop on errors.
  - Start with research or review before parallel implementation.
  
  — [Claude Code docs](https://code.claude.com/docs/en/agent-teams)
- [FACT] On safety and cost, the C compiler loop ran with `--dangerously-skip-permissions` inside containers, and an agent killed its own harness with `pkill -9 bash` — [Anthropic Engineering](https://www.anthropic.com/engineering/building-c-compiler)
- [OPINION/SECONDARY] Public scepticism of showcase claims: "check the repo before believing the hype" — [The Register, Jan 26, 2026](https://www.theregister.com/2026/01/26/cursor_opinion/)

### Inferences
Human checkpoints for nami should sit at:
1. Architecture and spec: trait boundaries, mesh and field data layout, numerical scheme choices.
2. The test-oracle design and reference data.
3. Periodic review of planner output and task graphs.
4. Sampling-based audits of merged code, especially numerics.

Code-level review of every PR does not scale. Encode architecture invariants (crate dependency direction, no `unsafe` outside designated modules, file-size limits, banned dependencies, no edits to reference data) as CI-enforced lints with remediation messages.

To control cost:
- Set token and dollar budgets per run.
- Bound run duration and use judge or restart cycles.
- Cap concurrency to what the oracle can parallelize.
- Restrict dependency additions, for example with `cargo deny` and an allowlist.

### Gaps
- No published dollar-per-merged-PR or dollar-per-LoC figures beyond the C compiler.
- No longitudinal data (over 6 months) on architecture erosion in agent-built codebases. OpenAI explicitly lists this as unknown.
- No primary source quantifies how much human review time the large swarms (Cursor, OpenAI) actually consumed.
