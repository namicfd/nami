//! `nami_float_reduction`: no naive float sums outside test code (ADR-0005).
//!
//! The lint reports two shapes, in method-call form (`values.iter().sum::<f64>()`) and in path
//! form (`Iterator::sum::<f64>(values.iter())`):
//!
//! - a call to `Iterator::sum` whose result type is a float;
//! - a call to `Iterator::fold` whose accumulator type is a float and whose closure adds to the
//!   accumulator: the closure's first parameter is an operand of a `+`, or the target of a `+=`.
//!
//! A fold that takes a maximum or a minimum is not reported, and neither is a fold such as
//! Horner's rule (`|acc, c| acc * x + c`), where the accumulator is not itself an addend: the
//! order of such a recurrence is part of its meaning, and `sum_fixed` cannot replace it.
//!
//! Test code is exempt, because a naive sum is a legitimate independent cross-check there:
//! `#[cfg(test)]` items, `#[test]` functions, and integration tests (a crate built as a test
//! harness from the package's `tests/` directory).
//!
//! # Known gaps
//!
//! - A sum written as a loop (`for value in values { total += value; }`) is not reported. That
//!   is also how `kernels::reduce` itself is written.
//! - `fold` is checked only when its function is a closure written at the call, with a float
//!   accumulator. A float carried inside a tuple or struct accumulator, a function passed by
//!   name (`fold(0.0, f64::add)`), an accumulator copied to another variable before the
//!   addition, and a running difference (`-`, `-=`) are not seen.
//! - Other ways to add up an iterator are not seen: `reduce`, `try_fold`, `rfold`, `product`,
//!   `Sum::sum` called directly, `sum` into `Option<f64>` or `Result<f64, _>`, and `sum` whose
//!   result type is a generic parameter.
//! - An integration test is recognised by its path, `tests/` below the package root. A test
//!   target that the manifest places elsewhere is linted like library code.

use clippy_utils::diagnostics::span_lint;
use clippy_utils::{fn_def_id, is_in_test};
use rustc_hir::def::Res;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{AssignOpKind, BinOpKind, Expr, ExprKind, HirId, PatKind, QPath};
use rustc_lint::{LateContext, Lint};
use rustc_middle::ty::TyCtxt;
use rustc_span::sym;
use std::path::Path;

/// The directory, below the package root, where Cargo looks for integration tests.
const INTEGRATION_TEST_DIRECTORY: &str = "tests";

/// The environment variable in which Cargo passes the package root to the compiler.
const PACKAGE_ROOT_VARIABLE: &str = "CARGO_MANIFEST_DIR";

const MESSAGE: &str = "naive float sum; use `kernels::reduce::sum_fixed`, or `sum_compensated` \
                       for conservation totals (ADR-0005)";

/// Reports `expression` if it is a naive float sum outside test code.
pub fn check_expr<'tcx>(cx: &LateContext<'tcx>, lint: &'static Lint, expression: &'tcx Expr<'tcx>) {
    let (arguments, span) = match expression.kind {
        ExprKind::MethodCall(_, _, arguments, span) => (arguments, span),
        ExprKind::Call(_, arguments) => (arguments, expression.span),
        _ => return,
    };
    let tcx = cx.tcx;
    let Some(callee) = fn_def_id(cx, expression) else {
        return;
    };
    let iterator = tcx.get_diagnostic_item(sym::Iterator);
    if iterator.is_none() || tcx.trait_of_assoc(callee) != iterator {
        return;
    }
    // `sum` returns the sum and `fold` returns its accumulator, so one type check serves both.
    if !cx.typeck_results().expr_ty(expression).is_floating_point() {
        return;
    }
    // The names are compared as text: the compiler has no predefined symbols for them.
    let is_naive_sum = match tcx.item_name(callee).as_str() {
        "sum" => true,
        "fold" => arguments.last().is_some_and(|function| adds_to_accumulator(tcx, function)),
        _ => false,
    };
    if is_naive_sum && !is_test_code(tcx, expression.hir_id) {
        span_lint(cx, lint, span, MESSAGE);
    }
}

