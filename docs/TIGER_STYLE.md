# TigerStyle for nami

> Status: **draft for maintainer review.** This file is code-owned (ADR-0008). Agents may propose
> changes through a PR; they do not edit it directly.

This is nami's coding style: [TigerBeetle's TigerStyle](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md)
(Apache License 2.0, retrieved 2026-11-25), translated from Zig to Rust and extended for numerical
code and for a codebase written mostly by AI agents. Where a rule comes from TigerStyle, it keeps
TigerStyle's reasoning. Where nami deviates or adds a rule, it says so and says why.

This file is the long-form reference. `AGENTS.md` carries only the one-line rules. The decisions
behind them are in `docs/design/` (ADRs).

**How rules are enforced.** Each rule ends with tags naming the mechanisms that enforce it.
Tidy checks every tag (rule `doc-tag`): a tag must name a rule or a lint that exists and is
switched on, so a tag can claim only what the tooling does today. `docs/RULES.md` lists what is
behind the tags.

| Tag | Mechanism |
|---|---|
| `[compiler]` | rustc itself: types, `no_std`, and its default lints, which `-D warnings` turns into errors |
| `[clippy:<lint>]` | a clippy lint set to `warn` or `deny` in the workspace lint table, with `clippy.toml` (ADR-0006). `[clippy:pedantic]` stands for a lint of the pedantic group |
| `[rustc:<lint>]` | a rustc lint set to `warn` or `deny` in the workspace lint table |
| `[tidy:<rule-id>]` | a rule of `cargo xtask tidy` (ADR-0006) |
| `[dylint:<lint>]` | a lint of the project dylint library (ADR-0006) |
| `[fmt]` | `cargo fmt --check` |
| `[deny]` | `cargo-deny` (ADR-0001) |
| `[planned:<id>]` | a mechanism that is promised but not built yet; each has a task in `docs/exec-plans/` |
| `[review]` | judgement: AI reviewer and human review (ADR-0008) |

If a rule has no mechanism, it is marked `[review]`. A rule that keeps getting broken in review is
a candidate for a mechanism.

## Why have style?

Another word for style is design. Our design goals are **safety, performance and developer
experience, in that order.** All three matter. Good style advances them, and readability is a means
to those ends, not an end in itself.

For nami, safety has a specific meaning. A crash is a liveness bug. **A plausible wrong answer is a
correctness bug, and it is the one that matters most.** A solver that runs, produces smooth-looking
fields, and quietly drops a source term or applies a boundary condition on the wrong side is worse
than one that crashes. The literature on LLM-written simulation code names exactly this as the
dominant failure (ADR-0007). Most of the rules below exist to turn silent wrong answers into loud
failures.

## Simplicity and elegance

Simplicity is not the first attempt but the hardest revision. It takes thought, several passes, and
sometimes throwing one away. An hour of design is worth weeks in production, and for nami a day of
thinking about a discretisation is worth weeks of chasing why a convergence test reads 1.7 instead
of 2.

In numerical code, simplicity usually means:

- **One equation, one function.** The code mirrors the discretisation as written in the plan or the
  paper, and cites it (`// Toro (2009), eq. 10.67`).
- **Few abstractions, each earning its place.** A trait for an equation system (`EquationSystem`
  with a const `NVAR`) makes the domain clearer. A tower of generic traits for "flexibility" does
  not. Abstractions are never zero-cost: every one is a place for a leak. `[review]`

## Zero technical debt

What could go wrong is a better question than what went wrong, because code is cheaper to change
while it is hot. nami has a **zero technical debt** policy: do it right the first time, because the
second time may never come.

- No `TODO`, `FIXME`, `XXX` or `dbg!` on `main`. Unfinished work lives in a task
  (`docs/exec-plans/`), not in a comment. `[tidy:debt-marker]` `[clippy:dbg_macro]`
- No quadratic algorithms where linear ones exist, no hidden copies in hot loops, no "temporary"
  workarounds. `[review]`

**For agents in particular.** A rule that blocks you is information, not an obstacle. Do not route
around it:

