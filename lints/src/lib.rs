//! nami's dylint library: rules that need the compiler's name resolution (ADR-0006).
//!
//! This crate is tooling. It walks compiler data structures with the compiler's own (recursive)
//! visitors; the no-recursion rule it enforces applies to nami's crates.
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_data_structures;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

mod recursion;

use rustc_lint::{LateContext, LateLintPass};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Reports functions that call themselves, directly or through other functions, including
    /// through closures and through trait methods resolved to a concrete impl.
    ///
    /// ### Why is this bad?
    ///
    /// Recursion makes stack depth depend on the input, so some executions that should be
    /// bounded are not (TigerStyle, NASA Power of Ten). Use a loop over a bounded range with an
    /// explicit work list instead.
    pub NAMI_RECURSION,
    Deny,
    "function that can call itself (ADR-0006)"
}

impl<'tcx> LateLintPass<'tcx> for NamiRecursion {
    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        recursion::check_crate(cx, NAMI_RECURSION);
    }
}

// Goal: each lint fires exactly on the cases in `ui/`, compared against the expected output in
// `ui/*.stderr`, and stays silent on the false-positive cases there.
#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
