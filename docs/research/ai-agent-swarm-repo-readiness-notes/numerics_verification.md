# Numerics Verification and Testing Practices for Finite-Volume / Hyperbolic PDE Codes as Automated Oracles for AI Agents

Scope note: these notes are for "nami", a Rust finite-volume framework for hyperbolic conservation laws that AI agent swarms will implement. Research ran 2026-11-24 with a budget of about 20 tool calls. Some pages could not be fetched: the MOOSE MMS docs returned 403, the Trixi.jl testing docs returned 404 after a redirect, and the corries docs.rs page returned 404. Items marked **[background, not fetched]** are standard domain knowledge that I did not verify against a fetched source this session. The report writer should treat them as unsourced or verify them.

---

## 1. Code verification: MMS, order-of-accuracy / convergence tests, CI automation, and shock-capturing pitfalls

### Takeaway
The accepted standard for code verification is the method of manufactured solutions (MMS) or exact smooth solutions, combined with an observed-order-of-accuracy test. Production codes such as Athena++ automate this as a few hard thresholds: an absolute error at one resolution, plus an error *ratio* between two resolutions. For shock-capturing schemes, run order tests only on smooth problems. Limiters drop accuracy to first order near discontinuities and at smooth extrema, so the formal order is not reached there.

### Cited Findings
- Roy (2005, J. Comput. Phys. 205:131–156, "Review of code and solution verification procedures for computational simulation") defines code verification as procedures to find coding mistakes in the discretization. It recommends "the method of manufactured solutions combined with order of accuracy verification" for code verification. Solution verification (estimating numerical error in a given run) covers the observed order of accuracy, error bands, and practical mesh refinement — [Roy 2005 JCP accepted manuscript](https://www.aoe.vt.edu/content/dam/aoe_vt_edu/people/faculty/cjroy/Publications-Articles/cjr_jcp.revise.final-accepted.pdf)
- The verification criterion: the observed order of accuracy of the discretization error should asymptotically approach the formal order of the scheme as the mesh is systematically refined — [Roy publications (search excerpt)](https://www.aoe.vt.edu/content/dam/aoe_vt_edu/people/faculty/cjroy/Publications-Articles/JCEAP.verification.final.pdf); same framework in [Oberkampf & Roy, *Verification and Validation in Scientific Computing*, Cambridge, ch. "Code verification"](https://www.cambridge.org/core/books/abs/verification-and-validation-in-scientific-computing/code-verification/733E362E9B977EA1E3745BF9FAF607A0)
- Shocks: limiters that suppress oscillations reduce accuracy to first order near shock waves, and the formal order of the truncation error can vary in space (for example at a shock). Mixed first- and second-order behavior complicates grid-convergence analysis. Roy, McWherter-Payne and Oberkampf (Sandia hypersonic study) proposed alternative methods to find the order of accuracy for mixed 1st/2nd-order schemes — [Roy 2003 AIAA grid-convergence paper](https://www.aoe.vt.edu/content/dam/aoe_vt_edu/people/faculty/cjroy/Publications-Articles/grid-converg.submit-final.pdf); [Sandia report (UNT digital library)](https://digital.library.unt.edu/ark:/67531/metadc721119/m2/1/high_res_d/761061.pdf); [Roy IJNMF 2004](https://www.aoe.vt.edu/content/dam/aoe_vt_edu/people/faculty/cjroy/Publications-Articles/IJNMF.final.2004.pdf)
- **Concrete CI-automated order test (Athena++ `hydro/hydro_linwave.py`).** It runs a 3D linear wave with static mesh refinement at nx1 = 32 and 64 (nx2 = nx3 = nx1/2, meshblocks nx1/4), for each Riemann solver (hlle, hllc, roe). It reads the L1 error from `linearwave-errors.dat` and fails if any of these holds:
  - sound-wave L1 error at 64 cells > 3.7e-8 (4.3e-8 for hlle)
  - error ratio err(64)/err(32) > 0.325 (0.33 for hlle)
  - entropy-wave L1 error > 2.7e-8 (3.1e-8 for hlle)
  - entropy ratio > 0.33 (0.34 for hlle)
  - left-going and right-going sound-wave errors are not **exactly equal** (a symmetry check)

  A code comment says "Asymptotic second-order convergence should have ratio <= 0.25", but the enforced thresholds are looser (about 0.33, which corresponds to observed order ≈ 1.6), allowing for pre-asymptotic behavior at coarse resolution — [Athena++ hydro_linwave.py](https://raw.githubusercontent.com/PrincetonUniversity/athena/master/tst/regression/scripts/tests/hydro/hydro_linwave.py)
- The Athena++ regression wiki says analytic solutions or error convergence are the *preferred* pass/fail metrics. Exact comparison against stored reference solutions depends on compiler, processor and library versions, and inexact comparison is "subjective", so stored references should be used only for a limited set of tests — [Athena++ Regression Testing wiki](https://github.com/PrincetonUniversity/athena/wiki/Regression-Testing)
- Linear-wave tests (one per wave family, periodic domain) measure both diffusion and dispersion error. They are smooth, so second-order convergence is expected. The Athena++ MHD paper reports strict second-order overall convergence with VL2 + PLM — [Athena linear waves test page](https://www.astro.princeton.edu/~jstone/Athena/tests/linear-waves/linear-waves.html); [Athena++ AMR framework paper, arXiv:2005.06651](https://arxiv.org/pdf/2005.06651); [Athena original paper, arXiv:0804.0402](https://arxiv.org/pdf/0804.0402)
- AthenaK runs the linear-wave convergence test in full 3D. It then reruns the same wave with one corner (quadrant) of the mesh refined one level higher, which tests prolongation and restriction. This is a cheap AMR-correctness oracle — [AthenaK numerical relativity paper, arXiv:2409.10383](https://arxiv.org/pdf/2409.10383)

### Inferences
- Formulas **[background, not fetched; standard in Roy 2005 / Roache]**:
  - Observed order from two grids with refinement ratio r: `p_obs = ln(E_coarse / E_fine) / ln(r)`
  - Three grids without an exact solution: `p_obs = ln((f3 − f2)/(f2 − f1)) / ln(r)`
  - Richardson extrapolation: `f_exact ≈ f1 + (f1 − f2)/(r^p − 1)`
  - Roache's Grid Convergence Index: `GCI = Fs·|ε|/(r^p − 1)`, with Fs = 1.25 for three-grid studies and Fs = 3 for two-grid studies

  Athena++'s ratio threshold 0.325 with r = 2 corresponds to p_obs ≥ log2(1/0.325) ≈ 1.62.
- Recommended CI design for nami:
  - Per scheme/equation, run a smooth problem (MMS source term or exact smooth solution such as a linear wave or isentropic vortex) at 3–4 resolutions, for example N = 32, 64, 128, 256.
  - Assert `p_obs ≥ p_formal − δ` (δ ≈ 0.1–0.4 depending on coarseness) on the finest pair.
  - Also assert an absolute error ceiling at one resolution. This catches "converges at the right rate but with a wrong constant" bugs, such as a wrong wave speed in a flux term that still converges.
  - Pin the error norm (L1 is conventional for FV), the evaluation time, and the CFL number.
- For limited schemes: choose MMS solutions without extrema in the measured region, or measure in L1 rather than L∞ (TVD limiters clip smooth extrema to first order locally). Otherwise expect observed L∞ order around 1 even when the code is correct. Test unlimited reconstruction separately to verify the formal order.
- For discontinuous problems, expected L1 convergence rates are roughly O(h^1) for contact-free shocks with first-order schemes, and lower for contacts (about h^{1/2} for first-order, h^{2/3} for second-order) **[background, not fetched]**. Use these as one-sided bounds (p_obs ≥ about 0.5–0.6 on Sod), not exact targets.
- MMS for conservation laws: add the manufactured source as a *cell-averaged* source (high-order quadrature). Otherwise point-evaluated sources inject a second-order error that masks higher-order schemes **[background, not fetched]**. Symbolic generation (sympy, or Rust build-time codegen) avoids hand-derivation errors by agents.

### Gaps
- MOOSE's MMS Python tooling page (mooseframework.inl.gov/python/mms.html) returned 403, so I have no concrete MOOSE convergence-test API or tolerance details.
- The Salari & Knupp (Sandia SAND2000-1444) MMS report was not fetched, so its specific procedures are not cited here.
- I did not retrieve the exact GCI and Richardson formula text from a fetched source this session (Roy 2005 PDF identified but not read in full).

---

## 2. Standard benchmark problems with exact or reference solutions, and how major codes structure test suites

### Takeaway
Major codes rely on three layers: (a) a small number of analytic or convergence-based tests that are authoritative, (b) many short "smoke + compare a few numbers" regression runs against committed reference data, and (c) error-norm checks with tight tolerances against stored values (Trixi.jl). For nami, the benchmark ladder should go from linear advection to Burgers to shallow water (dam-break) to Euler (Sod/Toro tests, Shu–Osher, isentropic vortex) to MHD (Brio–Wu, Orszag–Tang, linear MHD waves). Each problem should be tagged by oracle type: exact, convergence, reference-solution, or invariant-only.

### Cited Findings
- **Athena++ regression framework:**
  - `tst/regression/run_tests.py` runs all tests, a suite (for example `gr`), or a single test (`gr/mhd_shocks_hlld`).
  - Each suite is a directory under `scripts/tests/` with `__init__.py`. Each test is a `.py` file defining `prepare()`, `run()` and `analyze()`, and `analyze()` returns True/False.
  - Utilities include `athena.configure/make/run` and `comparison.l1_norm/l1_diff`. `l1_diff` compares datasets on a merged refined grid from both sets of cell faces, without interpolation.
  - Reference data lives in `data/`.

  — [Athena++ Regression Testing wiki](https://github.com/PrincetonUniversity/athena/wiki/Regression-Testing)
- **Trixi.jl:**
  - Tests run full "elixirs" (example scripts) via `@test_trixi_include(elixir; l2=[...], linf=[...])`. The macro recomputes errors through the `analysis_callback` and compares each value with `isapprox`.
  - Default tolerances are `atol = 500*eps(Float64)` (about 1.1e-13) and `rtol = sqrt(eps(Float64))` (about 1.5e-8).
  - Tests also fail if *anything* is written to stderr, apart from whitelisted regex patterns such as module-replacement warnings.
  - A separate `@test_allocations` macro asserts `(@allocated rhs!(...)) < allocs`, which enforces zero or bounded allocation in the hot path.

  — [TrixiTest.jl macros.jl](https://raw.githubusercontent.com/trixi-framework/TrixiTest.jl/main/src/macros.jl); [Trixi.jl test_trixi.jl](https://raw.githubusercontent.com/trixi-framework/Trixi.jl/main/test/test_trixi.jl)
- **Clawpack:**
  - Classic, amrclaw and geoclaw use `make tests`, which runs Python scripts that execute the Fortran code and compare "a small set of values derived from the output" against stored reference data. The docs say these tests give limited coverage.
  - Other tools: `chardiff.py` for full-output line diffs and `imagediff.py` for pixelwise plot diffs.
  - PyClaw uses verifier functions with `check_diff()` (absolute or relative tolerance) and `gen_variants()` to run the same test across kernel languages (Fortran/Python) and serial/parallel (PyClaw/PetClaw). CI runs basic tests per PR (Travis at the time).

  — [Clawpack regression docs v5.8](https://depts.washington.edu/clawpack/sampledocs/v580/regression.html)
- The clawpack/regression repo proposes short tests that produce small output (for example one set of gauge results) committed alongside the code. When new output is judged "correct", the new output data is committed. A GeoClaw PR updated gauge files after minor numeric changes caused by changes in how AMR regions are applied — [Clawpack regression search excerpts](https://github.com/clawpack/regression); [GeoClaw PR #562](https://github-users.yhajoa.workers.dev/clawpack/geoclaw/pull/562)
- AthenaK verifies AMR prolongation/restriction by rerunning the linear wave with a refined corner of the mesh — [arXiv:2409.10383](https://arxiv.org/pdf/2409.10383)

### Inferences
- Benchmark catalog with oracle types **[background, not fetched; parameters are the standard published ones and should be verified against Toro, *Riemann Solvers and Numerical Methods for Fluid Dynamics*, 3rd ed., §4.3 and §6.4 tests]**:
  - **Linear advection** (smooth sine/Gaussian, plus square wave and Jiang–Shu combined profile). Oracle: exact shifted profile after integer periods. Use it for the order test (smooth) and for TVD/max-principle checks (square wave).
  - **Burgers.** Oracle: exact solution via characteristics before shock formation (smooth order test). After shock formation use the exact entropy solution, for example the Riemann problem shock/rarefaction, and check shock position against the Rankine–Hugoniot speed (u_L+u_R)/2.
  - **Shallow water.** Oracles: 1D dam-break (Stoker wet bed / Ritter dry bed) exact solution; "lake at rest" well-balancing must be preserved to machine precision with bathymetry; positivity of h for dry-bed cases.
  - **Euler.**
    - Sod (ρ,u,p) = (1,0,1)/(0.125,0,0.1), γ = 1.4, t = 0.2 (Toro Test 1 uses (1,0.75,1)/(0.125,0,0.1)).
    - Toro tests 2–5: 123 problem with near-vacuum, strong blast waves, shock collision. Oracle: exact Riemann solver (Toro ch. 4 Newton iteration for p*), compared in L1 with p_obs ≈ 0.5–1 expected.
    - Shu–Osher. Oracle: reference solution from a very fine run, so this is a regression and qualitative test only.
    - Isentropic vortex (Shu, Yee et al.). Oracle: exact advection of the vortex, giving a smooth 2D order test.
  - **MHD.**
    - Linear MHD waves (fast/slow/Alfvén/entropy). Oracle: exact, convergence (as in Athena++).
    - Circularly polarized Alfvén wave. Oracle: exact nonlinear solution.
    - Brio–Wu. Oracle: reference only (no exact solution due to compound waves).
    - Orszag–Tang. Oracle: reference/qualitative plus ∇·B and symmetry checks.
- For an AI-agent oracle, the **exact** and **convergence** tests are the strong signals. Reference-solution tests (Shu–Osher, Brio–Wu, Orszag–Tang) can only detect *change*, not correctness, so agents must not be allowed to regenerate their reference data without human review. This is Clawpack's "commit new output if deemed correct" step, which is a human judgment call.
- Copy Trixi.jl's "stderr must be empty" and "allocation bound on RHS" patterns. They map directly to TigerStyle goals (no dynamic allocation after init, no warnings). In Rust, use a counting global allocator in tests to assert zero allocations per time step.

### Gaps
- I did not fetch AthenaK's own test harness (github.com/IAS-Astrophysics/athenak `tst/`), or the test docs for PLUTO, FLASH, Dedalus, deal.II or AMReX. Their regression-structure details are uncovered.
- Exact Toro test parameters and exact-solver algorithm details were not fetched this session.

---

## 3. Invariant and property checks (conservation, positivity, entropy, TVD, symmetry, invariance, CFL, determinism)

### Takeaway
Property checks are the cheapest and most agent-robust oracles. They need no reference data and they apply on every run. Production codes and recent AI-simulation verifiers use the same list: conservation residuals, positivity/physical bounds, monotonicity/TVD, entropy inequality, and exact symmetry preservation.

### Cited Findings
- Athena++ enforces **exact** (bitwise) equality of L1 errors between the left-going and right-going sound-wave runs. A mirror-symmetric problem must give identical errors, with no tolerance — [Athena++ hydro_linwave.py](https://raw.githubusercontent.com/PrincetonUniversity/athena/master/tst/regression/scripts/tests/hydro/hydro_linwave.py)
- The "Judge Agent" for AI-generated simulation code (Yang, arXiv:2603.25780, March 2026) uses two sets of checks:
  - Five pre-execution gates: dimensional consistency, boundary/initial-condition compatibility, well-posedness (coercivity, **CFL stability**, Lipschitz bounds, convergence-rate checks), problem classification, and cost estimation.
  - A post-execution audit: **conservation residuals, monotonicity, entropy inequality, symmetry preservation, physical bounds**, residual evaluation, and archetype matching.

  Its motivating silent-failure example is a CFL violation in a time integrator that produced plausible-looking stress about three times too large — [arXiv:2603.25780](https://arxiv.org/html/2603.25780v1)
- AthenaK's refined-corner linear-wave rerun is in effect an invariance check: adding AMR should not degrade a smooth solution beyond the expected error — [arXiv:2409.10383](https://arxiv.org/pdf/2409.10383)

### Inferences
Concrete CI-automatable checks for nami **[background, not fetched; these are standard FV properties]**:
- **Discrete conservation.**
  - On periodic or closed domains, assert `|Σ_i U_i^n ΔV_i − Σ_i U_i^0 ΔV_i| ≤ C · n_steps · ε · Σ|U_i ΔV_i|` (C about 10). This is round-off-level growth, not truncation-level.
  - With inflow/outflow boundaries, check the budget `ΔTotal = −∫ boundary flux dt` to the same tolerance.
  - This is the single most powerful bug detector for flux-form FV: any asymmetric face flux use, double counting, or ghost-cell error breaks it.
  - Use compensated (Kahan/Neumaier) summation for the totals so the check itself is not noise-limited.
- **Positivity.** Assert ρ > 0 and p > 0 (and h ≥ 0 for shallow water) after every stage on the Toro 123 problem and on dam-break-to-dry-bed. Express it as a TigerStyle assertion in debug builds and as a test assertion in CI.
- **TVD / maximum principle.** For scalar advection or Burgers with a TVD limiter, assert `TV(u^{n+1}) ≤ TV(u^n) + tol` and `min u^0 − tol ≤ u^n ≤ max u^0 + tol` on square-wave initial data, with tol = O(ε·max|u|).
- **Entropy.** For entropy-stable schemes, assert that the discrete total entropy `Σ η(U_i) ΔV_i` is non-increasing (periodic BCs, up to round-off). For entropy-conservative fluxes alone (no dissipation), assert that it is conserved to near round-off on smooth data with high-order time stepping.
- **Symmetry.**
  - Mirror-symmetric initial data (x → −x with u → −u) must give mirror-symmetric output *bitwise*, Athena++ style.
  - Rotating a 1D problem to the y/z axes must give bitwise-identical profiles on Cartesian grids.
  - This requires the flux code to be direction-agnostic and the summation order to be symmetric; otherwise relax to ULP-level tolerance.
- **Galilean invariance.** A Riemann problem in a frame moving with uniform velocity v should give results shifted by v·t, up to truncation error, not bitwise. Use it as a tolerance-based check or a convergence check.
- **Free-stream preservation.** A uniform state must remain uniform to machine precision on any mesh, including curvilinear and AMR meshes.
- **Well-balancing (shallow water).** Lake-at-rest preserved to machine precision.
- **∇·B (MHD).** max|∇·B| ≤ O(ε) for constrained transport, or bounded/decaying for cleaning schemes.
- **CFL.** Assert `dt ≤ CFL · min(Δx / (|u| + c))` and that the computed max wave speed is ≥ the true max eigenvalue (use the exact eigenvalues as an oracle in unit tests). Fuzz the wave-speed estimators with random valid states.
- **Determinism.** Two runs with the same build and inputs must give bitwise-identical output, including with multithreading (fixed reduction order). This should be a CI test, see section 4.

### Gaps
- I found no fetched source giving a published numeric tolerance convention for conservation checks (for example "N·ε"). The formula above is my recommendation.
- I did not fetch a positivity-preserving or entropy-stable scheme paper (Zhang–Shu, Tadmor, Chandrashekar) with formal test procedures.

---

## 4. Regression testing of floating-point output: tolerances, golden files, snapshots, cross-platform determinism

### Takeaway
Bitwise equality is achievable and valuable *within a pinned build configuration* (same compiler, flags, FMA policy and thread decomposition). Across platforms, use tolerance-based or statistical comparison. FMA contraction is the most common cause of cross-platform divergence. Trixi.jl's default (atol 500ε, rtol √ε on error norms) is a well-tested baseline for "golden" error-norm comparison.

### Cited Findings
- Trixi.jl compares l2/linf error norms against hardcoded reference values with `isapprox(atol=500*eps(), rtol=sqrt(eps()))`. Reference values are written into the test file next to the elixir call. That makes them a reviewable "golden number" diff rather than a binary golden file — [TrixiTest.jl macros.jl](https://raw.githubusercontent.com/trixi-framework/TrixiTest.jl/main/src/macros.jl)
- Athena++ warns that exact comparison to stored references depends on compiler, processor and library versions, and so prefers analytic and convergence oracles — [Athena++ Regression wiki](https://github.com/PrincetonUniversity/athena/wiki/Regression-Testing)
- FLiT (Floating-point Litmus Tester, Sawaya et al. 2017) quantifies result variability across compilers, flags and platforms. It argues strict bitwise identity is not always the right target: accept results within a developer-defined acceptance tolerance around a trusted baseline — [FLiT paper](https://pruners.github.io/pdf/sawaya_flit_2017.pdf); [FLiT extension, NSF PAR](https://par.nsf.gov/servlets/purl/10101376)
- In LLNL's MFEM study (Ahn et al.), most of the fastest builds were bitwise reproducible. The tooling isolates which optimizations cause divergence — [arXiv:1811.05618](https://arxiv.org/pdf/1811.05618v1)
- CESM's Ensemble Consistency Test uses statistical distinguishability rather than bit-for-bit comparison. It caught large discrepancies from FMA optimizations and a compiler bug. CESM output is bit-reproducible only when code, compiler and flags, MPI library and processor counts all match — [CESM-ECT, NSF PAR](https://par.nsf.gov/servlets/purl/10304358); [CACM, "Keeping Science on Keel When Software Moves"](https://cacm.acm.org/research/keeping-science-on-keel-when-software-moves)
- FELTOR: fixing the order of floating-point operations and avoiding value-changing optimizations gives bitwise reproducibility, but only up to the hardware's FMA behavior. Compilers can rewrite `a*x+b*y` in several ways that round differently — [arXiv:1807.01971](https://arxiv.org/pdf/1807.01971)
- SLEEF notes that FP contraction (merging multiply and add into an FMA) introduces inconsistency, and provides bit-consistent function variants with and without FMA — [arXiv:2001.09258](https://arxiv.org/pdf/2001.09258)
- Intel recommends `-no-fma` for reproducibility across FMA and non-FMA processors, `-fp-model consistent` (v17+), and `-fimf-arch-consistency=true` for math-library consistency — [Intel article](https://www.intel.com/content/www/us/en/developer/articles/technical/consistency-of-floating-point-results-using-the-intel-compiler.html)
- Clawpack compares "a small set of values derived from the output" (small golden data), with `chardiff`/`imagediff` for full-output diffs. GeoClaw maintainers periodically regenerate regression data after intentional algorithmic changes — [Clawpack regression docs](https://depts.washington.edu/clawpack/sampledocs/v580/regression.html); [GeoClaw PR #562](https://github-users.yhajoa.workers.dev/clawpack/geoclaw/pull/562)

### Inferences
- Rust-specific notes **[background, not fetched]**:
  - rustc does **not** contract `a*b+c` into an FMA unless you call `f64::mul_add`. Basic IEEE ops are deterministic.
  - Divergence sources:
    - `f64::sin`/`exp`/`powf`, which call the platform libm and differ across OS/arch. Use the `libm` crate or a pinned implementation for golden tests.
    - Explicit `mul_add` on non-FMA targets, which falls back to a software call that is correctly rounded but slow.
    - SIMD reductions and `rayon` parallel reductions with nondeterministic order.
    - `-C target-cpu=native`.
  - Policy: ban `target-cpu=native` in golden-test builds; use a deterministic, fixed-tree reduction order for parallel sums; route transcendental functions through one pinned implementation.
- Tiered tolerance strategy for nami:
  1. **Bitwise** (hash of the output state) for same-platform determinism and for refactors that should not change arithmetic. This gives a fast, unambiguous agent signal.
  2. **ULP-distance** (for example ≤ 4 ULP per value) for changes that reorder a few operations.
  3. **Relative error-norm tolerance** (Trixi.jl-style rtol √ε on L1/L2/L∞ error norms vs exact) across platforms.
  4. **Convergence/analytic oracles** that are platform-independent by construction.
- Golden-file hygiene for agent swarms:
  - Store small golden outputs: error norms, conserved totals, a few probe points (Clawpack/Trixi style), not full fields.
  - Make golden updates a separate, human-approved commit type, with CI blocking any PR that both changes numerics code and rewrites golden data.
  - Snapshot tools such as Rust `insta` can hold golden numbers as text so diffs are reviewable. Format floats with `{:e}` and full round-trip precision (17 significant digits) or as hex bit patterns.

### Gaps
- No fetched source on Rust-specific floating-point determinism (libm variance across targets, `mul_add` codegen). The points above are from background knowledge.
- No fetched source on ULP-based comparison conventions in CFD codes specifically.

---

## 5. Reports of LLMs/AI agents writing scientific/numerical simulation code: what verification made it work or fail

### Takeaway
The 2025–2026 literature agrees that execution success ("it runs") and LLM self-review do not catch semantic numerical errors. Generated solvers often run cleanly while solving the wrong physics. Grounded, automated checks against exact or reference solutions, conservation, CFL and physical bounds are what separate success from failure. Rates of silent failure without such checks are high: 42% in one study.

### Cited Findings
- **LLM-PDEveloper** (arXiv:2509.25194), a multi-agent generator of new PDE modules inside an existing library.
  - Target: XLB, a JAX lattice-Boltzmann library. Tasks: a new advection-diffusion (AD) solver, AD boundary conditions, an ADR solver, and a power-law non-Newtonian solver.
  - Pass rates (10 trials each):
    - AD solver: 8/10 (o1-preview), 7/10 (o3-mini), 7/10 (Claude 3.5 Sonnet)
    - BCs for AD: 0/10, 3/10, 0/10
    - ADR solver: 6/10, 7/10, 6/10
    - Non-Newtonian solver: 10/10, 5/10, 10/10
  - Verification was weak: one generated "Tester" case run by a non-LLM Checker, which flags only interpreter errors, plus two LLM "Inspector" agents. No MMS or convergence rates.
  - Semantic failures dominated: misreading the PDE (dropping the streaming step that carries advection), applying Dirichlet/Neumann conditions to the wrong boundaries, and "spurious programs that run without error but do no meaningful computation".
  - The authors found that LLM inspection did not reliably catch semantic errors. Comparison to reference solutions exposed the missing advection, but they called that impractical for new PDEs without ground truth.

  — [arXiv:2509.25194](https://arxiv.org/html/2509.25194)
- **Judge Agent** (Yang, arXiv:2603.25780, March 2026).
  - Results: silent-failure rate falls from 42% to 1.5% across 134 test cases in 12 domains (12 development, 72 prospective, 50 adversarial). The 72 blinded tasks from 12 external scientists reached 89% success (95% CI 80–95%) with the Judge vs 53% without.
  - Backbone: Claude Sonnet 4.6 at temperature 0. Opus 4.6 scored 12/12 on the development set.
  - Remaining failures cluster at bifurcation points.
  - The checks are listed in section 3.
  - Caveats: single author who founded the vendor company (declared conflict of interest), and self-described unvalidated claims about the framework's completeness.

  — [arXiv:2603.25780](https://arxiv.org/html/2603.25780v1)
- **CodePDE** (Li, Marwah, Shen, Sun, Risteski, Yang, Talwalkar; arXiv:2505.08783) frames PDE solving as LLM code generation, with self-debugging, self-refinement and test-time scaling. It evaluates 16 LLMs on accuracy, efficiency and failure modes, and reports trade-offs between solver reliability and sophistication. The details of its evaluation metric were not visible in the abstract — [arXiv abs 2505.08783](https://arxiv.org/abs/2505.08783); search summary in [arXiv PDF](https://arxiv.org/pdf/2505.08783)
- **"Your Simulation Runs but Solves the Wrong Physics"** (arXiv:2605.09360) targets LLM-generated MOOSE input files. It reconstructs the PDE the generated code actually encodes and compares it with the intended PDE, without relying on execution or LLM self-judgment — [arXiv:2605.09360](https://arxiv.org/html/2605.09360v1) (search-result summary only; not fetched in full)
- **SciML Agents** (arXiv:2509.09936) asks whether LLMs can write runnable, scientifically appropriate ODE solver code (for example choosing stiff vs non-stiff solvers). Results are preliminary — [ar5iv 2509.09936](https://ar5iv.labs.arxiv.org/html/2509.09936)
- An agentic scientific-simulation position paper (arXiv:2603.00214) discusses execution-grounded agents, underspecification and reproducibility, and cites MooseAgent (an LLM with iterative verification against solver feedback) — [arXiv:2603.00214](https://arxiv.org/html/2603.00214v1) (search summary only)

### Inferences
- For nami, the agent oracle has to be numerical and grounded: convergence order, exact-solution error, conservation, positivity, symmetry. "Compiles and runs" or a reviewer-agent sign-off is not enough. LLM-PDEveloper's failure modes (wrong-boundary BCs, dropped terms, no-op code) would each be caught by an MMS order test, a conservation check or a free-stream check.
- Boundary conditions were the weakest area (0–3/10 pass). nami should give each BC type its own dedicated oracle tests: reflecting-wall symmetry vs a doubled periodic domain, outflow with zero reflection of a simple wave, and inflow-fixed values.
- The "spurious no-op program" failure means the test harness must assert that the solution *changed* in the expected way (for example the wave moved by c·t), not only that errors are small. A zero-update solver passes trivially on steady states and on "error vs initial condition" checks.
- Keep the test suite and reference data outside the agent's writable scope, or gated by CODEOWNERS. Otherwise agents may "fix" failing oracles. This is an inference; no source above measured reward hacking of tests in numerical codes.

### Gaps
- I found no published report (as of Nov 2026) of an AI agent swarm building a full production FV/hyperbolic framework, or quantitative data on which verification layer caught which agent errors in such a project.
- I did not fetch CodePDE's full results (per-PDE accuracy, failure-mode taxonomy).
- arXiv:2605.09360 and arXiv:2603.00214 are summarized from search snippets only.

---

## 6. Existing Rust numerical PDE / CFD crates and their testing

### Takeaway
The Rust ecosystem has no mature, widely used finite-volume hyperbolic conservation-law framework comparable to Clawpack, Athena++ or Trixi.jl. The closest is `corries`, a small 1D Riemann-solver hydrodynamics crate. nami would be early in this space and cannot copy a Rust test-suite precedent, so it should take its test design from Athena++, Trixi.jl and Clawpack.

### Cited Findings
- `corries` (crates.io/docs.rs, v0.6.x) describes itself as a "CORrosive RIEmann Solver for 1d hydrodynamics problems", aimed at non-relativistic astrophysics, with a planned Kurganov–Tadmor flux. MUSCL reconstruction was not confirmed in search results — [docs.rs corries 0.6.0](https://docs.rs/crate/corries/0.6.0) (the page returned 404 on direct fetch; info is from the search index)
- `flow2d_rs` is a Rust 2D viscous *incompressible* Navier–Stokes solver on a staggered grid, with no compressible Riemann solver — [docs.rs flow2d_rs](https://docs.rs/crate/flow2d_rs/latest); [lib.rs flow2d_rs](https://lib.rs/crates/flow2d_rs)
- `lau_conservation_laws` on docs.rs is about proving properties of CUDAclaw "cell agents", not a general solver — [docs.rs lau_conservation_laws](https://docs.rs/lau-conservation-laws/latest/lau_conservation_laws/)
- A search for Rust FV hyperbolic or Euler solvers mostly returned non-Rust codes: ryujin (C++/deal.II, high-order FE for Euler/NS), hyper2D (Fortran FV, first-order and MUSCL), FiniteVolumeSolver (C++/AMReX), and FVENS (C++) — [ryujin](https://github.com/conservation-laws/ryujin); [hyper2D](https://github.com/sbocce/hyper2D); [FiniteVolumeSolver](https://github.com/maikel/FiniteVolumeSolver); [FVENS](https://github.com/Slaedr/FVENS)

### Inferences
- With no Rust precedent, nami's test infrastructure will need to be built from scratch. Candidate building blocks from the Rust ecosystem **[background, not fetched]**:
  - `cargo test`/`cargo nextest` for the tiered suites
  - `proptest` for property tests (random valid states feeding flux consistency F(U,U) = F(U), wave-speed bounds, and positivity of Riemann solver outputs)
  - `approx` or `float-cmp` for ULP/relative comparisons
  - `insta` for reviewable golden snapshots
  - a counting `#[global_allocator]` for zero-allocation assertions, mirroring Trixi's `@test_allocations`
  - `criterion` for performance regression
- An exact Riemann solver for Euler (Toro ch. 4), plus exact solutions for advection, Burgers and shallow-water dam-break, should be written early. Ideally a human writes them, or they are cross-checked against a reference implementation such as Clawpack's Riemann repo or Toro's NUMERICA. They are oracles, so they need higher trust than the code under test.

### Gaps
- I could not fetch the `corries` source or tests, so its testing practice is unknown.
- I did not find or evaluate other potentially relevant Rust crates (for example any Rust lattice-Boltzmann, SPH, or DG crates, or a Rust port of Clawpack Riemann solvers). A crates.io search by keyword ("riemann", "hllc", "godunov", "finite-volume") was not done this session.