- **Never silence a lint.** `#[allow]` is banned. `#[expect(lint, reason = "...")]` needs a real
  reason and counts against a budget that only humans can raise. `[clippy:allow_attributes]`
  `[clippy:allow_attributes_without_reason]` `[tidy:expect-budget]`
- **Never loosen a tolerance, threshold or reference value** to make a test pass. They are
  human-owned (ADR-0007), and edits to `crates/reference/` wait for the code owner (ADR-0015).
  `[review]`
- **Never clamp, floor or "sanitise" a value to make a test pass** (see *Physical admissibility*
  below).
- If a rule is wrong for your case, stop and propose an ADR. That is the only sanctioned way to
  change a rule.

## Safety

> "The rules act like the seat-belt in your car: initially they are perhaps a little uncomfortable,
> but after a while their use becomes second-nature and not using them becomes unimaginable."
> — Gerard J. Holzmann

TigerStyle starts from NASA's [Power of Ten](https://spinroot.com/gerard/pdf/P10.pdf). In Rust:

### Control flow

- **Use only simple, explicit control flow.** `[review]`
- **Do not use recursion,** direct or mutual. This guarantees that everything that should be
  bounded is bounded. `[dylint:nami_recursion]` `[planned:recursion-cross-crate]`
- **No `dyn Trait` and no function pointers in `kernels`.** Static dispatch keeps the call
  graph fully resolvable for the recursion check and keeps inner loops free of indirect calls.
  `[tidy:kernel-indirection]`
- **No `async`.** Functions run to completion, so precondition assertions hold for the whole
  function. `[tidy:async]`

### Put a limit on everything

Every loop, every iteration and every buffer has a fixed upper bound. Violations then fail fast
instead of hanging.

- **Prefer `for` over a bounded range or a slice iterator.** `loop` and `while` are banned, and so
  is `for` over a range without an end. A `for _ in 0..LIMIT` with an assertion after the loop is
  almost always clearer anyway. `[tidy:unbounded-loop]`
- **Iterative methods name their limit and assert convergence.** This applies to Newton for the
  exact Riemann solver, fixed-point iterations and implicit sources. Running out of iterations is a
  crash, not a silent "best effort":

  ```rust
  /// Upper bound on Newton iterations for the star-region pressure.
  const PRESSURE_ITERATIONS_MAX: u32 = 32;

  /// Relative change in pressure below which the Newton iteration has converged.
  const PRESSURE_TOLERANCE_RELATIVE: f64 = 1.0e-12;

  /// Solves `f(p) = 0` for the star-region pressure by Newton iteration.
  pub fn pressure_star(pressure_guess: f64, residual: impl Fn(f64) -> (f64, f64)) -> f64 {
      assert!(pressure_guess > 0.0, "initial pressure guess must be positive");

      let mut pressure = pressure_guess;
      for _ in 0..PRESSURE_ITERATIONS_MAX {
          let (value, derivative) = residual(pressure);
          assert!(derivative > 0.0, "pressure function is monotone increasing");
          let pressure_next = pressure - value / derivative;
          let change_relative = (pressure_next - pressure).abs() / pressure;
          pressure = pressure_next;
          if change_relative < PRESSURE_TOLERANCE_RELATIVE {
              assert!(pressure > 0.0, "converged pressure must be positive");
              return pressure;
          }
      }
      panic!("pressure iteration did not converge in {PRESSURE_ITERATIONS_MAX} iterations");
  }
  ```

- **The time loop is bounded too.** It runs `for step in 0..steps_max` and stops when the end time
  is reached. Hitting `steps_max` without reaching the end time is reported as an error, never
  ignored. `[review]`

### Integer types

- **Use explicitly sized types.** Indices and counts are `u32` newtypes (`CellIndex`, `FaceIndex`,
  `CellCount`). `usize` appears only inside `base::index` (ADR-0002). `[tidy:usize]`
  `[planned:usize-alias]`
- **No `as` casts.** Conversions go through named functions (`CellIndex::to_usize()`) or
  `try_from(..)` with an assertion. `[clippy:as_conversions]`

### Assertions

Assertions detect **programmer errors**. Unlike operating errors, which are expected and must be
handled, an assertion failure means the code is wrong, and the only correct response to wrong code
is to crash. Assertions downgrade catastrophic correctness bugs into liveness bugs, and they
multiply the bug-finding power of fuzzing and seeded simulation.