/// True if `function`, the last argument of a `fold`, is a closure that adds to its first
/// parameter, the accumulator.
fn adds_to_accumulator<'tcx>(tcx: TyCtxt<'tcx>, function: &'tcx Expr<'tcx>) -> bool {
    let ExprKind::Closure(closure) = function.kind else {
        return false;
    };
    let body = tcx.hir_body(closure.body);
    let Some(accumulator) = body.params.first() else {
        return false;
    };
    let mut finder = AdditionFinder { accumulators: Vec::new(), found: false };
    accumulator.pat.walk_always(|pattern| {
        if let PatKind::Binding(_, binding, _, _) = pattern.kind {
            finder.accumulators.push(binding);
        }
    });
    finder.visit_expr(body.value);
    finder.found
}

/// Looks for `accumulator + _`, `_ + accumulator` and `accumulator += _`.
struct AdditionFinder {
    /// The bindings of the closure's first parameter.
    accumulators: Vec<HirId>,
    found: bool,
}

impl AdditionFinder {
    fn is_accumulator(&self, operand: &Expr<'_>) -> bool {
        let operand = operand.peel_drop_temps().peel_borrows();
        matches!(
            operand.kind,
            ExprKind::Path(QPath::Resolved(None, path))
                if matches!(path.res, Res::Local(local) if self.accumulators.contains(&local))
        )
    }
}

impl<'tcx> Visitor<'tcx> for AdditionFinder {
    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        self.found |= match expression.kind {
            ExprKind::Binary(operator, left, right) if operator.node == BinOpKind::Add => {
                self.is_accumulator(left) || self.is_accumulator(right)
            }
            ExprKind::AssignOp(operator, target, _) if operator.node == AssignOpKind::AddAssign => {
                self.is_accumulator(target)
            }
            _ => false,
        };
        intravisit::walk_expr(self, expression);
    }
}

/// True inside `#[cfg(test)]` items and `#[test]` functions, and everywhere in an integration
/// test.
fn is_test_code(tcx: TyCtxt<'_>, id: HirId) -> bool {
    is_in_test(tcx, id) || is_integration_test_crate(tcx)
}

/// True if the crate being compiled is a test harness whose root file lies in the package's
/// `tests/` directory.
fn is_integration_test_crate(tcx: TyCtxt<'_>) -> bool {
    let session = tcx.sess;
    let (Some(package_root), Some(crate_root)) =
        (std::env::var_os(PACKAGE_ROOT_VARIABLE), session.io.input.opt_path())
    else {
        return false;
    };
    session.opts.test && is_integration_test_path(Path::new(&package_root), crate_root)
}

/// True if `crate_root` lies in the `tests/` directory directly below `package_root`. A
/// relative `crate_root` is resolved against the working directory, as the compiler does.
fn is_integration_test_path(package_root: &Path, crate_root: &Path) -> bool {
    assert!(package_root.is_absolute(), "Cargo passes the package root as an absolute path");
    let Ok(crate_root) = std::path::absolute(crate_root) else {
        return false;
    };
    crate_root
        .strip_prefix(package_root)
        .is_ok_and(|relative| relative.starts_with(INTEGRATION_TEST_DIRECTORY))
}

#[cfg(test)]
mod tests {
    use super::is_integration_test_path;
    use std::path::Path;

    // Goal: only a crate root in `tests/` directly below the package root is an integration
    // test; sources, benches and a `tests` directory further down or further up are not.
    #[test]
    fn integration_tests_are_recognised_by_path() {
        let package = Path::new("/work/nami/crates/solver");
        let is_test = |crate_root: &str| is_integration_test_path(package, Path::new(crate_root));
        assert!(is_test("/work/nami/crates/solver/tests/advection.rs"), "tests/<name>.rs");
        assert!(is_test("/work/nami/crates/solver/tests/euler/main.rs"), "tests/<name>/main.rs");
        assert!(!is_test("/work/nami/crates/solver/src/lib.rs"), "library source");
        assert!(!is_test("/work/nami/crates/solver/benches/step.rs"), "benchmark");
        assert!(!is_test("/work/nami/crates/solver/src/tests/mod.rs"), "module named tests");
        assert!(!is_test("/work/nami/crates/solver/tests_common/lib.rs"), "name prefix only");
        assert!(!is_test("/work/nami/crates/base/tests/index.rs"), "another package");
        let nested = Path::new("/work/tests/demo");
        assert!(!is_integration_test_path(nested, Path::new("/work/tests/demo/src/lib.rs")));
    }
}
