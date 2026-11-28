# ADR-0037: Extension points and how a run is composed

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

ADR-0035 asks for flexibility through a small, fixed set of extension points resolved at compile
time, and ADR-0003 bans `dyn` in kernels. ADR-0036 requires every combination the configuration
can select to be verified. This ADR defines the extension points and how a run is assembled from
them. Data layout and kernel shape are decided separately, in ADR-0043.

## Decision

1. **A run is a compiled combination.** A problem crate depends on nami, provides the initial
   conditions, user boundary conditions and user source terms, and names its combination of
   extension points as Rust types. It may compile several combinations and select one at init.
   The runtime configuration sets only parameters, validated at init into a `Result`. Sizes that
   help the compiler, such as the ghost width, the number of passive scalars and possibly the
   block size, may be compile-time constants of the combination.
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
   - **Coordinates:** face areas, cell volumes and geometric source terms. Cartesian is constant,
     and its update is written per direction as (Δt/Δxₐ) times the flux difference, so that it
     reduces exactly to the 1D form (ADR-0042).
   - **Source terms:** composed statically as tuples, applied in a fixed order.
   - **Boundary conditions:** chosen per block face at init, dispatched once per face per stage.

   This list maps onto ADR-0035's as follows:
   - wave-speed bounds and the eigenvector trait are not new extension points. They are parts
     of the equation-system point, split into small traits so that generic solvers ask only for
     what they use;
   - the equation of state belongs to the equation-system point, as in ADR-0035. Its interface is
     designed with M2's plan, Euler being the first system that has one;
   - refinement criteria stay an extension point, and their interface is designed with M9's plan;
   - outputs stay an extension point; ADR-0041 fixes the format, and the interface for
     user-defined outputs comes with the first plan that needs one.
3. **The face frame.** Riemann solvers see states rotated into the face frame, normal component
   first and the transverse components in cyclic order (for a y face: z, then x). That is an
   exact permutation on structured grids, and a general rotation on unstructured grids later.
   For MHD, the face supplies the normal magnetic field.
4. **Adding an extension point needs an ADR** (ADR-0035). Adding an implementation of an
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
- Generic code across several traits can become hard to read (ADR-0034, Failure). Each trait stays
  small and is documented with its equations and sources.

## Alternatives considered

- **One binary with every combination, selected at runtime.** Rejected as the default. It is just
  as fast in the hot loop, because selection happens once at init, but every build compiles the
  whole menu, and sizes such as the ghost width cannot be compile-time constants. A problem
  crate can still do this for a few combinations.
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