- **Assert all function arguments and return values, preconditions, postconditions and
  invariants.** A function must not operate blindly on data it has not checked. Assertion density
  must average at least two per function. `[tidy:assertion-density]` `[tidy:unasserted-function]`
- **Every assertion has a message** that states the expectation: `"density must be positive"`, not
  `"bad"`. `[clippy:missing_assert_message]`
- **Use `assert_eq!` for equality.** It prints both values on failure. `[review]`
- **Split compound assertions.** Prefer `assert!(a, ..); assert!(b, ..);` over
  `assert!(a && b, ..)`. Separate assertions are easier to read and say exactly which condition
  failed. `[review]`
- **Assert implications with a single-line `if`:** `if a { assert!(b, ".."); }`. `[review]`
- **Pair assertions.** Enforce each property on at least two code paths. For example, assert
  positivity of reconstructed states when the reconstruction produces them and again when the
  Riemann solver consumes them. Assert conservation totals after the update and again after the
  boundary fill. `[review]`
- **Assert relationships between compile-time constants.** These check the design before the
  program runs:

  ```rust
  pub const GHOST_LAYERS: u32 = 2;
  pub const STENCIL_WIDTH: u32 = 5;
  const _: () = assert!(STENCIL_WIDTH == 2 * GHOST_LAYERS + 1, "stencil must fit the ghost layers");
  ```

- **Assert both the positive space and the negative space.** Assert what you expect, and also what
  you do not expect, because interesting bugs happen where data crosses from valid to invalid.
  Tests must likewise cover invalid inputs and states that are about to become invalid, not only
  valid data. `[review]`
- **Place assertions by cost** (ADR-0004):
  - `assert!` at function boundaries, before loops and once per step. Length and range asserts
    before a loop are free or faster, because they let the compiler drop bounds checks.
  - `debug_assert!` only for per-cell checks inside hot loops in `kernels`.
  - A per-pencil `assert!` backstop, such as "all finite, density > 0", written as a vectorisable
    reduction.

  `[tidy:debug-assert-placement]` `[review]`
- **No unchecked hints.** `assert_unchecked`, `unreachable_unchecked` and `get_unchecked` are
  unsafe, and `unsafe` exists only in the test allocator crate (ADR-0014). If the optimiser
  needs a fact, a real `assert!` provides it. `[rustc:unsafe_code]` `[tidy:unsafe]`
- **Assertions are a safety net, not a substitute for understanding.** References and fuzzers can show
  that bugs are present, never that they are absent. So:
  1. Build a precise mental model of the numerics first: the equations, the discretisation, the
     stability limit.
  2. Encode that model as assertions.
  3. Write code and comments that explain and justify the model to your reviewer.
  4. Let the references and the seeded simulation be the final line of defence.

### Errors

- **Operating errors are values. Programmer errors are crashes.**
  - Invalid user input (a configuration with a negative CFL number, unreadable initial data, a full
    disk) is an operating error. Return a `Result` with a typed error and handle it.
  - An inadmissible state *inside* the solver (negative density after an update, a NaN flux, a time
    step that violates the CFL limit computed for it) means the code or its stability analysis is
    wrong. Assert.
