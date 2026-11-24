# In-repo guidance, context files, and harness features for AI coding agents (as of Nov 2026)

Scope: how projects structure AGENTS.md/CLAUDE.md, progressive-disclosure docs, harness guardrails (hooks/skills/subagents/rules), spec-driven development, and TigerStyle, as input for tooling decisions on "nami" (a Rust finite-volume framework for hyperbolic PDEs written in TigerStyle and built mostly by agent swarms). Research date: 2026-11-24.

---

## 1. AGENTS.md / CLAUDE.md conventions and what the evidence says

### Takeaway
The cross-tool convention is a short root AGENTS.md (CLAUDE.md for Claude Code), with nested per-directory files where the nearest file wins. Vendors converge on about 100–200 lines, and Codex caps combined project docs at 32 KiB by default. Controlled studies (2026) find context files have little or no effect on task success: LLM-generated files are neutral-to-negative and cost about 20% more, while human-written files give a small, non-significant gain. Files are followed closely, so they reliably change behaviour (tool choice, testing). Overviews do not help navigation. The value is in non-obvious, repo-specific commands, conventions and constraints, not in summaries of the codebase.

### Cited Findings

**Format, stewardship, adoption, nesting**
- AGENTS.md is "a README for agents". It is plain Markdown with no required fields and is now stewarded by the Agentic AI Foundation under the Linux Foundation. The site claims use by "over 60k open-source projects". Recommended sections: project overview, build and test commands, code style, testing instructions, security considerations, plus optional PR/commit guidelines and deployment steps — [agents.md](https://agents.md/)
- Nesting rule: "The closest AGENTS.md to the edited file wins; explicit user chat prompts override everything." The main OpenAI repo reportedly has 88 AGENTS.md files — [agents.md](https://agents.md/)
- Supported by Codex, Jules, Factory, Aider, goose, opencode, Zed, Warp, VS Code, Devin, Junie, Amp, Cursor, RooCode, Gemini CLI, Kilo Code, GitHub Copilot coding agent, Windsurf, Augment and others — [agents.md](https://agents.md/)
- **Codex:** walks from the git root down to the cwd and includes at most one file per directory, checking `AGENTS.override.md` first, then `AGENTS.md`, then configurable `project_doc_fallback_filenames`. Files are concatenated root-first, so "files closer to your current directory override earlier guidance because they appear later in the combined prompt." Codex stops adding files once the combined size reaches `project_doc_max_bytes` (default **32 KiB**). It also has a global `~/.codex/AGENTS.md` — [OpenAI Codex docs: AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md) (formerly developers.openai.com/codex/guides/agents-md)
- **Claude Code:**
  - CLAUDE.md files in the cwd and its ancestors load at launch. Subdirectory CLAUDE.md files load lazily, the first time Claude reads or edits a file in that directory.
  - `@path` imports are supported, recursing up to 4 hops. Imports "don't reduce its context cost."
  - Size guidance: "target under 200 lines per CLAUDE.md file. Longer files consume more context and reduce adherence."
  - Contradictory instructions: "Claude may pick one arbitrarily."
  - `/doctor prompt-audit` audits instruction files. `claudeMdExcludes` skips irrelevant ancestor files in monorepos.
  - Source: [Claude Code docs: memory](https://code.claude.com/docs/en/memory)
- **Claude Code and AGENTS.md:** since v2.1.277, Claude Code reads AGENTS.md natively, but only when no CLAUDE.md exists in the cwd or above it. If both exist, only CLAUDE.md is read unless CLAUDE.md imports AGENTS.md with `@AGENTS.md`. The common cross-tool pattern is therefore a one-line CLAUDE.md that imports AGENTS.md, or a symlink — [Claude Code docs: memory](https://code.claude.com/docs/en/memory)
- **Cursor:** supports AGENTS.md at the root and in subdirectories, where "more specific instructions taking precedence". It also has its own `.cursor/rules/*.mdc` (see section 3) — [Cursor docs: rules](https://cursor.com/docs/context/rules)

**Practitioner guidance (HumanLayer, Nov 2025)**
- Structure the file as WHAT (stack, map of the codebase), WHY (purpose of each part) and HOW (how to work on it, especially how to verify changes with tests, typecheck and build) — [HumanLayer: Writing a good CLAUDE.md](https://www.humanlayer.dev/blog/writing-a-good-claude-md)
- "< 300 lines is best, and shorter is even better". The author's own root file is under 60 lines — [HumanLayer](https://www.humanlayer.dev/blog/writing-a-good-claude-md)
- Instruction budget: "Frontier thinking LLMs can follow ~ 150-200 instructions with reasonable consistency." Claude Code's own system prompt already contains about 50 instructions. Adherence degrades uniformly as count rises, and models favour the beginning and end of the prompt — [HumanLayer](https://www.humanlayer.dev/blog/writing-a-good-claude-md)
- Claude Code wraps CLAUDE.md in a reminder that it "may or may not be relevant to your tasks", so non-universal content is more likely to be ignored — [HumanLayer](https://www.humanlayer.dev/blog/writing-a-good-claude-md)
- Further advice — [HumanLayer](https://www.humanlayer.dev/blog/writing-a-good-claude-md):
  - "Never send an LLM to do a linter's job." Use Stop hooks plus deterministic formatters and linters instead.
  - "Prefer pointers to copies": use `file:line` references rather than pasted snippets.
  - Don't auto-generate the file with `/init`.

**Instruction-count and long-context evidence (the "context rot" family)**
- IFScale (Distyl AI, July 2025; NeurIPS 2025) tested 20 models at 10–500 simultaneous instructions. The best frontier models reached only **68% accuracy at 500 instructions**. Decay patterns vary by model, with a bias toward earlier instructions — [arXiv 2507.11538](https://arxiv.org/abs/2507.11538)
- Chroma's "Context Rot" (July 2025) evaluated 18 LLMs. Performance "grows increasingly unreliable as input length grows" even on trivial tasks such as repeating a word. Degradation is worse when needle and question are less semantically similar and when distractors are present. Caveat: Chroma sells retrieval infrastructure — [Chroma research](https://research.trychroma.com/context-rot)
- Anthropic (Sep 29, 2025) on context:
  - Context is a finite "attention budget", and each added token draws it down.
  - System prompts should sit at the "right altitude", between brittle if-else logic and vague guidance.
  - Claude Code's hybrid approach puts CLAUDE.md up front and retrieves everything else just-in-time with glob/grep.
  - Source: [Anthropic: Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)

**Controlled studies of context files (2026)**
- **Gloaguen, Mündler-Sasahara, Vechev (ETH Zurich) and Müller, Raychev (LogicStar.ai), arXiv 2602.11988 (v1 Feb 2026, v3 Sep 2026; ICLR 2026)** — [arXiv HTML v3](https://arxiv.org/html/2602.11988v3)
  - Setup: SWE-bench Lite (300 tasks) plus CTXbench (138 instances from 12 niche repos with developer-written context files). Agents: Claude Code/Sonnet-4.5, Codex/GPT-5.2 and GPT-5.1 mini, Qwen Code.
  - LLM-generated files: resolution changed by −0.5% (SWE-bench) and −2% (CTXbench). Neither change is significant (p=0.87, 0.37). Cost rose **+20–23%** and steps rose by about 2.5–3.9.
  - Developer-written files: about **+2.4%** success (not significant, p=0.21). They significantly beat LLM-generated files (p=0.038), but cost up to 19% more.
  - "Repository overviews, although popular and recommended by model providers, are not helpful." Agents took as many steps to reach the relevant files as they did without context files.
  - Instructions are followed. `uv` was used 1.6 times per instance when mentioned, versus under 0.01 when not. Repo-specific tools: 2.5 uses versus under 0.05.
  - Length and content categories showed no clear effect.
  - When the repo's own docs were removed, LLM-generated files helped by about +2.7%. Context files therefore substitute for missing documentation.
  - Recommendation: human-written files "should only contain specific additional instructions beyond what is already available in the codebase."
  - Note: one search-engine summary described this paper as finding files "reduce task success". The paper's own numbers show a small, non-significant change, so cite the paper directly.
- **Lulla, Mohsenimofidi et al., arXiv 2601.20404 (Jan 2026, ICSE JAWs 2026 workshop):** 10 repos, 124 PRs. AGENTS.md was associated with a **28.64% lower median runtime** and **16.58% fewer output tokens**, with task completion roughly unchanged. This contradicts the ETH cost finding. The difference is plausibly because these were real developer files, measured on efficiency rather than success — [arXiv 2601.20404](https://arxiv.org/abs/2601.20404)
- **McMillan, arXiv 2605.10039 (May 2026):** a factorial study of 1,650 Claude Code sessions and 16,050 function-level observations across two TypeScript codebases and three frontier models.
  - None of the four file-structure variables showed a detectable effect on compliance: **size, position, conflict, architecture**. Size and conflict nulls have Bayes-factor support (BF10 0.05–0.10).
  - The largest effect was within-session drift: each additional generated function lowered the odds of compliance by about **5.6%** (OR 0.944). This was found post hoc and replicated on a second codebase and on Opus 4.6.
  - Source: [arXiv 2605.10039](https://arxiv.org/abs/2605.10039)
- **Khatri, arXiv 2607.27250 (Jul 2026):** 17 tasks, 3 repos, 288 runs with Claude Code and Codex. Context strategy "does not measurably move correctness", bounded within ±10–15 percentage points.
  - Failures came from implementation skill (design, pattern selection, wiring), not missing repo knowledge.
  - The real AGENTS.md never turned a near-miss into a pass.
  - Source: [arXiv 2607.27250](https://arxiv.org/abs/2607.27250)
- **dos Santos, Valente et al., arXiv 2606.15828 (SCAM 2026):** the first catalogue of six "configuration smells" in AGENTS.md/CLAUDE.md, measured across 100 popular repos.
  - **Lint Leakage** (62% of files): style rules a linter should enforce.
  - **Context Bloat** (42%).
  - **Skill Leakage** (35%): procedures that belong in skills.
  - **Conflicting Instructions**, which often co-occurs with the above.
  - Source: [arXiv 2606.15828](https://arxiv.org/abs/2606.15828)

### Inferences
- For nami, keep the root AGENTS.md short (aim for 60–150 lines). Fill it with things an agent cannot infer:
  - exact `cargo`/`just` commands and the test/verify loop;
  - TigerStyle non-negotiables that tools can't check;
  - numerical-method invariants (conservation, CFL, positivity);
  - where the docs map lives.
- Don't spend lines on an architecture overview inside AGENTS.md. The ETH finding says overviews don't speed navigation, so put the map in ARCHITECTURE.md and point to it.
- Use one canonical AGENTS.md plus a CLAUDE.md that imports it (`@AGENTS.md`), so Codex, Cursor and Claude Code agents in a mixed swarm all get the same rules.
- Within-session compliance drift (McMillan) argues for small tasks per agent session and for re-injection or verification via hooks, not just longer files.
- Since files substitute for documentation (ETH supplementary result), a well-documented repo plus a lean AGENTS.md is the evidence-backed combination.

### Gaps
- No study found specifically on numerical/scientific Rust codebases, or on swarms (many concurrent agents) rather than single agents.
- The full text of McMillan and of the smells paper (smell definitions beyond the top 3–4, and detection heuristics) was not readable, because PDF extraction failed. Only the abstracts were used.
- arXiv 2610.21421 ("The Role of Agentic Instruction Categories for Code Generation") surfaced in search but was not reviewed.

---

## 2. Progressive disclosure: docs/ layout, ARCHITECTURE.md, plans, progress files

### Takeaway
The best-documented large-scale agent-first practice (OpenAI's "harness engineering", Feb 2026) uses about a 100-line AGENTS.md as a map. A structured, indexed `docs/` acts as the system of record, with design docs, exec plans and quality grades. CI linters keep the docs fresh, and recurring "doc-gardening" agents fix drift. Anthropic's long-running-agent pattern adds machine-readable state: a JSON feature list with pass/fail, a progress log, git history, and an `init.sh` smoke test.

### Cited Findings
- **OpenAI, "Harness engineering" (Ryan Lopopolo, Feb 11, 2026)** — [OpenAI](https://openai.com/index/harness-engineering/)
  - Scale: about 1M Lines of Code (LOC) with zero hand-written lines, built over about 5 months. About 1,500 merged PRs from 3 growing to 7 engineers, roughly 3.5 PRs per engineer per day. Single Codex runs often last over 6 hours.
  - AGENTS.md is about 100 lines: "give Codex a map, not a 1,000-page instruction manual." They tried one big file first, and it failed: it "crowd[s] out the task", "everything look[s] important", it rots, and it is hard to verify.
  - Repo layout: `AGENTS.md`, `ARCHITECTURE.md` (domains and package layering), and `docs/` containing:
    - `design-docs/`: indexed, with verification status and "core beliefs";
    - `exec-plans/{active,completed}/` and `tech-debt-tracker.md`;
    - `generated/` (for example a DB schema doc);
    - `product-specs/`;
    - `references/`: LLM-oriented docs for external dependencies;
    - top-level files `DESIGN.md`, `PLANS.md`, `QUALITY_SCORE.md`, `RELIABILITY.md`, `SECURITY.md` and others.
  - Plans are "first-class artifacts". Complex work gets checked-in exec plans with progress and decision logs, while small changes use ephemeral plans.
  - QUALITY_SCORE grades each domain and layer and tracks gaps over time.
  - Mechanical enforcement:
    - Layers are fixed (Types → Config → Repo → Service → Runtime → UI), with dependencies only "forward". Cross-cutting concerns enter through a Providers interface. Custom linters and structural tests check this, plus file-size limits and naming.
    - "Because the lints are custom, we write the error messages to inject remediation instructions into agent context."
    - "Enforce invariants, not implementations."
  - Legibility: "anything it can't access in-context while running effectively doesn't exist". Knowledge held in Google Docs, Slack or people's heads must be encoded as repo markdown. They preferred "boring" in-repo dependencies, and sometimes had the agent reimplement small utilities.
  - Entropy control: Codex replicates existing patterns, including bad ones. Before automation, cleaning up "AI slop" took about 20% of the week (Fridays). They now run recurring background agents that check against "golden principles", update quality grades and open small refactor PRs. A doc-gardening agent opens fix-up PRs for stale docs.
- **Anthropic, "Effective harnesses for long-running agents" (Nov 26, 2025)** — [Anthropic engineering](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)
  - Two prompts share one harness: an **initializer agent** that runs once, and **coding agents** that run every later session.
  - The initializer writes a JSON feature list (over 200 items for a claude.ai clone). Each entry has `category`, `description`, `steps` and `passes`, all starting as failing. Coding agents may only flip `passes`: "It is unacceptable to remove or edit tests…"
  - JSON was chosen because the model was less likely to tamper with it than with Markdown.
  - A `claude-progress.txt` file plus git history let a fresh agent quickly understand state, which the article calls "the key insight". Agents commit with descriptive messages so bad changes can be reverted.
  - `init.sh` starts the environment. Each session runs it and a basic end-to-end check before starting new work.
  - One feature per session.
  - Failure modes: trying to one-shot the whole app, declaring victory early, leaving undocumented or buggy state, and marking features done without end-to-end testing. The fix for the last was browser automation (Puppeteer MCP).
- **Anthropic, context engineering (Sep 29, 2025)** — [Anthropic](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
  - Structured note-taking outside the context window (for example NOTES.md and to-do lists).
  - Compaction, where clearing old tool results is the safest form.
  - Subagents return condensed summaries, "often 1,000-2,000 tokens".
- **HumanLayer:** an `agent_docs/` directory of descriptively named files (building, testing, conventions, architecture), listed in CLAUDE.md with one-line descriptions so the agent reads them on demand — [HumanLayer](https://www.humanlayer.dev/blog/writing-a-good-claude-md)
- **Cursor:** "Reference files instead of copying their contents"; "Start simple. Add rules only when you notice Agent making the same mistake repeatedly." — [Cursor docs](https://cursor.com/docs/context/rules)

### Inferences
- A nami layout following the OpenAI pattern:
  - `AGENTS.md` as the map;
  - `ARCHITECTURE.md` covering crate layering (for example core types → mesh → state/storage → fluxes/Riemann solvers → time integration → I/O → drivers), plus which layers may depend on which;
  - `docs/design/` with ADR-style design docs that carry a status field;
  - `docs/exec-plans/{active,completed}/`;
  - `docs/glossary.md` (PDE and FV terms: conserved vs primitive variables, ghost cells, CFL, flux limiter, and so on);
  - `docs/references/` (numerical-method notes and LLM-oriented summaries of the few allowed deps);
  - `docs/quality.md` grading each module.
- Enforce the layering with a structural test (for example a cargo-metadata or `cargo deny` check of crate dependency direction). Write the error message as remediation text, in the style OpenAI describes.
- For swarms, the Anthropic JSON feature/progress-file pattern maps naturally to a `docs/exec-plans/<plan>/tasks.json`. Each task carries a `passes` flag that only a verification hook or CI may flip, plus an append-only progress log per plan.
- Budget for doc gardening as a recurring agent job from the start, because pattern-replication drift is the documented dominant failure at scale.

### Gaps
- OpenAI does not publish the QUALITY_SCORE grading scale or the actual AGENTS.md text.
- I found no quantitative evidence on ADRs specifically for agents, nor on glossaries' measured effect. Their use here is practitioner reasoning only.

---

## 3. Harness features for project-level guardrails (Claude Code, Codex, Cursor)

### Takeaway
Instructions are requests and hooks are enforcement. Claude Code documents this explicitly. Claude Code offers a layered toolkit:
- CLAUDE.md, always loaded;
- `.claude/rules/` with path-scoped frontmatter, loaded on file match;
- skills, where only the description loads until the skill is used;
- subagents, which run in isolated context;
- hooks, deterministic and around 30 lifecycle events;
- plugins, for packaging.

Cursor mirrors the rules model with `.mdc` frontmatter, and Codex relies mainly on layered AGENTS.md files.

### Cited Findings
- "Put guardrails in hooks. An instruction like 'never edit `.env`' in CLAUDE.md or a skill is a request, not a guarantee. A `PreToolUse` hook that blocks the edit is enforcement." — [Claude Code docs: features overview](https://code.claude.com/docs/en/features-overview)
- **Context cost by feature** — [features overview](https://code.claude.com/docs/en/features-overview):
  - CLAUDE.md: full content on every request.
  - Skills: only the description on every request, with full content on use. `disable-model-invocation: true` gives zero cost until invoked.
  - MCP: tool names only, with schemas deferred.
  - Subagents: isolated context.
  - Hooks: zero cost unless they return output.
- **Suggested order of adoption** — [features overview](https://code.claude.com/docs/en/features-overview):
  - CLAUDE.md after a mistake happens twice.
  - A skill once a playbook has been pasted three times.
  - A subagent for noisy side tasks.
  - A hook for anything that must always happen.
  - A plugin once a second repo needs the same setup.
- **Path-scoped rules:** `.claude/rules/*.md` with YAML `paths:` globs, for example `src/api/**/*.ts`. A rule loads only when Claude reads or edits a matching file. Rules are discovered recursively and can be symlinked — [Claude Code docs: memory](https://code.claude.com/docs/en/memory)
- **Precedence** — [features overview](https://code.claude.com/docs/en/features-overview):
  - CLAUDE.md files are additive.
  - Skills override by name (managed > user > project).
  - Subagents override by name (managed > CLI flag > project > user > plugin).
  - Hooks merge from all sources.
- **Subagents** — [features overview](https://code.claude.com/docs/en/features-overview):
  - Each gets its own system prompt, CLAUDE.md, git status, and the skills listed in its `skills:` field.
  - The built-in Explore/Plan agents start without CLAUDE.md, and `omitClaudeMd` disables it per agent.
  - "Dynamic workflows" are scripts that run many subagents. Agent teams have a `TeammateIdle` hook.
- **Hook events (Claude Code, Nov 2026)** — [Claude Code docs: hooks guide](https://code.claude.com/docs/en/hooks-guide):
  - Session and prompt: SessionStart, Setup, UserPromptSubmit, UserPromptExpansion, SessionEnd.
  - Tools and permissions: PreToolUse (can block), PermissionRequest, PermissionDenied, PostToolUse, PostToolUseFailure, PostToolBatch.
  - Subagents and tasks: SubagentStart, SubagentStop, TaskCreated, TaskCompleted, TeammateIdle.
  - Turn end: Stop, StopFailure.
  - Instructions and config: InstructionsLoaded, ConfigChange, CwdChanged, FileChanged.
  - Worktrees: WorktreeCreate, WorktreeRemove.
  - Compaction: PreCompact, PostCompact.
  - Also Notification, Elicitation, and model-switch events.
- **Hook types:** `command`, `http`, `mcp_tool`, `prompt` (single-turn LLM judgement) and `agent` (multi-turn verifier with tools; experimental). Example agent hook prompt: "Verify that all unit tests pass. Run the test suite and check the results." — [hooks guide](https://code.claude.com/docs/en/hooks-guide)
- **Hook mechanics** — [hooks guide](https://code.claude.com/docs/en/hooks-guide):
  - Exit code 2 blocks the action and feeds stderr back to Claude. Alternatively, return JSON `{"decision": "block", "reason": ...}`.
  - For PreToolUse, the most restrictive decision across all hooks wins (deny > defer > ask > allow).
  - A Stop hook that blocks is overridden after 8 consecutive blocks with no tool call. Scripts should check `stop_hook_active` to avoid infinite loops.
- **Canonical examples** — [hooks guide](https://code.claude.com/docs/en/hooks-guide):
  - Auto-format: a PostToolUse hook with matcher `Edit|Write` runs a formatter on `tool_input.file_path`, for example `jq -r '.tool_input.file_path'` piped into prettier. Use rustfmt for Rust.
  - Protected files: a PreToolUse `Edit|Write` hook script exits 2 on patterns like `.env`, lockfiles or `.git/`.
  - Caveat from the docs: Claude can still use the shell to edit, so add `permissions.deny` rules such as `Edit(/.env)` as well.
- **Practitioner pattern:** a Stop hook runs the formatter and linter and feeds errors back, instead of putting style rules in CLAUDE.md — [HumanLayer](https://www.humanlayer.dev/blog/writing-a-good-claude-md). The smells study found "Lint Leakage" in 62% of files — [arXiv 2606.15828](https://arxiv.org/abs/2606.15828)
- **Cursor** — [Cursor docs: rules](https://cursor.com/docs/context/rules):
  - `.cursor/rules/*.mdc` files with frontmatter `description`, `globs` and `alwaysApply`.
  - Four modes: Always Apply, Apply Intelligently (by description), Apply to Specific Files (globs), Apply Manually (@-mention).
  - Keep rules under 500 lines. Don't copy style guides; use a linter.
- **Codex:** layered AGENTS.md / AGENTS.override.md with a 32 KiB cap — [Codex docs](https://learn.chatgpt.com/docs/agent-configuration/agents-md). OpenAI's harness relies on custom linters and structural tests with remediation-oriented messages rather than harness hooks — [OpenAI](https://openai.com/index/harness-engineering/)

### Inferences
- **Hooks for nami:**
  - PostToolUse on `Edit|Write` of `*.rs` runs `rustfmt` on the file.
  - PreToolUse blocks edits to golden reference solutions, convergence-test baselines, `Cargo.lock`/`deny.toml`, and the TigerStyle doc itself.
  - The Stop or SubagentStop hook runs `cargo clippy -D warnings`, a custom TigerStyle lint pass and fast tests, and returns failures (guarding with `stop_hook_active`).
  - TaskCompleted gates on the verification command.
- **Path-scoped rules for nami:**
  - `paths: crates/*/src/**/*.rs`: TigerStyle Rust specifics.
  - `paths: crates/riemann/**`: solver invariants.
  - `paths: tests/convergence/**`: rules for the order-of-accuracy test harness.
- **Skills:** `/add-flux-scheme`, `/add-equation-system`, `/convergence-study`, and a `tigerstyle-review` reference skill. Mark side-effecting skills `disable-model-invocation`.
- **Subagents:** a reviewer subagent (TigerStyle and assertion-density audit) and a numerics-verifier subagent. Package all of the above as a project plugin if other repos need it.
- **Mechanical checks:** many TigerStyle rules can be checked mechanically in Rust:
  - function ≤70 lines: clippy `too_many_lines` with its threshold configured;
  - no recursion;
  - assertion density, via a custom script counting `assert!`/`debug_assert!` per fn;
  - no allocation after init, via a counting global allocator in tests;
  - bounded loops, which are harder to check.
  
  Put these in CI and linters with remediation messages, per OpenAI's approach, rather than in prose.

### Gaps
- No controlled evidence was found on whether hooks or skills improve agent success rates. Support for them is vendor guidance and practitioner reports.
- I did not verify Codex's or Cursor's own hook equivalents in their late-2026 docs. Cursor and Codex have shipped hook-like features, but these weren't fetched.

---

## 4. Spec-driven development (Spec Kit, Kiro, OpenSpec, Tessl)

### Takeaway
Spec-driven development (SDD) tools structure work as a chain of markdown artifacts, roughly constitution/steering → requirements/spec → design/plan → tasks. Evidence of benefit is weak and mostly anecdotal or vendor-produced:
- Thoughtworks' Böckeler found heavy SDD overkill for small changes, with a high review burden, and found agents still ignoring spec content.
- Lighter tools (OpenSpec) are reported cheaper in tokens and better for brownfield work.
- Benefits appear concentrated on multi-file features.

### Cited Findings
- **Böckeler (Thoughtworks), "Understanding SDD: Kiro, spec-kit, and Tessl", Oct 15, 2025** — [martinfowler.com](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)
  - Defines **spec-first** (spec guides the current task), **spec-anchored** (spec kept and evolved with the feature) and **spec-as-source** (humans edit only the spec). "All SDD approaches… I've found are spec-first."
  - **Kiro:** Requirements → Design → Tasks in `requirements.md`, `design.md`, `tasks.md`. Requirements are user stories with GIVEN/WHEN/THEN acceptance criteria. Steering files are `product.md`, `structure.md`, `tech.md`. Tasks trace to requirement numbers.
  - **GitHub Spec Kit:** Constitution → Specify → Plan → Tasks. Each feature gets a folder `specs/NNN-feature/` with `spec.md`, `plan.md`, `tasks.md`, `research.md`, `data-model.md` and contract files, plus its own branch. The constitution holds the "immutable principles".
  - **Tessl:** one `.spec.md` maps 1:1 to a generated file marked "GENERATED FROM SPEC - DO NOT EDIT".
  - Criticisms:
    - Kiro turned a small bug into "4 'user stories' with a total of 16 acceptance criteria".
    - Spec Kit "felt like overkill" for a 3–5 point feature: "I'd rather review code than all these markdown files".
    - The agent ignored Spec Kit's research notes and duplicated existing classes, and sometimes over-applied the constitution.
    - Spec-as-source risks "inflexibility and non-determinism", like the old model-driven development (MDD).
- **OpenSpec vs Spec Kit (secondary, low-rigour sources)** — [search summary of OpenSpec vs Spec Kit comparisons](https://ypyl.github.io/programming/2026/06/03/openspec-vs-spec-kit-sdd.html), [Thoughtworks Radar: OpenSpec](https://www.thoughtworks.com/en-gb/radar/tools/openspec), [dev.to comparison](https://dev.to/willtorber/spec-kit-vs-bmad-vs-openspec-choosing-an-sdd-framework-in-2026-d3j):
  - One blog measured about 92k tokens (OpenSpec) versus about 181k (Spec Kit), with the gap coming from Spec Kit's planning phase.
  - A Feb 2026 hands-on scoring (Ran Isenberg) favoured OpenSpec for brownfield work.
  - A vendor benchmark (Uvik, 50 Python tickets, 5 workflows including a no-spec control) reportedly found spec workflows helped most on feature and data-pipeline tickets touching 3 or more files.
  - Thoughtworks Radar reportedly placed OpenSpec in "Assess" (April 2026).
  - These came via search summaries and were not individually verified. Treat them as low confidence.
- **Related primary evidence:** the Anthropic harness's JSON feature list with `passes` flags is effectively a minimal, machine-checkable spec and task list — [Anthropic](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents). OpenAI uses `product-specs/` and exec plans with decision logs — [OpenAI](https://openai.com/index/harness-engineering/)
- Khatri's ablation found agent failures stem from implementation skill (design, wiring) rather than missing repo knowledge. This is consistent with planning and design artifacts, not more static context, being the useful lever, though the study did not test specs — [arXiv 2607.27250](https://arxiv.org/abs/2607.27250)

### Inferences
- For nami, a lightweight spec-anchored approach fits better than a heavy SDD framework:
  - a short `constitution`, which is simply the TigerStyle-for-nami doc plus core numerical invariants;
  - per-feature design docs with equations, discretisation, boundary conditions, stability limits and acceptance criteria expressed as executable verification tests (convergence order on manufactured solutions, conservation to machine precision, positivity), plus a `tasks.json` with `passes` flags.
- Hyperbolic-PDE work has unusually crisp, testable specs (exact Riemann solutions, the method of manufactured solutions (MMS), known convergence orders). That makes "spec = executable test" much more valuable than prose user stories.
- Avoid Kiro-style user-story inflation for small tasks. Scale planning artifacts to change size, as OpenAI does with ephemeral versus checked-in plans.

### Gaps
- No peer-reviewed controlled study of SDD tools' effect on agent success was found.
- The Uvik and Isenberg numbers were not verified at source.

---

## 5. TigerStyle: how TigerBeetle documents it, and adaptations for agents

### Takeaway
TIGER_STYLE.md is a prose essay of rules with rationale, ordered by design goals (safety > performance > developer experience). It is long and principle-heavy, which suits a reference doc or skill, not an always-loaded context file. Community TigerStyle "skills" exist, mostly Zig-focused plus a language-agnostic one. No Rust-specific agent adaptation or official TigerBeetle AGENTS.md guidance was found.

### Cited Findings
- **Structure** — [TigerBeetle TIGER_STYLE.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md):
  - Sections: The Essence of Style; Why Have Style; On Simplicity and Elegance; Technical Debt ("zero technical debt"); Safety; Performance; Developer Experience; The Last Stage.
  - Every rule is motivated: "Always motivate, always say why."
- **Safety rules** — [TIGER_STYLE.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md):
  - Simple, explicit control flow with no recursion.
  - "Put a limit on everything": all loops and queues bounded, and non-terminating loops asserted.
  - Explicitly sized integers (`u32`) rather than `usize`.
  - At least **2 assertions per function** on average, covering pre- and postconditions and invariants.
  - **Pair assertions**: check a property at two code paths.
  - Assert both the **positive and negative space**.
  - Compile-time assertions.
  - **Static allocation**: all memory allocated at startup.
  - **70-line function limit**: parents hold control flow, helpers stay pure.
  - Handle all errors.
  - Batch external events.
  - "Assertions are a safety net, not a substitute for human understanding."
- **Performance and developer-experience rules** — [TIGER_STYLE.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md):
  - Back-of-envelope performance sketches across network, disk, memory and CPU; batching.
  - snake_case naming with no abbreviations and units or qualifiers last (`latency_ms_max`).
  - Explicit options at call sites.
  - Index, count and size treated as distinct.
  - 100-column limit with 4-space indent.
  - "Zero dependencies" beyond the toolchain; scripts in Zig rather than shell.
- **Community skills:** TigerStyle SKILL.md packages exist, including a Zig-focused one (writing, analysis and quick-check modes), a language-agnostic variant (fotoetienne/agent-essentials) that "translates Zig-specific rules" to the target language, and zig-tiger-style on skills.sh. No Rust TigerStyle port was found — [claudskills.com tiger-style](https://claudskills.com/skills/tiger-style/), [skillsmp fotoetienne tiger-style](https://skillsmp.com/creators/fotoetienne/agent-essentials/plugins-tiger-style-skills-tiger-style), [skills.sh zig-tiger-style](https://www.skills.sh/zigcc/skills/zig-tiger-style)
- **Rust project example:** one Rust project's agent docs list a TIGER_STYLE.md alongside AGENTS.md and a "Grug Brain" simplicity guide — [checkle: Working with AI Agents](https://nrminor.github.io/checkle/contributing/ai-agents.html). This is a single small example.
- **Rust compiler's own AGENTS.md:** exists at the repo root as an example of a major Rust project adopting the format, but was not reviewed in detail — [rust-lang/rust AGENTS.md](https://raw.githubusercontent.com/rust-lang/rust/HEAD/AGENTS.md)

### Inferences
- For nami, split TigerStyle three ways:
  1. A Rust-adapted `docs/TIGER_STYLE.md` with full rationale, written by humans.
  2. About 10–15 one-line non-negotiables in AGENTS.md:
     - no recursion;
     - bounded loops with an asserted upper bound;
     - no heap allocation after init;
     - ≥2 asserts per fn, covering both positive and negative space;
     - ≤70-line fns;
     - explicit integer widths;
     - units last in names;
     - zero new dependencies without an ADR.
  3. Mechanical enforcement through clippy config (`too_many_lines`, `disallowed_methods`/`disallowed_types` for `Vec::push` outside init and `Box::new`), a custom lint or script, a counting allocator in tests, and hooks.
- A path-scoped rule or a `tigerstyle-rust` skill plus a reviewer subagent can carry the detailed guidance on demand.
- Rust-specific translation decisions need writing down because agents will otherwise guess:
  - `usize` is unavoidable for indexing, so state the policy;
  - `debug_assert!` vs `assert!` policy, given that TigerBeetle keeps assertions on in production;
  - arena/pool patterns for static allocation;
  - `const` assertions (`const _: () = assert!(...)`).
- Writing TigerStyle as rules with a "why" matches the "right altitude" advice. Very long prose in the always-loaded file would cut against the instruction-budget findings, so keep the full essay out of AGENTS.md.

### Gaps
- No evidence was found on whether agents follow TigerStyle-like constraints well, or on assertion-density outcomes for agent-written code.
- I did not check whether TigerBeetle's repo itself has an AGENTS.md/CLAUDE.md or a stated policy on AI contributions; this was not found in this pass.
- The language-agnostic TigerStyle skill's Rust guidance was not inspected.
