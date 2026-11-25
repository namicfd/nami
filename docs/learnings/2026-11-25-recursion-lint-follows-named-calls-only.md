# The recursion lint follows named calls only

- Status: Open
- Found in: the guardrail review of 2026-11-25, while rewriting `nami_recursion`

## What happened

`nami_recursion` builds its call graph from call expressions with a named callee. Scratch runs
showed that these recursive shapes pass:

- a function passed by name and called by the receiver: `apply(walk, n)` where `walk` calls
  `apply(walk, n - 1)`;
- an overloaded operator whose impl calls back: `impl Add for State` that evaluates `a + b`;
- a `Drop`, `Deref` or `Index` impl that reaches itself;
- a call through `dyn Trait` to a required method, or through an `Fn` bound.

`kernels` bans `dyn` and function pointers (tidy rule `kernel-indirection`), but not `impl Fn`
parameters, and the other crates ban neither.

## Proposed guardrail

Dylint, in `lints/src/recursion.rs`: add an edge for a path to a function that is used as a
value, and for the method behind an overloaded operator, which the type-check results name.
Both should be small additions to the call-site collector. Each needs a firing and a non-firing UI
case.
