//! Orchestration for nami: the control plane (`TIGER_STYLE.md`, Performance).
//!
//! This crate owns all allocation. `Solver::init` sizes every field, ghost layer, scratch pencil
//! and output buffer once, and nothing allocates after that (ADR-0003). It also runs the time
//! loop, chooses the time step, fills boundaries and writes output, calling into the
//! allocation-free kernels in `kernels`.
