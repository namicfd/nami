//! Numerical references for nami (ADR-0007).
//!
//! Exact Riemann solvers, manufactured solutions, the convergence-order harness, invariant
//! checkers and reference data live here. This crate is code-owned: every change needs a human
//! review (ADR-0008). A reference lands before the feature it tests, and is written by a different
//! agent session from the one implementing that feature.
//!
//! Every reference is anchored to something outside the agent swarm: published tables (Toro),
//! generated source terms (`tools/reference-gen/`, `SymPy`), or independent reference data
//! (`PyClaw`) with a provenance file.
