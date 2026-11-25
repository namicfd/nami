//! nami's dylint library: rules that need the compiler's types and name resolution (ADR-0006).
//!
//! - `nami_recursion`: no function may reach itself through calls (ADR-0006).
//! - `nami_float_reduction`: no naive float sums outside test code (ADR-0005).
//!
//! This crate is tooling. It walks compiler data structures with the compiler's own (recursive)
//! visitors; the no-recursion rule it enforces applies to nami's crates.
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_data_structures;
extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod float_reduction;
mod recursion;

use rustc_hir::Expr;
use rustc_lint::{LateContext, LateLintPass, LintStore};
use rustc_session::Session;

dylint_linting::dylint_library!();

dylint_linting::declare_lint! {
    /// ### What it does
    ///
    /// Reports functions that call themselves, directly or through other functions, including
    /// through closures, through trait methods resolved to a concrete impl, and through generic
    /// functions that call back into a trait impl.
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

dylint_linting::declare_lint! {
    /// ### What it does
    ///
    /// Reports `Iterator::sum` with a float result, and `Iterator::fold` with a float
    /// accumulator whose closure adds to the accumulator with `+` or `+=`. Test code is exempt.
    ///
    /// ### Why is this bad?
    ///
    /// A naive sum adds in sequence. That is deterministic but slow, and the usual way to make
    /// it fast, letting the compiler reorder the additions, changes the result bits. nami's
    /// sums go through `kernels::reduce::sum_fixed`, which fixes a fast order in code, or
    /// through `sum_compensated` for conservation totals (ADR-0005).
    pub NAMI_FLOAT_REDUCTION,
    Deny,
    "naive float sum (ADR-0005)"
}

dylint_linting::declare_lint_pass!(NamiRecursion => [NAMI_RECURSION]);

dylint_linting::declare_lint_pass!(NamiFloatReduction => [NAMI_FLOAT_REDUCTION]);

/// Registers both lints. dylint calls this once per compiler session.
///
/// dylint finds this function by its symbol name, so the name must not be mangled, and no other
/// symbol in the library has it. `declare_late_lint!` generated the same attribute while the
/// library held a single lint; a library with several lints has to write it out.
#[unsafe(no_mangle)]
pub fn register_lints(session: &Session, lint_store: &mut LintStore) {
    dylint_linting::init_config(session);
    lint_store.register_lints(&[NAMI_RECURSION, NAMI_FLOAT_REDUCTION]);
    lint_store.register_late_lint_pass(Box::new(|_| Box::new(NamiRecursion)));
    lint_store.register_late_lint_pass(Box::new(|_| Box::new(NamiFloatReduction)));
}

impl<'tcx> LateLintPass<'tcx> for NamiRecursion {
    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        recursion::check_crate(cx, NAMI_RECURSION);
    }
}

impl<'tcx> LateLintPass<'tcx> for NamiFloatReduction {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expression: &'tcx Expr<'tcx>) {
        float_reduction::check_expr(cx, NAMI_FLOAT_REDUCTION, expression);
    }
}

// Goal: each lint fires exactly on the cases in `ui/`, compared against the expected output in
// `ui/*.stderr`, and stays silent on the false-positive cases there.
#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
