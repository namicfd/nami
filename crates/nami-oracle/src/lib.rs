//! Numerical oracles for nami (ADR-0007).
//!
//! Exact Riemann solvers, manufactured solutions, the convergence-order harness, invariant
//! checkers and reference data live here. This crate is code-owned: every change needs a human
//! review (ADR-0008). An oracle lands before the feature it tests, and is written by a different
//! agent session from the one implementing that feature.
//!
//! Every oracle is anchored to something outside the agent swarm: published tables (Toro),
//! generated source terms (`tools/oracle-gen/`, `SymPy`), or independent reference data
//! (`PyClaw`) with a provenance file.
