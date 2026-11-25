//! Core types shared by every nami crate.
//!
//! This crate is `no_std` without `alloc`, so the numerical kernels can depend on it
//! (ADR-0003).
#![cfg_attr(not(test), no_std)]

pub mod index;
