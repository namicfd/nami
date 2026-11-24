# Mechanical guardrails for Rust projects built by AI agents (lints, formatting, CI gates, testing): state as of late Nov 2026

Context: "nami", a Rust finite-volume framework for hyperbolic PDEs, written in TigerStyle and implemented mostly by AI agent swarms. Toolchain reference point: the clippy CHANGELOG says **Rust 1.100 is "Current stable, released 2026-11-12"**. See [clippy CHANGELOG](https://github.com/rust-lang/rust-clippy/blob/master/CHANGELOG.md). All lint groups, versions and default thresholds below come from the clippy source at master commit `560923e` (2026-11-24): `declare_clippy_lint!` blocks in [clippy_lints/src](https://github.com/rust-lang/rust-clippy/tree/master/clippy_lints/src) and [clippy_config/src/conf.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_config/src/conf.rs).

Crate versions come from the crates.io API on 2026-11-24 (`https://crates.io/api/v1/crates/<name>`): cargo-nextest 0.9.152, proptest 1.12.0, quickcheck 1.1.0, cargo-fuzz 0.13.2, bolero 0.13.7, arbitrary 1.5.0, insta 1.49.0, cargo-mutants 27.1.0, kani-verifier 0.68.0, loom 0.7.3, shuttle 0.9.10, turmoil 0.7.2, madsim 0.2.34, cargo-deny 0.22.1, cargo-audit 0.23.0, cargo-vet 0.10.3, cargo-auditable 0.7.7, cargo-machete 0.10.1, cargo-shear 1.14.1, cargo-udeps 0.1.61, cargo-semver-checks 0.51.0, cargo-hack 0.6.46, cargo-msrv 0.19.6, cargo-llvm-cov 0.9.2, cargo-careful 0.4.10, criterion 0.8.2, divan 0.1.21 (last release 2025-04-10), iai-callgrind 0.16.1 (last release 2025-07), gungraun 0.20.0, codspeed-criterion-compat/codspeed-divan-compat 6.1.0, cargo-dylint/dylint-link 6.4.0, assert_no_alloc 1.1.2 (last release 2021-08), stats_alloc 0.1.10 (2022), allocation-counter 0.8.1 (2023), no-panic 0.1.37.

---

## Q1. Clippy and rustc lint configuration for TigerStyle (workspace `[lints]`, pedantic/restriction lints, clippy.toml thresholds, forbid(unsafe_code), deny(warnings); which lints are noisy for numeric code)

### Takeaway
Clippy covers about half of TigerStyle mechanically: panics and unwraps, indexing, integer overflow, casts, function length, complexity, nesting, float equality, assertion messages, unsafe, and `#[allow]` hygiene. Configure it once in `[workspace.lints]`, with `pedantic` at `priority = -1` and individually chosen `restriction` lints. Tune thresholds in `clippy.toml`. Gate CI with `cargo clippy --all-targets --all-features -- -D warnings`. Clippy has **no lint that bans recursion in general** (only `unconditional_recursion`). Its `too_many_lines` counts only code lines with a default limit of 100, so it is not equivalent to TigerStyle's 70 physical lines.

### Cited Findings

**Cargo `[lints]` mechanics**
- `[lints]` has been respected since Rust 1.74. It has `rust`, `clippy` and `cargo` tables, and the short form `x = "deny"` expands to `{ level = "deny", priority = 0 }`. `priority` is a signed integer: lower numbers are overridden by higher ones and are emitted first on the rustc command line. Lints apply only to the current package, never to dependencies (non-path dependencies are capped with `--cap-lints`). See the [Cargo manifest reference: lints](https://doc.rust-lang.org/cargo/reference/manifest.html).
- Bevy notes that Cargo "does not currently support overriding workspace lints inside a particular crate" ([rust-lang/cargo#13157](https://github.com/rust-lang/cargo/issues/13157)). Bevy duplicates the whole lint table into crates that need an exception. See [bevy Cargo.toml](https://github.com/bevyengine/bevy/blob/main/Cargo.toml).

**Reference configs from strict, widely used projects**
- **uv** (Astral): `[workspace.lints.rust] unsafe_code = "warn"`, `unreachable_pub = "warn"`; `[workspace.lints.clippy] pedantic = { level = "warn", priority = -2 }`.
  - Pedantic lints it allows: `too_many_lines`, `too_many_arguments`, `similar_names`, `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `match_same_arms`, `map_unwrap_or`, `implicit_hasher`, `struct_excessive_bools`, `used_underscore_binding`, `if_not_else`, `collapsible_if`, `collapsible_else_if`, `char_lit_as_u8`, `assert_is_empty`.
  - Restriction lints it enables: `print_stdout`, `print_stderr`, `dbg_macro`, `exit`, `get_unwrap`, `empty_drop`, `empty_structs_with_brackets`, `rc_buffer`, `rc_mutex`, `rest_pat_in_fully_bound_structs`, `use_self`.
  - `large_stack_arrays = "allow"` with the comment "Diagnostics are not actionable" ([clippy#13774](https://github.com/rust-lang/rust-clippy/issues/13774)).
  - See [uv Cargo.toml](https://github.com/astral-sh/uv/blob/main/Cargo.toml).
- uv's `clippy.toml` uses `disallowed-types` with reasons, for example `{ path = "url::Url", reason = "Use uv_redacted::SafeUrl ..." }`, and bans `std::fs::File` and `std::fs::OpenOptions` in favour of wrappers. It also uses `disallowed-methods` and `doc-valid-idents = [..., ".."]`. This is a mechanical way to force "use our wrapper", the Rust analogue of TigerBeetle's ban list. See [uv clippy.toml](https://github.com/astral-sh/uv/blob/main/clippy.toml).
- **ruff** uses the same base as uv and adds `iter_over_hash_type = "warn"` (a restriction lint relevant to determinism), the nursery lints `redundant_clone`, `debug_assert_with_mut_call` and `unused_peekable`, and `unexpected_cfgs = { level = "warn", check-cfg = ["cfg(fuzzing)", "cfg(codspeed)"] }`. See [ruff Cargo.toml](https://github.com/astral-sh/ruff/blob/main/Cargo.toml).
- **Bevy** sets:
  - `unsafe_code = "deny"`, `unsafe_op_in_unsafe_fn`, `unused_qualifications`, `missing_docs`.
  - Clippy: `undocumented_unsafe_blocks`, `allow_attributes = "warn"`, `allow_attributes_without_reason = "warn"`, `std_instead_of_alloc`, `alloc_instead_of_core` (no_std hygiene), `ptr_as_ptr`, `ref_as_ptr`, `semicolon_if_nothing_returned`, `manual_let_else`.
  - It allows `std_instead_of_core` because the lint falsely fires on `std::io`.
  - See [bevy Cargo.toml](https://github.com/bevyengine/bevy/blob/main/Cargo.toml).

**Lints that map to TigerStyle rules** (group and first clippy version, from source)
- Panics and partial functions:
  - restriction: `unwrap_used` (1.45), `expect_used` (1.45), `panic` (1.40), `todo`, `unimplemented`, `unreachable` (1.40), `unwrap_in_result` (1.48), `panic_in_result_fn` (1.48), `get_unwrap`.
  - `manual_assert` is pedantic (1.57).
  - See [clippy lint list](https://rust-lang.github.io/rust-clippy/master/index.html).
- Indexing:
  - `indexing_slicing` is restriction.
  - `missing_asserts_for_indexing` (restriction, 1.74) flags repeated indexing into a slice without a prior `assert!(v.len() > N)`. Its rationale is that one length assert lets LLVM elide every later bounds check, which is directly TigerStyle-aligned for stencil kernels. Its documented drawbacks are false positives, and it does not track asserts across function calls.
  - See [missing_asserts_for_indexing.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/missing_asserts_for_indexing.rs).
- Integer overflow: `arithmetic_side_effects` (restriction, 1.64) checks `+ - * << / %` that can overflow or panic. **It ignores floats**, `Wrapping`/`Saturating`, const contexts and allowed types. It can be tuned with `arithmetic-side-effects-allowed`, `arithmetic-side-effects-allowed-binary` (supports `"*"` wildcards) and `arithmetic-side-effects-allowed-unary` in clippy.toml. Related restriction lints: `integer_division`, `integer_division_remainder_used` (1.79), `modulo_arithmetic`. See [conf.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_config/src/conf.rs).
- Casts (TigerStyle "explicitly-sized types"):
  - pedantic: `cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`, `cast_precision_loss`, `cast_lossless`.
  - restriction: `as_conversions` (1.41) bans `as` entirely.
  - suspicious: `cast_abs_to_unsigned`.
  - `default_numeric_fallback` (restriction, 1.52) flags unconstrained literals that silently become `i32`/`f64`.
  - See [clippy lint list](https://rust-lang.github.io/rust-clippy/master/index.html).
- Floats:
  - `float_cmp` (pedantic) and `float_cmp_const` (restriction).
  - `float_equality_without_abs` (suspicious, on by default).
  - `lossy_float_literal` (restriction), `excessive_precision` (style).
  - `imprecise_flops` and `suboptimal_flops` (nursery: suggest `mul_add`, `hypot`, `ln_1p`, …), `while_float` (nursery, 1.80).
  - `manual_midpoint` (pedantic, 1.87).
  - `float_arithmetic` (restriction) bans all float math and is unusable for a PDE code.
  - The `float_cmp` docs warn: "Do not use `f64::EPSILON`" as an error margin; use domain-specific absolute or relative tolerances instead. See [operators/mod.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/operators/mod.rs).
- Function size and complexity:
  - `too_many_lines` (pedantic): `too-many-lines-threshold` defaults to **100**. The implementation **skips blank lines and comment lines and strips the enclosing braces**. Closures are not checked separately; they count toward their parent. See [too_many_lines.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/functions/too_many_lines.rs).
  - `cognitive_complexity` is now in **restriction** (1.35), with `cognitive-complexity-threshold` default 25.
  - `excessive_nesting` (complexity, 1.72) is **disabled by default**: `excessive-nesting-threshold = 0` means off.
  - `too_many_arguments` (complexity) has `too-many-arguments-threshold` default 7. `fn_params_excessive_bools` uses `max-fn-params-bools` default 3.
  - See [conf.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_config/src/conf.rs).
- Assertions:
  - `missing_assert_message` (restriction, 1.70) requires a message on every `assert!`.
  - `assertions_on_result_states` (restriction) and `assert_is_empty` (pedantic, **new in 1.99**).
  - `manual_assert_eq` (pedantic, **new in 1.97**).
  - `debug_assert_with_mut_call` (nursery).
  - See [clippy CHANGELOG](https://github.com/rust-lang/rust-clippy/blob/master/CHANGELOG.md).
- Recursion: the only lints are `unconditional_recursion` (suspicious, 1.77, plus rustc's built-in lint of the same name), `only_used_in_recursion` (complexity), `self_only_used_in_recursion` (pedantic, 1.92) and `recursive_format_impl` (correctness). **No lint forbids recursion in general.** See [unconditional_recursion.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/unconditional_recursion.rs).
- Loops: `infinite_loop` (restriction, 1.76) flags possibly unintended infinite loops, which partly supports "bounded loops". `needless_range_loop` (style) pushes towards iterators.
- Stack and allocation hygiene:
  - `large_stack_arrays` and `large_const_arrays` use `array-size-threshold` default 16 KiB. `large_stack_frames` uses `stack-size-threshold` default 512,000 bytes.
  - `std_instead_of_core`, `std_instead_of_alloc` and `alloc_instead_of_core` are restriction lints.
  - Clippy has no lint that bans heap allocation.
- Lint-suppression hygiene, which matters with agents that "fix" lints by silencing them:
  - `allow_attributes` (restriction, 1.70) forces `#[expect]`, which itself warns when the lint no longer fires.
  - `allow_attributes_without_reason` (restriction, 1.61) forces `reason = "..."`.
  - BeautifulRust says these need `rust-version >= 1.81` ([BeautifulRust](https://github.com/morganlinton/BeautifulRust)).
- Naming and shadowing: `min_ident_chars` (restriction, threshold default 1), `many_single_char_names` (pedantic), `shadow_unrelated` (restriction), `wildcard_enum_match_arm` (restriction), `precedence_bits` (restriction, 1.86; mirrors TigerBeetle's "no mixing bitops and arithmetic without parens"). The default `precedence` lint covers the arithmetic/shift case.
- Test-friendly config knobs, all defaulting to `false`: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-indexing-slicing-in-tests`, `allow-panic-in-tests`, `allow-dbg-in-tests`. `allow-unwrap-in-consts` and `allow-expect-in-consts` default to `true`. See [conf.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_config/src/conf.rs).

**Agent-oriented lint configs**
- **BeautifulRust** (Oct 2026) ships two things:
  - A drop-in `lints/lints.toml` plus `clippy.toml`. It flags `unwrap()` outside tests, `unsafe` without `// SAFETY:`, `#[allow]` without a reason, any `bool` fn parameter, silent lossy `as` casts, missing `# Errors` and `# Panics` docs, `assert!` without a message, non-`#[non_exhaustive]` exported enums, and `dbg!`/`todo!`.
  - `scripts/check-lints.sh`, which runs clippy on a fixture crate with one deliberate violation per lint. It fails if a lint stops firing, if an unexpected lint fires, or if the fixture's copy of the table drifts from `lints.toml`. This is a regression test for the lint config itself.
  - The author's own single-run benchmark, graded by a Claude model: Claude Code (Opus 5.5) scored 72/85 without the skills and 80/85 with them; Codex CLI scored 53/85 without and 76/85 with.
  - See [BeautifulRust](https://github.com/morganlinton/BeautifulRust).
- Blog-level guides (lower authority) recommend:
  - Putting the exact commands in CLAUDE.md or AGENTS.md (`cargo clippy --workspace --all-targets -- -D warnings`).
  - Pinning the toolchain via `rust-toolchain.toml` so CI is deterministic.
  - Targeted `#[allow]` with a justification comment instead of crate-level blanket allows.
  - See [The Prompt Shelf](https://thepromptshelf.dev/blog/claude-code-rust-projects-guide-2026/), [DEV: 14 CLAUDE.md rules](https://dev.to/olivia_craft/14-claudemd-rules-that-make-ai-write-truly-safe-rust-code-184m) and [HackerNoon: Coding Rust with Claude Code and Codex](https://hackernoon.com/coding-rust-with-claude-code-and-codex).

### Inferences

**Proposed starting config for nami** (a synthesis, not copied from any one project)

```toml
# Cargo.toml (workspace root)
[workspace.package]
rust-version = "1.100"   # MSRV == pinned toolchain for an application-like codebase

[workspace.lints.rust]
unsafe_code = "forbid"                 # or "deny" + #[expect(unsafe_code, reason=...)] in one audited module
unsafe_op_in_unsafe_fn = "deny"
missing_docs = "warn"
unreachable_pub = "warn"
unused_qualifications = "warn"
unused_results = "warn"                # forces explicit handling of returned values
unexpected_cfgs = { level = "warn", check-cfg = ["cfg(fuzzing)", "cfg(kani)", "cfg(codspeed)"] }

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# --- TigerStyle: crash on programmer error, never on input ---
unwrap_used = "deny"
expect_used = "warn"                   # allow in init code with #[expect(..., reason)]
panic = "warn"                         # prefer assert!/unreachable! with messages
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
print_stdout = "warn"
print_stderr = "warn"
missing_assert_message = "warn"
allow_attributes = "deny"              # force #[expect] (self-cleaning)
allow_attributes_without_reason = "deny"
# --- integers / casts / explicit sizes ---
arithmetic_side_effects = "warn"       # ints only; floats ignored
as_conversions = "warn"                # pedantic cast_* also on
default_numeric_fallback = "warn"
cast_possible_truncation = "deny"
cast_sign_loss = "deny"
cast_possible_wrap = "deny"
# --- indexing ---
indexing_slicing = "warn"              # consider "allow" in kernels; see below
missing_asserts_for_indexing = "warn"
# --- floats ---
float_cmp = "deny"
float_cmp_const = "deny"
lossy_float_literal = "deny"
suboptimal_flops = "warn"              # nursery
imprecise_flops = "warn"               # nursery
# --- size / complexity / control flow ---
too_many_lines = "deny"                # threshold in clippy.toml
cognitive_complexity = "warn"
excessive_nesting = "warn"             # needs threshold in clippy.toml (default 0 = off)
infinite_loop = "warn"
wildcard_enum_match_arm = "warn"
shadow_unrelated = "warn"
precedence_bits = "warn"
iter_over_hash_type = "deny"           # determinism
std_instead_of_core = "warn"           # if kernels are no_std
std_instead_of_alloc = "warn"
alloc_instead_of_core = "warn"
# pedantic noise for numeric code
module_name_repetitions = "allow"
similar_names = "allow"                # u_l/u_r, rho_l/rho_r etc.
many_single_char_names = "allow"       # x, y, u, v, p in math
must_use_candidate = "allow"
cast_precision_loss = "warn"           # u32 -> f64 for cell counts; keep visible, not fatal
```

```toml
# clippy.toml
too-many-lines-threshold = 60          # code lines only (no blanks/comments) ~ 70 physical lines
cognitive-complexity-threshold = 15
excessive-nesting-threshold = 4
too-many-arguments-threshold = 6
max-fn-params-bools = 0
array-size-threshold = 4096
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
arithmetic-side-effects-allowed = []   # add newtypes (e.g. CellIndex) whose ops are checked internally
disallowed-methods = [
  { path = "std::vec::Vec::push", reason = "no allocation after init; use fixed-capacity buffers" },
  { path = "std::vec::Vec::with_capacity", reason = "allocate only in init module" },
  { path = "std::collections::HashMap::new", reason = "nondeterministic iteration; use BTreeMap or index arrays" },
  { path = "std::time::Instant::now", reason = "inject a Clock for deterministic simulation" },
]
disallowed-types = [
  { path = "std::collections::HashMap", reason = "determinism" },
  { path = "std::collections::HashSet", reason = "determinism" },
  { path = "std::rc::Rc", reason = "no shared ownership graphs" },
]
disallowed-macros = [
  { path = "std::vec", reason = "allocate only in init" },
  { path = "std::format", reason = "allocates" },
]
```

**Notes on that config**
- The `disallowed-*` entries carry `reason` strings, which become the agent's error message. This is the cheapest way to encode project rules. uv uses the same pattern.
- Noisy lints for numeric/FV code:
  - `indexing_slicing` fires on every `u[i]` in stencil code. Better options: allow it in kernel modules, prefer `chunks_exact`/`windows`/fixed-size arrays, and use `missing_asserts_for_indexing`, which matches TigerStyle's "assert preconditions" better than banning indexing.
  - `as_conversions` and `cast_precision_loss` fire on every `usize`↔`f64` conversion. A small set of audited `fn to_f64(n: u32) -> f64` helpers, behind a single `#[expect]`, keeps the rest of the code clean.
  - `float_arithmetic` is unusable.
  - `similar_names` and `many_single_char_names` conflict with mathematical notation.
  - `arithmetic_side_effects` does not fire on floats, so it is cheap in a float-heavy code. It mainly hits index arithmetic, where checked or explicit arithmetic is TigerStyle-desirable anyway.
  - `float_cmp` is valuable: it pushes tests towards tolerance-based comparisons.
  - Nursery `suboptimal_flops` may change numerics (`mul_add`). Keep it as warn and decide per site, because FMA changes bitwise reproducibility.
- `too_many_lines` cannot express TigerStyle's 70 *physical* lines, because it ignores comments and blank lines and excludes the signature. Use a code-lines threshold around 55–60, or add a custom tidy check that counts physical lines (see Q2).
- `deny(warnings)` should **not** go in source (`#![deny(warnings)]`): new toolchains add lints and break builds. Set warn levels in `[lints]` and pass `-D warnings` only in CI and in the agent's check command (`RUSTFLAGS="-D warnings"` or `cargo clippy ... -- -D warnings`).
- For agents, `allow_attributes = "deny"` plus `allow_attributes_without_reason = "deny"` is high-leverage. Every suppression must be an `#[expect(lint, reason = "...")]`, which is greppable and countable by tidy, and becomes an error once it is stale.

### Gaps
- I did not verify per-lint false-positive rates on numeric codebases. The "noisy" assessments above come from lint semantics and the projects' allow-lists, not from measurements.
- I found no maintained clippy lint for general recursion, call-graph cycles or "loop must have bound".
- I did not check whether `disallowed-methods` can ban trait-method calls like `Vec::push` in every form (for example via `Extend`). Verify against the clippy docs before relying on it.
- I could not confirm the exact BeautifulRust lint levels; the README does not reproduce `lints.toml`.

---

## Q2. Enforcing TigerStyle rules clippy can't: custom tidy, dylint, grep checks, no-alloc enforcement, assertion density

### Takeaway
TigerBeetle enforces its style with a hand-written `tidy` test that runs in the normal test suite. Its features are a ban list with suggested replacements, a ratcheting function-length check, dead-code and dead-file detection, a line-length limit, a blob-size limit and commit-trailer checks. The same design ports directly to a Rust `cargo xtask tidy` built on `syn`. That is simpler and more robust than dylint, which needs a nightly toolchain pinned per library. For no-allocation-after-init, the practical options are:
- A custom `#[global_allocator]` that counts allocations or panics after an "init done" flag (no well-maintained crate exists; `assert_no_alloc` was last released in 2021).
- A `#![no_std]` kernel crate with no `alloc` dependency, so the compiler enforces it.

### Cited Findings

**TigerBeetle's TIGER_STYLE.md: the rules to enforce**
- No recursion.
- "Put a limit on everything": every loop and queue gets a fixed upper bound.
- Explicitly sized types (`u32`) instead of `usize`.
- Assert all arguments, return values, pre/postconditions and invariants, with "a minimum of two assertions per function" on average.
- Pair assertions, split compound assertions, and assert compile-time constant relationships.
- "No memory may be dynamically allocated (or freed and reallocated) after initialization."
- A "hard limit of 70 lines per function".
- A line length of at most 100 columns.
- "Zero technical debt".
- See [TIGER_STYLE.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md).

**What TigerBeetle's tidy actually enforced** (last version before it was moved out on 2026-09-23)
- It ran as `test "tidy"` via `./zig/zig build test -- tidy` ([tidy.zig @ 24d92bf](https://github.com/tigerbeetle/tigerbeetle/blob/24d92bff033453dcc757adfcd9aa14655d1b1734/src/tidy.zig)). Commit `2f45ef6` ("tidy: move — We now have a separate place for such tooling") removed `src/tidy.zig` and its HACKING.md entry ([commit 2f45ef6](https://github.com/tigerbeetle/tigerbeetle/commit/2f45ef6d49084d8f6a1840eca2934bf6795b94fd)).
- **Ban list with replacement hints.** Errors look like `file:line: error: X is banned, use Y`. Entries include `@memcpy(` → `stdx.copy_disjoint`, `std.BoundedArray` → `stdx.BoundedArrayType`, `std.time.Instant` → `stdx.Instant`, PRNG helpers → `stdx.PRNG`, `debug.assert(` → unqualified `assert`, `== error.` → `switch`, `usingnamespace`, and `Self = @This()` → a proper type name.
- **`FIXME` and `dbg(` cannot merge.** They are allowed while iterating but "leftover FIXME, remove before merge" fails tidy.
- **Function length as a ratchet.** The "red zone" is exclusive 70 < len < 73. The comment reads: "We ratchet 70-lines-per-function TigerStyle rule from the bottom up. Some functions want to be really long … The most value is in preventing originally small functions to grow over time." Existing long functions are therefore grandfathered; a function that grows past 70 is flagged.
- **AST checks.** It forbids mixing bitwise and arithmetic operators without parentheses, enforces blank-line grouping around `defer`, and requires CamelCase functions to return types.
- **Other checks:**
  - Dead private declarations and dead files, using an import-grep heuristic, because the compiler doesn't see unreferenced files.
  - 100-column lines, with exceptions for links and raw literals.
  - Control characters (tabs allowed only for Go).
  - Markdown titles.
  - Changelog line length.
  - "No large blobs" across all of git history (>4 MiB, via `git rev-list --objects | git cat-file --batch-check`; requires a non-shallow clone).
  - Unix permissions.
- **"tidy commits" rejects LLM contributions.** It scans the last 32 commits' author and trailers (`Co-authored-by`, `Generated-by`, `Made-with`, `AI-Author`, `Signed-off-by`, …) for `aider`, `claude`, `codex`, `copilot`, `cursor` and `gemini`, and prints "TigerBeetle does not allow LLM contributions." This is notable context: TigerStyle's originators explicitly reject agent-written code. nami would be applying the style in the opposite setting.

**dylint (custom rustc lints as dynamic libraries)**
- `cargo dylint new <name>` scaffolds a `LateLintPass`.
- Libraries are declared in `[workspace.metadata.dylint] libraries = [{ git = "https://github.com/trailofbits/dylint", pattern = "examples/general" }, { path = "lints/no_recursion" }]` and run with `cargo dylint --all`.
- Libraries are built per toolchain (`target/dylint/libraries/<toolchain>/release`). CI should cache `~/.dylint_drivers/` and `~/.rustup/toolchains/`.
- Writing lints uses `rustc_private`, which means nightly internals. Each lint library pins its own nightly in its own `rust-toolchain`; that convention is well known but not stated on the README page I read.
- See [trailofbits/dylint](https://github.com/trailofbits/dylint). Current version: cargo-dylint 6.4.0, released 2026-11-10 ([crates.io](https://crates.io/crates/cargo-dylint)).

**No-alloc enforcement options**
- **`assert_no_alloc`** (Windfisch): register `AllocDisabler` as `#[global_allocator]` and wrap code in `assert_no_alloc(|| ...)`. `permit_alloc(|| ...)` re-enables allocation and `PermitDrop` covers deallocating drops. The default is to abort; `warn_debug`/`warn_release` print instead. `disable_release` (on by default) makes it a no-op in release builds. It depends on std: a thread-local flag over `std::alloc::System`. Last release 1.1.2 in 2021; the Marcono1234 fork is more recent but undocumented as to its differences. See [Windfisch/rust-assert-no-alloc](https://github.com/Windfisch/rust-assert-no-alloc), [Marcono1234 fork](https://github.com/Marcono1234/rust-assert-no-alloc) and [docs.rs](https://docs.rs/crate/assert_no_alloc/1.1.2).
- `qadapt` is deprecated in favour of `alloc-counter`. `alloc-fmt` (nightly, 2020) provides allocation-safe assertion macros for use inside an allocator. See [qadapt](https://github.com/bspeice/qadapt) and [alloc-fmt on lib.rs](https://lib.rs/crates/alloc-fmt).
- Counting allocators for test assertions: `stats_alloc` 0.1.10 (2022) and `allocation-counter` 0.8.1 (2023), both old but simple ([crates.io](https://crates.io/crates/stats_alloc)).
- **TigerBeetle's own approach** is a `static_allocator.zig` and a `counting_allocator.zig` in `src/`, visible in the [repo tree](https://github.com/tigerbeetle/tigerbeetle/tree/main/src). The static allocator transitions from an "init" to a "static" state. That reading of `static_allocator.zig` comes from my prior knowledge; I did not read the file in this session.
- **`no-panic`** 0.1.37 (2026-07) makes a function fail to link if the compiler cannot prove it never panics. It is a mechanical "panic-free kernel" check, but brittle across optimisation levels ([crates.io](https://crates.io/crates/no-panic)).

### Inferences
- **Recommended tidy for nami** is `cargo xtask tidy`, run inside `cargo test` as a `#[test]` so agents can't skip it. It would use `syn` plus `proc-macro2` spans. Checks:
  1. **Function length:** physical lines from `fn` to the closing `}`, at most 70, including methods and closures. Use an explicit allow-list file if needed rather than TigerBeetle's red-zone ratchet, since nami starts from zero.
  2. **Assertion density:** count `assert!`, `assert_eq!`, `assert_ne!`, `debug_assert*!` and project macros (for example `nami::assert_pre!`) per fn. Fail if the crate average is below 2.0, or if any non-trivial fn over N lines has 0. Exempt trivial getters by size.
  3. **No recursion:** build a name-based call graph per crate with `syn` and reject cycles, or use a dylint `LateLintPass` on HIR/MIR for exact resolution. Name-based is a heuristic: trait dispatch and closures are blind spots.
  4. **Bounded loops:** ban `loop {` and `while` without a `// bound:` comment or an accompanying assert of an iteration counter. Alternatively require `for _ in 0..MAX_ITERS` style.
  5. **Integer types:** ban `usize` in public signatures except at an indexing boundary. clippy can't express this; `disallowed-types` can't ban primitives.
  6. **Ban list with replacements**, as in TigerBeetle: `HashMap`, `Instant::now`, `thread_rng`, `.clone()` in kernels, `Box::new`/`vec!` outside init modules, `FIXME`/`TODO`/`XXX`/`dbg!`, `#[allow(`, `unsafe`, `#[ignore]` without a reason, and `std::process::exit`.
  7. **Debt markers:** zero `TODO` in merged code ("zero technical debt"), with no exceptions.
  8. **Line length ≤ 100:** use `rustfmt.toml` `max_width = 100` plus a tidy check for comments and strings, which rustfmt doesn't wrap.
  9. **Dead files**, large blobs, and executable bits.
  10. **Each `#[expect]` must carry a reason**, and the total count is ratcheted: it may only go down.
- **No-alloc design** for a solver with "allocate at init, then step" structure:
  - Put kernels in a `#![no_std]` crate **without** `extern crate alloc`. The compiler then rejects `Vec`, `Box` and `String` in kernels at zero runtime cost, which is the strongest guarantee.
  - The orchestration crate allocates in `Solver::init()`.
  - In debug and test builds, install a counting `#[global_allocator]` that `panic!`s or aborts when a global `INIT_DONE: AtomicBool` is set. Assert zero allocations across N time steps in an integration test. About 40 lines of custom code; an external crate is not needed.
  - Use `abort` rather than `panic`, because panic formatting can allocate (the motivation for alloc-fmt).
- Grep checks, like ripgrep in a `just tidy` recipe, are fine for bans but produce false positives in strings and comments. AST-based checks via `syn` give precise spans, which agents can act on directly.

### Gaps
- I did not find where TigerBeetle's tidy moved after 2026-09-23 ("a separate place for such tooling").
- TigerBeetle has a Rust client (`src/clients/rust`) and a Rust Vortex driver (`src/testing/vortex/rust_driver`). I did not inspect their lint or tidy configuration, which would be the most direct "TigerStyle in Rust" evidence.
- I found no maintained Rust crate for assertion-density measurement or recursion bans; both would be custom.
- I did not verify whether any existing dylint example library already detects recursion.

---

## Q3. Testing tools: nextest, proptest/quickcheck, fuzzing, insta, cargo-mutants, miri, sanitizers, Kani/Prusti/Creusot, loom, deterministic simulation testing

### Takeaway
A gate suitable for agents runs four layers:
- **cargo-nextest** for the fast runner with terse output and per-test processes.
- **proptest** (or **bolero**, which unifies property tests with fuzzing and Kani) for invariants such as conservation, positivity and symmetry.
- **cargo-mutants `--in-diff`** on every PR, so AI-written tests must actually kill mutants in the changed code.
- A seed-driven deterministic simulation harness in TigerBeetle VOPR style, run nightly with many seeds.

Kani is the mature option for bounded proofs of small kernels: index math, flux functions on bounded inputs, absence of overflow. Miri and loom matter little if nami has no unsafe code and no concurrency.

### Cited Findings
- **cargo-mutants** 27.1.0 (2026-06):
  - `--in-diff <file>` restricts mutants to regions changed in a diff, layered on other filters.
  - The documented PR workflow fetches full history, runs `git diff origin/<base>.. > git.diff`, then `cargo mutants --no-shuffle -vV --in-diff git.diff`, and uploads `mutants.out`.
  - The docs suggest `--in-place` in CI to avoid copying the tree, and frame two uses: changed-code mutants on PRs and full runs on main.
  - Since 26.2.0, a diff with no Rust changes gives a clearer error.
  - See [mutants.rs: in-diff](https://mutants.rs/in-diff.html), [mutants.rs: PR diff](https://mutants.rs/pr-diff.html), [mutants.rs: CI](https://mutants.rs/ci.html) and the [NEWS](https://docs.rs/crate/cargo-mutants/latest/source/NEWS.md).
- **Deterministic simulation testing (DST) in Rust**, from S2's write-up of 2025-04-02 (inspired by FoundationDB and TigerBeetle):
  - Everything flows from one RNG seed.
  - Tokio runs single-threaded with a paused clock, and its RNG is seeded. Turmoil simulates the network and hosts.
  - The `mad-turmoil` crate overrides `getrandom`/`getentropy`/`CCRandomGenerateBytes` and `clock_gettime`.
  - Randomized `HashMap` hashing was a flagged source of nondeterminism.
  - A CI "meta test" **reruns the same seed and compares TRACE logs byte for byte** to prove determinism.
  - DST runs on every PR plus "thousands of nightly trials"; it found 17 notable issues.
  - See [S2: DST for async Rust](https://s2.dev/blog/dst).
- turmoil 0.7.2 calls itself "very experimental" ([turmoil README](https://docs.rs/crate/turmoil/0.7.2/source/README.md)). madsim 0.2.34 is a drop-in tokio replacement enabled with `RUSTFLAGS="--cfg madsim"` ([madsim](https://github.com/madsim-rs/madsim)). There is a FOSDEM 2026 talk on Rust DST, "Random Seeds and State Machines" ([FOSDEM 2026](https://fosdem.org/2026/schedule/event/rust-deterministic-simulation-testing)).
- In TigerStyle, assertions are "a safety net"; VOPR, TigerBeetle's simulator, is "the final line of defense" against bugs in your understanding ([TIGER_STYLE.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md)). TigerBeetle's tree has `src/vopr.zig`, `src/testing/cluster/*_checker.zig` and many `*_fuzz.zig` files, plus `fuzz_tests.zig` ([repo tree](https://github.com/tigerbeetle/tigerbeetle/tree/main/src)). A deterministic seeded PRNG is mandatory: TigerBeetle's tidy bans std PRNG range helpers in favour of `stdx.PRNG` ([tidy.zig](https://github.com/tigerbeetle/tigerbeetle/blob/24d92bff033453dcc757adfcd9aa14655d1b1734/src/tidy.zig)).
- **Current versions:**
  - Test runner and properties: cargo-nextest 0.9.152 (2026-11-13), proptest 1.12.0 (2026-10), quickcheck 1.1.0.
  - Fuzzing: bolero 0.13.7, cargo-fuzz 0.13.2, arbitrary 1.5.0.
  - Snapshots, verification, concurrency: insta 1.49.0, kani-verifier 0.68.0 (2026-09), loom 0.7.3, shuttle 0.9.10.
  - Coverage and UB checks: cargo-llvm-cov 0.9.2, cargo-careful 0.4.10.
  - Source: [crates.io](https://crates.io/crates/cargo-nextest) (API queried 2026-11-24).
- Tool roles, from primary docs:
  - nextest runs each test in its own process, with retries, timeouts and slow-test detection, JUnit output, partitioning (`--partition`) and profiles in `.config/nextest.toml`. It does **not** run doctests, so run `cargo test --doc` separately ([nexte.st](https://nexte.st/)).
  - insta does snapshot testing through `cargo insta review` and `INSTA_UPDATE=no` / `--check` in CI ([insta.rs](https://insta.rs/)).
  - bolero runs one harness under libFuzzer, AFL, honggfuzz, Kani or plain `cargo test` ([bolero](https://github.com/camshaft/bolero)).
  - Kani is a bit-precise bounded model checker that runs `#[kani::proof]` harnesses with `cargo kani` ([Kani](https://model-checking.github.io/kani/)).
  - Miri interprets MIR to detect UB in unsafe code ([miri](https://github.com/rust-lang/miri)).
  - cargo-careful runs with a debug-assertion-enabled std ([cargo-careful](https://github.com/RalfJung/cargo-careful)).

### Inferences
- Gates proposed for nami, by cost:
  - **Every commit (fast):**
    - `cargo nextest run --workspace` with debug assertions **and** `overflow-checks = true` in a test profile that is otherwise optimised.
    - `cargo test --doc`.
    - proptest with a pinned case count, using `PROPTEST_CASES` or the config, and with failure persistence committed (`proptest-regressions/`).
  - **Every PR:**
    - `cargo mutants --in-diff` with zero missed mutants allowed in changed code. Missed mutants are a strong signal that agent-written tests are asserting nothing, so treat them as errors.
    - Kani proofs for small kernels, such as Riemann-solver positivity on bounded inputs and index math without overflow.
    - A short DST run with a fixed set of seeds.
  - **Nightly:**
    - A full `cargo mutants` run.
    - Long fuzzing (bolero/cargo-fuzz) against flux, reconstruction and limiter functions, with NaN/Inf/denormal/extreme-state inputs.
    - Thousands of simulation seeds.
    - Miri, only if unsafe exists.
- **VOPR-style DST for a PDE code** means a seeded generator of initial and boundary conditions, mesh sizes, CFL numbers and fault injection (NaN injection, extreme states, truncated restarts). Each run steps the solver under heavy asserts and checks invariants: conservation to round-off, positivity, entropy, symmetry preservation and bitwise determinism across reruns. Failures print the seed, giving a one-line repro command, which is an ideal agent feedback format.
- Snapshot tests (insta) for numeric fields should snapshot a *rounded or summarised* representation (norms, conserved totals), or use tolerance-aware comparisons, to avoid churn. Agents tend to blindly run `cargo insta accept`, so CI must run with `--check` (or `INSTA_UPDATE=no`) and tidy can forbid `.snap.new` files.
- Concurrency tools (loom, shuttle) are needed only if nami adds threads. TigerStyle's single-threaded, run-to-completion stance suggests it won't initially.

### Gaps
- I did not fetch current docs for Prusti or Creusot. Their 2026 maturity and toolchain compatibility are unverified, and Prusti has historically lagged nightly.
- I did not verify Kani's current float support; bounded model checking of float-heavy flux code may be slow or limited.
- I found no published Rust "VOPR for numerical solvers" example.
- I did not confirm sanitizer usage specifics (`-Zsanitizer=address` requires nightly) from a primary source this session.

---

## Q4. Supply chain and hygiene: cargo-deny, cargo-audit, unused deps, semver-checks, MSRV, toolchain pinning, feature checks

### Takeaway
Use the standard 2026 stack:
- `rust-toolchain.toml` pinning an exact stable (for example `1.100.0`) with `components = ["rustfmt", "clippy"]`.
- `cargo deny check` for advisories, licenses, bans, duplicates and sources. It subsumes most of `cargo audit`.
- `cargo machete` or `cargo shear` for fast unused-dependency detection; `cargo-udeps` is slower and nightly.
- `cargo hack --each-feature` (or `--feature-powerset`) for feature combinations.
- `cargo semver-checks` only once nami publishes a library API.

All are actively maintained as of Nov 2026.

### Cited Findings
- Versions and release dates are from the [crates.io API](https://crates.io/crates/cargo-deny), queried 2026-11-24:

  | Tool | Version | Released |
  |---|---|---|
  | cargo-deny | 0.22.1 | 2026-11-24 |
  | cargo-audit | 0.23.0 | 2026-11-10 |
  | cargo-vet | 0.10.3 | 2026-10-22 |
  | cargo-auditable | 0.7.7 | — |
  | cargo-machete | 0.10.1 | 2026-11-16 |
  | cargo-shear | 1.14.1 | 2026-11-13 |
  | cargo-udeps | 0.1.61 | 2026-04 |
  | cargo-semver-checks | 0.51.0 | 2026-10 |
  | cargo-hack | 0.6.46 | 2026-11-01 |
  | cargo-msrv | 0.19.6 | 2026-11-24 |
- The rust-clippy repo pins its own toolchain with a `rust-toolchain.toml` at the root ([rust-clippy repo](https://github.com/rust-lang/rust-clippy)). Guides for agent workflows recommend `rust-toolchain.toml` pinning for deterministic CI ([The Prompt Shelf](https://thepromptshelf.dev/blog/claude-code-rust-projects-guide-2026/)).
- Cargo's own lints table supports `[lints.cargo]` (for example `unused_dependencies` in the reference example) ([Cargo manifest reference](https://doc.rust-lang.org/cargo/reference/manifest.html)). Ruff sets `[workspace.lints.cargo] redundant_homepage = "allow"` ([ruff Cargo.toml](https://github.com/astral-sh/ruff/blob/main/Cargo.toml)).
- Tool documentation:
  - cargo-deny has `advisories`, `licenses`, `bans` (deny or skip specific crates and duplicate versions) and `sources` (allowed registries and git) sections in `deny.toml` ([cargo-deny book](https://embarkstudios.github.io/cargo-deny/)).
  - cargo-hack offers `--each-feature`, `--feature-powerset` and `--rust-version` ([cargo-hack](https://github.com/taiki-e/cargo-hack)).
  - cargo-semver-checks lints the public API against the last published version ([cargo-semver-checks](https://github.com/obi1kenobi/cargo-semver-checks)).

### Inferences
- For a no-allocation numeric framework, dependency minimalism is itself a TigerStyle value. Use `cargo deny`'s `[bans]` with an explicit allow-list (`allow = [...]`) so an agent cannot add a crate without a human-reviewed `deny.toml` change. Pair that with CODEOWNERS on `Cargo.toml`, `Cargo.lock`, `deny.toml`, `clippy.toml`, `rust-toolchain.toml` and `.github/`, so agents can't relax the guardrails themselves.
- `deny.toml` sketch: `[advisories] yanked = "deny"`; `[bans] multiple-versions = "deny"`, `wildcards = "deny"`, `allow = ["proptest", ...]` (dev-dependencies can be scoped); `[sources] unknown-registry = "deny"`, `unknown-git = "deny"`; `[licenses] allow = ["MIT", "Apache-2.0", ...]`.
- MSRV: for an application-like research framework, set `rust-version` equal to the pinned toolchain and bump both together, for example with a monthly Renovate or Dependabot PR. `cargo hack --rust-version` or `cargo msrv verify` is needed only if a lower MSRV is promised.
- Run `cargo update --locked` or `cargo build --locked` in CI so agents can't silently change `Cargo.lock`.

### Gaps
- I did not fetch the cargo-deny docs page this session; the config keys above come from my prior knowledge of the long-stable `deny.toml` schema. Check them against the 0.22 docs.
- I did not compare cargo-machete and cargo-shear accuracy.

---

## Q5. Performance gates: criterion / divan / iai-callgrind (gungraun), CodSpeed, Bencher

### Takeaway
For CI regression gating on shared runners, use **instruction-count** benchmarks rather than wall-clock: **gungraun** (the renamed iai-callgrind, on Valgrind) or **CodSpeed**'s simulated-CPU instrumentation. Keep criterion or divan for local wall-clock work. Bencher (bencher.dev) offers self-hostable statistical thresholds and fails a PR with `--error-on-alert`.

### Cited Findings
- **iai-callgrind is now published as Gungraun**, with the runner renamed to `gungraun-runner`. iai-callgrind's last release was 0.16.1 (2025-07); gungraun is at 0.20.0 (2026-09-26). See [crates.io](https://crates.io/crates/gungraun), [docs.rs gungraun](https://docs.rs/crate/gungraun/), [Gungraun book](https://gungraun.github.io/gungraun/latest/html/index.html) and [lib.rs gungraun-runner](https://lib.rs/crates/gungraun-runner). rust-lang/compiler-builtins migrated benchmarks to gungraun ([PR #1033](https://github.com/rust-lang/compiler-builtins/pull/1033/files)).
- **CodSpeed** provides compatibility layers for divan (recommended), criterion.rs and libtest bench. The crates codspeed-criterion-compat and codspeed-divan-compat are at 6.1.0 (2026-11-20). See [CodSpeed Rust docs](https://codspeed.io/docs/benchmarks/rust), [codspeed-rust](https://github.com/codspeedhq/codspeed-rust) and [crates.io](https://crates.io/crates/codspeed-divan-compat). Ruff declares `cfg(codspeed)` in its check-cfg, which indicates it uses CodSpeed ([ruff Cargo.toml](https://github.com/astral-sh/ruff/blob/main/Cargo.toml)).
- divan 0.1.21's last release was 2025-04-10, so it is effectively in maintenance. criterion 0.8.2 was released 2026-02 ([crates.io](https://crates.io/crates/divan)).
- **Bencher thresholds:** each Branch × Testbed × Measure combination gets one statistical Test: `percentage`, `z_score` (best with at least 30 samples), `t_test`, `log_normal`, `iqr`, `delta_iqr` or `static`. Sample windows are set with `--threshold-min-sample-size`, `--threshold-max-sample-size` and `--threshold-window`. `--error-on-alert` fails the CI command. Its Rust adapters cover libtest bench, Criterion, Iai and Gungraun; divan is not listed. See [Bencher thresholds](https://bencher.dev/docs/explanation/thresholds/).

### Inferences
- For nami, benchmark a few core kernels with gungraun: flux evaluation per cell, reconstruction sweep, and one full time step on a fixed mesh. Instruction counts are deterministic enough to gate at a threshold of a few percent on PRs without dedicated hardware. Wall-clock (criterion or divan) belongs on a bare-metal nightly job.
- Make the perf gate *advisory with a required human ack*, rather than a hard fail, at first. Agents will otherwise "fix" regressions with micro-hacks that may hurt readability or violate TigerStyle.

### Gaps
- I found no independent 2026 comparison of CodSpeed and Bencher.
- I did not confirm CodSpeed pricing or open-source terms for 2026.
- The divan maintenance status is inferred from release dates only.

---

## Q6. CI structure for agents: fast local pre-commit, single check command, terse output, merge queue, branch protection

### Takeaway
Agents work best with **one command** that runs every gate in fail-fast order, with machine-readable or terse output: `just check` or `cargo xtask check`, mirrored exactly in CI. TigerBeetle's model puts tidy inside the test suite, so it cannot be skipped. Server-side enforcement (required status checks, merge queue, CODEOWNERS on guardrail files) matters more than client hooks, because agents can bypass local hooks (`--no-verify`).

### Cited Findings
- TigerBeetle ran its style lints as part of `zig build test` (`./zig/zig build test -- tidy`), so tidy was just another test ([commit 2f45ef6 diff of HACKING.md](https://github.com/tigerbeetle/tigerbeetle/commit/2f45ef6d49084d8f6a1840eca2934bf6795b94fd)).
- TigerBeetle uses a `src/scripts/ci.zig` CI driver and per-client `ci.zig` scripts, including `src/clients/rust/ci.zig`. CI logic is written in the project language, not YAML ([repo tree](https://github.com/tigerbeetle/tigerbeetle/tree/main/src/scripts)). This is analogous to the `cargo xtask` pattern ([matklad/cargo-xtask](https://github.com/matklad/cargo-xtask)).
- Agent-workflow guides recommend:
  - Listing the exact build, test, lint and format commands in CLAUDE.md or AGENTS.md.
  - Making zero clippy warnings a commit requirement.
  - Using AGENTS.md for multi-agent teams.
  - See [The Prompt Shelf](https://thepromptshelf.dev/blog/claude-code-rust-projects-guide-2026/) and [agentvera: Claude Code for Rust](https://agentvera.dev/en/blog/claude-code-for-rust/).
- BeautifulRust's `check-lints.sh` shows that the lint config itself is tested, so a silently disabled lint is caught ([BeautifulRust](https://github.com/morganlinton/BeautifulRust)).
- S2 runs DST on every PR and commit plus nightly trials, and uses a determinism meta-test in CI ([S2](https://s2.dev/blog/dst)).

### Inferences

**Proposed `justfile`** (all recipes are also callable from CI)

```make
# fast inner loop for agents (< ~60 s target)
check: fmt-check tidy clippy test
fmt-check:
    cargo fmt --all -- --check
tidy:
    cargo xtask tidy            # also runs as #[test] in xtask crate
clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
test:
    cargo nextest run --workspace --locked --status-level fail --final-status-level fail
    cargo test --doc --workspace --locked
# PR-level
ci: check deny hack mutants-diff kani dst-short
deny:
    cargo deny check
hack:
    cargo hack check --workspace --each-feature --no-dev-deps
mutants-diff:
    git diff origin/main.. > /tmp/pr.diff && cargo mutants --in-diff /tmp/pr.diff --no-shuffle
```

- **Terse output for agents:**
  - nextest `--status-level fail` and `--final-status-level fail`, or `--message-format libtest-json`.
  - `cargo clippy --message-format short`, or `--message-format=json` for tooling.
  - Tidy errors in `file:line: error: X is banned, use Y` form, as TigerBeetle does, so the fix is in the message.
  - Fail-fast ordering: fmt, then tidy, then clippy, then tests.
- **Pre-commit:** lefthook or pre-commit running `cargo fmt` and `just tidy` only. Keep it under 5 s; heavier gates belong in CI. Treat local hooks as a convenience, not a guarantee.
- **Server side:**
  - A GitHub ruleset or branch protection on `main` with required checks, a merge queue (so swarm PRs are tested against the actual merge result, avoiding semantic conflicts between parallel agents), and linear history.
  - CODEOWNERS requiring human review for guardrail files: `clippy.toml`, `[workspace.lints]`, `deny.toml`, `rust-toolchain.toml`, `xtask/src/tidy.rs`, `.github/workflows/`, `justfile`.
  - Tidy can also verify that the `[workspace.lints]` table has not been weakened compared with a committed snapshot.
- **Anti-bypass checks**, to be added to tidy:
  - No `#[allow(` (only `#[expect(..., reason)]`).
  - No `#[ignore]` without a reason.
  - No `#![allow(...)]` at crate roots.
  - A ratcheted count of `#[expect]`.
  - No new `unsafe`.
  - No `proptest` case-count reductions.
  - No `.snap.new` files.

### Gaps
- I did not fetch the GitHub merge-queue or rulesets docs, or the lefthook and just docs, this session. The recommendations rely on standard, widely known features.
- I found no primary-source write-up of a large Rust project's CI explicitly designed for **agent swarms** with measured outcomes. The agent-specific material is mostly blogs and the BeautifulRust repo.

---

## Q7. Write-ups of Rust projects specifically configured for AI agents

### Takeaway
Primary-source, rigorous examples are scarce. The best concrete artifact is **BeautifulRust**: agent skills plus a tested clippy config, with a small self-reported benchmark. The rest are blog-level CLAUDE.md guides. Notably, **TigerBeetle, the origin of TigerStyle, mechanically rejects LLM-attributed commits in its tidy check.**

### Cited Findings
- BeautifulRust provides:
  - Agent Skills for writing, reviewing and porting idiomatic Rust.
  - A tested clippy config, verified by `scripts/check-lints.sh`.
  - Install paths for Claude Code (`.claude/skills/`, plugin `beautiful-rust`), Codex (`.agents/skills/`), Copilot (`gh skill install`, gh ≥ 2.90), Cursor and Gemini CLI.
  - An Oct 2026 single-run benchmark, graded by a Claude model: Claude Code 72→80/85 and Codex 53→76/85 with the skills. It caveats that Claude did worse on a Bevy task with the skills.
  - See [BeautifulRust](https://github.com/morganlinton/BeautifulRust).
- TigerBeetle's tidy check is described in Q2: "tidy commits" fails CI if any of the last 32 commits has an author or AI trailer matching `aider`, `claude`, `codex`, `copilot`, `cursor` or `gemini`, printing "TigerBeetle does not allow LLM contributions." See [tidy.zig @ 24d92bf](https://github.com/tigerbeetle/tigerbeetle/blob/24d92bff033453dcc757adfcd9aa14655d1b1734/src/tidy.zig).
- Guide-level sources:
  - [The Prompt Shelf: Claude Code for Rust Projects (2026)](https://thepromptshelf.dev/blog/claude-code-rust-projects-guide-2026/).
  - [DEV: 14 CLAUDE.md rules for safe Rust](https://dev.to/olivia_craft/14-claudemd-rules-that-make-ai-write-truly-safe-rust-code-184m), which requires a `// SAFETY:` comment for each unsafe block and specifies error crates.
  - [HackerNoon: Coding Rust with Claude Code and Codex](https://hackernoon.com/coding-rust-with-claude-code-and-codex).
  - [agentvera](https://agentvera.dev/en/blog/claude-code-for-rust/).
  - [michaelalber/ai-toolkit clippy-configuration.md](https://github.com/michaelalber/ai-toolkit/blob/main/skills/rust-architecture-checklist/references/clippy-configuration.md), which is itself marked AI-generated.

  These are opinion pieces with anecdotal evidence; treat their claims as low authority.

### Inferences
- The consistent theme across sources:
  1. Encode rules as tool errors, not prose. Clippy `disallowed-*` entries with `reason`, and tidy messages with a replacement, beat a CLAUDE.md paragraph.
  2. Test the guardrails themselves, as with BeautifulRust's lint fixture crate.
  3. Make suppression costly and visible: `#[expect]` with a reason, ratcheted counts and CODEOWNERS.
  4. Have one deterministic command whose output names the fix.
- Because TigerBeetle explicitly rejects LLM-generated code, nami cannot cite TigerBeetle as precedent for agent-written TigerStyle. Its tooling ideas (tidy, VOPR, assertion density) still transfer, and nami would be a novel test of whether mechanical TigerStyle gates make agent output trustworthy.

### Gaps
- I found no peer-reviewed or large-scale empirical study, as of Nov 2026, measuring which Rust lints or gates most improve agent-written code quality.
- I found no published example of a Rust scientific-computing or PDE project configured for agent swarms.