- **All errors are handled.** No `unwrap()`. No `let _ =` on a `Result`. No ignored
  `#[must_use]` values. `expect("...")` is allowed only where the failure is a programmer error,
  and the message says why it cannot happen. `[clippy:unwrap_used]`
  `[clippy:let_underscore_must_use]` `[rustc:unused_results]` `[review]`

  An analysis of production failures in distributed systems found that 92% of catastrophic failures
  came from incorrect handling of non-fatal errors
  ([Yuan et al., OSDI 2014](https://www.usenix.org/system/files/conference/osdi14/osdi14-paper-yuan.pdf)).

### Memory

- **All memory is allocated during initialisation. Nothing is allocated, freed or reallocated
  afterwards** (ADR-0003).
  - `kernels` is `#![no_std]` without `alloc`, so allocation there is a compile error.
    `[compiler]`
  - Elsewhere, `Solver::init()` sizes every field, ghost layer, scratch pencil and output buffer
    once. `[review]`
  - A counting allocator (`crates/alloc-guard`) fails any test that allocates inside
    `assert_no_allocations`. Wrapping the time loop in it is still to do.
    `[planned:alloc-after-init]`
- **Storage that must not grow is `Box<[T]>`, not `Vec<T>`.** A boxed slice has no `push`, so the
  type itself documents and enforces the fixed size. `[tidy:vec-field]`
- **No hidden allocation in the data plane.** That means no `collect()`, `clone()` of buffers,
  `format!` or `to_string()` after init. Formatting for output happens in the control plane, into
  preallocated buffers. `[review]` `[planned:alloc-after-init]`

### Scope and size

- **Declare variables in the smallest possible scope, and keep as few in scope as possible.**
  Shadow a name only to refine the same quantity, for example a unit conversion. Never reuse a name
  for a different quantity. `[clippy:shadow_unrelated]` `[review]`
- **Functions are at most 70 lines,** counting physical lines and including closures. There is a
  sharp discontinuity between a function that fits on a screen and one you have to scroll.
  `[tidy:function-length]` `[clippy:too_many_lines]`
  When splitting a function:
  - Aim for the inverse of an hourglass: few parameters, a simple return type, and a lot of meaty
    logic between the braces.
  - **Push `if`s up and `for`s down**
    ([matklad](https://matklad.github.io/2023/11/15/push-ifs-up-and-fors-down.html)). Keep all
    branching in the parent and move non-branchy work to helpers. In numerical code, choose the
    scheme, limiter or boundary type **outside** the cell loop (with `match` or generics), so that
    the inner loop is branch-free and easy to vectorise.
  - Centralise state changes. The parent holds the state. Helpers compute what should change and
    return it. Keep leaf functions pure.
- **Treat all compiler warnings at the strictest setting as errors from day one.** The workspace
  lints are pedantic plus selected restriction lints, and `-D warnings` applies in `just` and CI.
  Never put `#![deny(warnings)]` in source. `[clippy:pedantic]` `[review]`

### External events and control flow

- **Do not act directly in reaction to external events.** The solver runs at its own pace.
  Snapshots, checkpoints, logging and signal handling happen at step boundaries chosen by the time
  loop, never from a callback in the middle of a step. This keeps control flow under our control,
  allows batching, and keeps the work per step bounded. `[review]`
- **Split compound conditions** into nested `if`/`else` blocks so that every case is visible, and
  ask whether each `if` needs a matching `else` that handles or asserts the other case. `[review]`
- **State invariants positively.** `if index < count { … } else { … }` is easier to get right than
  `if index >= count`. `[review]`
- **Pass options explicitly at the call site. Never rely on defaults.** Rust has no named arguments,
  so:
  - Solver and scheme configuration (CFL number, limiter, Riemann solver, γ, floors, output cadence)
    lives in plain structs whose fields are all written out at construction.
  - **No `Default` implementations and no `..Default::default()` for numerical configuration.** A
    silently defaulted γ = 1.4 or CFL = 0.9 is a latent wrong answer. `[tidy:default]`
- **Always say why.** Explaining the reasoning helps the reader understand, makes them more likely
  to comply, and gives them the criteria to judge the decision. Every non-obvious choice gets a
  comment. In numerical code, that comment usually cites an equation or a paper. `[review]`

## Numerical code

These rules are new; TigerStyle has nothing equivalent. They encode what goes wrong in
finite-volume codes, and especially in agent-written ones.

### Conservation and form

- **Updates are written in conservative flux-difference form**:
  `u_next = u - (dt/dx) · (F_right − F_left)`, with exactly one flux per face, shared by its two
  neighbouring cells. Discrete conservation then holds to round-off, and the references assert it.
  `[planned:reference-suite]`

  ```rust
  /// Applies the conservative update `u_next = u - (dt/dx) (F_right - F_left)` to one pencil.
  ///
  /// `flux` holds one value per face, so it is one longer than `state`.
  pub fn update_pencil(state: &[f64], flux: &[f64], dt_over_dx: f64, state_next: &mut [f64]) {
      assert_eq!(flux.len(), state.len() + 1, "one flux per face");
      assert_eq!(state_next.len(), state.len(), "state buffers must match");
      assert!(dt_over_dx > 0.0, "time step must be positive");
      assert!(dt_over_dx.is_finite(), "time step must be finite");

      let cells = state_next.iter_mut().zip(state).zip(flux.array_windows::<2>());
      for ((next, &current), &[flux_left, flux_right]) in cells {
          *next = current - dt_over_dx * (flux_right - flux_left);
      }

      assert!(all_finite(state_next), "updated state must be finite; NaN is always a bug (ADR-0005)");
  }
  ```

  This is `crates/kernels/src/update.rs`, the template to copy for a new kernel. It also shows
  the kernel conventions:
  - lengths asserted before the loop;
  - no raw indexing (`array_windows` with array patterns, ADR-0003);
  - primitive arguments instead of `&self`;
  - a per-pencil backstop after the loop (ADR-0004);
  - names with qualifiers last.

  Its tests show the test conventions: exact expected bits, a check that the solution moved
  correctly, the conservation identity, and one test per assertion.

### Data access in kernels

- **Kernels access data only through pencils, fixed-size states and typed gathers** (ADR-0003):
  - `windows`, `array_windows` or `chunks_exact` over contiguous slices;
  - `[f64; NVAR]` states with named accessors;
  - `field[CellIndex]` gathers for indirect access.

  Raw slice indexing is denied. `[clippy:indexing_slicing]`
- **Hot loops are free functions over slices and primitives, without `self`.** The compiler then
  does not need to prove it can keep struct fields in registers, and a human reader can spot
  redundant work more easily. This is TigerStyle's rule, and it matches the pencil design. `[review]`

### Floating point

- **Results are bitwise reproducible across platforms** (ADR-0005):
  - Transcendental functions come only from `libm`, through `kernels::math`; the std methods are
    banned. `[clippy:disallowed_methods]` `[tidy:direct-libm]`
  - No `algebraic_*` operations. No fast-math of any kind. `[clippy:disallowed_methods]`
  - `mul_add` is allowed where written explicitly. Never rewrite an expression into `mul_add`
    without a reason, because doing so changes results. `[review]`
  - **Sums use `sum_fixed`** (or `sum_compensated` for conservation totals), never a naive fold.
    The lint sees `sum`, `product`, `reduce` and the `fold` family; a sum written as a `for`
    loop is for review.
    `[dylint:nami_float_reduction]` `[review]`
  - No iteration over `HashMap` or `HashSet`. `[clippy:iter_over_hash_type]`
    `[clippy:disallowed_types]`
- **NaN is always a bug.** Every state, flux and time step is finite, and that is asserted
  (per-step and per-pencil backstops, ADR-0004). Never test for NaN in order to skip or repair a
  value.
- **No magic tolerances.** Every epsilon is a named constant whose doc comment says where it comes
  from (for example "relative change below round-off for 64-bit pressure"). Test tolerances live in
  `reference/thresholds.toml`. `f64::EPSILON` is not a tolerance: it is the spacing at 1.0, not
  at your value. `[tidy:magic-tolerance]` `[planned:thresholds-file]` `[review]`
- **Do not compare floats with `==`** except against exact sentinels or in bitwise determinism
  tests, and say which one it is. `[clippy:float_cmp]` `[planned:float-eq-in-eq-fn]`
- **Guard every division by a quantity that can vanish** (density, pressure, wave-speed
  differences) with an assertion that it is positive or non-zero, placed before the division.
  `[review]`

### Physical admissibility

- **No silent floors, clamps or fix-ups.** `rho.max(1e-12)` hides a bug. It makes a failing test
  pass while the physics is wrong. A positivity-preserving limiter or a floor that is part of the
  scheme must:
  - be a named, documented component in the plan;
  - be configured explicitly (see *Pass options explicitly at the call site* above);
  - **count every activation** and report the count per step.

  An unexpected activation count is a test failure. `[tidy:magic-tolerance]` `[review]`
  `[planned:floor-activation-count]`
- **A wave speed must bound the true maximum eigenvalue.** A time-step controller that
  underestimates it is a stability bug even when the tests pass. Assert the bound.
  `[planned:reference-suite]`

### Ghost cells and buffer bleeds

- **Every ghost cell is written before it is read, every step.** In `dev`, `test` and
  `release-checked` builds, fill new and scratch buffers with NaN, so that reading a cell that was
  never written trips the finite-state assertion. This is nami's version of TigerStyle's "buffer
  bleed" rule: an under-filled buffer is a silent wrong answer, and it can also break determinism.
  `[planned:reference-suite]`
- **Each boundary-condition type has its own reference.** In the literature, boundary conditions were
  where agents were weakest (ADR-0007). `[planned:reference-suite]`

## Performance

> "The lack of back-of-the-envelope performance sketches is the root of all evil." — Rivacindela
> Hudsoni

- **Think about performance during design,** when the 1000× wins are available and nothing can yet
  be measured or profiled.
- **Sketch on the back of an envelope** against the four resources (network, disk, memory, CPU) and
  their two characteristics (bandwidth, latency).
  - Finite-volume updates are usually **memory-bandwidth bound**. Estimate the bytes moved and the
    flops per cell update before writing a kernel, and put the estimate in the plan.
  - Optimise the slowest resource first, after weighting by how often it is used.
- **Separate the control plane from the data plane.**
  - The control plane is the time loop, time-step control, boundary setup, I/O and diagnostics. It
    is assertion-dense.
  - The data plane is the pencil kernels. Its assertions are batched per pencil and per step.

  This separation is what makes heavy assertion safety cheap.
- **Batch.** Give the CPU large, predictable chunks of work. The CPU should be a sprinter in the
  100 m, not zig-zagging between lanes.
- **Be explicit. Minimise dependence on the compiler doing the right thing for you.** Assert slice
  lengths so that bounds checks disappear. Choose schemes outside the loop. Use fixed-lane
  reductions.
- **Performance is measured in instruction counts** (Gungraun) rather than wall-clock time on shared
  runners. Performance regressions are reviewed by a human. Never trade a style rule for a
  micro-optimisation without an ADR. `[review]`

## Developer experience

> "There are only two hard things in Computer Science: cache invalidation, naming things, and
> off-by-one errors." — Phil Karlton

### Naming

- **Get the nouns and verbs just right.** Good names capture what a thing is or does and show that
  you understand the domain. `[review]`
- **Follow Rust conventions:**
  - `snake_case` for functions, variables, modules and files;
  - `UpperCamelCase` for types and traits;
  - `SCREAMING_SNAKE_CASE` for constants.

  `[compiler]`
- **Acronyms are written as words: `HllcSolver`, `MhdState`, not `HLLCSolver`.** This deviates
  from TigerStyle (`VSRState`). We follow Rust's convention because the toolchain lints it and
  every Rust reader expects it. `[clippy:upper_case_acronyms]`
- **Do not abbreviate,** with one exception for numerical code: **the standard symbols of the
  referenced equations** (`rho`, `u`, `p`, `e`, `c` for sound speed, `q` for a state vector, `f` for
  a flux). Use them only where the function cites the equations they come from. Everything else is
  written out: `wave_speed_max`, not `smax`. Scripts use long flags (`--force`, not `-f`).
  `[review]`
- **Put units and qualifiers last,** ordered by descending significance:
  - `wave_speed_max`, `dt_max`, `density_floor`, `time_end_s`, `cells_interior_count`;
  - not `max_wave_speed`.

  Related names then line up and group together. `[review]`
- **Give related names the same length where possible,** so that related expressions line up.
  Use suffix pairs such as `rho_l`/`rho_r`, and prefer `source`/`target` over `src`/`dest`.
  `[review]`
- **Prefix helpers with their caller's name** to show the call history: `fill_ghosts()` and
  `fill_ghosts_reflecting()`. `[review]`
- **Callbacks and closures go last** in a parameter list, because they are also invoked last.
  `[review]`
- **Order items for the top-down reader:**
  - Module docs, then the main public items, then their helpers.
  - Inside a type: fields first, then associated types and constants, then methods.

  `[review]`
- **Do not overload names.** For example, "flux" means the numerical flux at a face, while the
  physical flux function is `flux_physical`. Pick one meaning per word and record it in
  `docs/glossary.md`. `[review]` `[planned:glossary]`
- **Prefer nouns that work in prose,** such as `solver.pencil` over `solver.sweeping`. `[review]`
- **Make arguments impossible to mix up.**
  - A function with two or more parameters of the same type that could be swapped (two `f64`
    lengths, two `u32` counts) takes newtypes or a parameter struct instead.
  - Symmetric functions such as `minmod(a, b)` are exempt.

  `[review]`

### Comments, docs and commits

- **Say why.** Code alone is not documentation. Comments explain why the code is the way it is, and
  in numerical code they cite their sources. `[review]`
- **Say how.** Every test starts with a comment stating its goal and method. For reference tests this
  includes the tier (ADR-0007), the expected order of accuracy and its source. `[tidy:test-goal]`
  `[planned:tier-tag]`
- **Every public item has a doc comment** (`missing_docs`). Doc comments put identifiers and
  formulas in backticks (`doc_markdown`). `[rustc:missing_docs]` `[clippy:pedantic]`
- **Comments are sentences,** with a capital letter and a full stop, or a colon if something follows.
  End-of-line comments can be phrases. `[review]`
- **Commit messages are descriptive and say why.** A PR description is not stored in git and does
  not show in `git blame`, so it is no substitute for a commit message. `[review]`

### Cache invalidation

- **Do not duplicate state or keep aliases of it.** Derived quantities (primitive variables from
  conserved ones, wave speeds) are recomputed or explicitly refreshed at a single point in the step.
  They are never cached in two places. `[review]`
- **Pass small `Copy` states (`[f64; NVAR]`) by value and anything large by reference.** Clippy's
  `large_types_passed_by_value` and `trivially_copy_pass_by_ref` enforce the thresholds.
  `[clippy:pedantic]`
- **Compute or check values close to where they are used.** A gap in time or space between the
  check and the use is where bugs live. For example, assert the CFL condition in the step that
  uses the time step, not three calls earlier. `[review]`
- **Prefer simpler return types:** `()` over `bool`, `bool` over `u32`, `u32` over `Option<u32>`, and
  `Option<u32>` over `Result<u32, E>`. Each step up adds branches at every call site, and those
  branches spread through the call chain. `[review]`

### Off-by-one errors

- **Indices, counts and sizes are distinct types** (ADR-0002). An index plus one is a count, and a
  count times a unit is a size. Encode the difference in the type, or at least in the name.
  `[review]`
- **Interior and ghost ranges come from helpers, never from arithmetic in the caller.** Per
  dimension, a pencil has `GHOST_LAYERS + interior + GHOST_LAYERS` cells and `interior + 1`
  interior faces. Write these relations once and assert them at compile time where possible.
  `[review]`
- **Show your intent in integer division:** `div_ceil`, or `/` together with an assertion that the
  remainder is zero. `[review]`

### Style by the numbers

- **Run `cargo fmt`.** Indentation is 4 spaces, the rustfmt default. `[fmt]`
- **Lines are at most 100 columns, without exception.** That is two copies of the code side by side
  on one screen. rustfmt cannot wrap long string literals or comments, so tidy checks the limit.
  `[tidy:line-length]`
- **Functions are at most 70 lines.** `[tidy:function-length]`

### Dependencies

- **Runtime dependencies are `libm` and nothing else.** Dev and tooling crates are allow-listed, and
  anything new requires an ADR (ADR-0001). Every dependency is a supply-chain, safety and
  performance risk, and for foundational code those costs compound. `[tidy:runtime-dependency]`
  `[deny]`

### Tooling

- **A small, standard toolbox beats a cabinet of specialist instruments.**
  - Our primary tool is Rust, so project scripts are `cargo xtask` subcommands, not shell scripts.
    That makes them portable, typed and testable.
  - `justfile` recipes are one-line entry points.
  - The only planned exceptions are the offline Python reference generators (ADR-0007).

  `[review]`

> "The right tool for the job is often the tool you are already using — adding new tools has a
> higher cost than many people appreciate." — John Carmack

## The last stage

Keep trying things out, keep the references honest, and remember the point of all this: a small,
fast solver whose answers you can trust.
