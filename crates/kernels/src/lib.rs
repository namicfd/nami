//! Numerical kernels for nami.
//!
//! This crate is `no_std` and does not link `alloc`, so heap allocation here is a compile error
//! (ADR-0003). Every kernel is bitwise deterministic across platforms (ADR-0005).
#![cfg_attr(not(test), no_std)]

pub mod math;
pub mod reduce;
