# ADR-0025: Extension points and how a run is composed

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

ADR-0023 asks for flexibility through a small, fixed set of extension points resolved at compile
time, and ADR-0003 bans `dyn` in kernels. ADR-0024 requires every combination the configuration
can select to be verified. This ADR defines the extension points and how a run is assembled from
them. Data layout and kernel shape are decided separately (`docs/ARCHITECTURE.md` § 4(a), pending
the layout prototype).

## Decision

1. **A run is a compiled combination.** A problem crate depends on nami, provides the initial
   conditions, user boundary conditions and user source terms, and names its combination of
   extension points as Rust types. It may compile several combinations and select one at init.
   The runtime configuration sets only parameters, validated at init into a `Result`.
2. **The extension points** (`docs/ARCHITECTURE.md` § 4(b) gives the details):
   - **Equation system:** associated `Conserved`, `Primitive` and `Parameters` types; conversion
     between conserved and primitive variables; the physical flux along a face normal; a
     guaranteed, asserted bound on the signal speed.
   - **Wave-speed bounds:** per system, guaranteed bounds on the Riemann fan.
   - **Riemann solvers:** `RiemannSolver<E>`, working in the face frame. LLF and HLLE are generic
     over systems with wave-speed bounds; HLLC and HLLD are implemented per system.
   - **Reconstruction:** per variable over rows, independent of the system. Characteristic
     reconstruction goes through a separate eigenvector trait. The ghost width is a constant of
     the type.
   - **Time integrators:** constant tables in the low-storage 3S* form, run by one generic
     driver. Implicit-explicit methods and super-time-stepping get their own drivers later.
   - **Coordinates:** face areas, cell volumes and geometric source terms. Cartesian is constant.
   - **Source terms:** composed statically as tuples, applied in a fixed order.
   - **Boundary conditions:** chosen per block face at init, dispatched once per face per stage.
3. **The face frame.** Riemann solvers see states rotated into the face frame, normal component
   first: an exact permutation on structured grids, a general rotation on unstructured grids
   later. For MHD, the face supplies the normal magnetic field.
4. **Adding an extension point needs an ADR** (ADR-0023). Adding an implementation of an
   existing one does not; it needs its references (ADR-0007).
5. **nami's test suite instantiates every supported combination,** and each one passes the
   reference tiers that apply to it.

## Enforcement

- The compiler rejects combinations that are not implemented, and `dyn` in kernels (tidy,
  ADR-0003).
- A test that enumerates the supported combinations and runs the references for each is planned
  with M0. Until a second combination exists, it covers one.
- Review: a new extension point without an ADR is rejected.

## Consequences

- Hot loops are fully specialised per combination, and no indirection is left.
- Switching a method means rebuilding the problem crate. Agents and researchers write problem
  crates, not just configuration files. Topic 6 designs that interface.
- Compile time grows with the number of combinations a build includes, not with the size of the
  menu.
- Generic code across several traits can become hard to read (ADR-0022, Failure). Each trait stays
  small and is documented with its equations and sources.

## Alternatives considered

- **One binary with every combination, selected at runtime.** Rejected as the default. It is just
  as fast in the hot loop, because selection happens once at init, but every build compiles the
  whole menu. A problem crate can still do this for a few combinations.
- **Dynamic dispatch per face or per cell.** Rejected. It prevents inlining and vectorisation and
  conflicts with ADR-0003.
- **Integrators as hand-written code per method.** Rejected. Tables read like the papers, and one
  driver is less code to verify.
- **Riemann solvers in the global frame, each handling directions itself.** Rejected. It
  multiplies the code by three, and it rules out reusing the solvers on unstructured grids.

## Evidence

- Athena++ (Stone et al. 2020, ApJS 249, 4): problem generators chosen at configure time, Riemann
  solvers in the face frame through index permutation, low-storage integrators as coefficient
  tables. These are background knowledge; check them against the source when implementing.
- Ketcheson 2010, J. Comput. Phys. 229, 1763 (low-storage 3S* Runge–Kutta methods).
- The maintainer's answers in the architecture discussion of 2026-11-26.
