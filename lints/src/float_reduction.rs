//! `nami_float_reduction`: no naive float sums outside test code (ADR-0005).
//!
//! The lint reports these calls of `Iterator` and `DoubleEndedIterator` methods, in method-call
//! form (`values.iter().sum::<f64>()`) and in path form (`Iterator::sum::<f64>(values.iter())`):
//!
//! - `sum` or `product` whose result is a float, or an `Option` or `Result` of a float;
//! - `fold` or `rfold` whose result is a float, `try_fold` or `try_rfold` whose initial value
//!   is a float, and `reduce` whose result is an `Option` of a float, when their function adds
//!   to or subtracts from the accumulator.
//!
//! The function adds to or subtracts from the accumulator if it is a closure whose first
//! parameter, the accumulator, is an operand of a `+` or `-` or the target of a `+=` or `-=`,
//! or if it is a path to the method of `Add` or `Sub` (`f64::add`, `core::ops::Sub::sub`).
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
//! - A fold is checked only with a float accumulator, and only when its function is a closure
//!   written at the call or a path to `Add` or `Sub`. A float carried inside a tuple or struct
//!   accumulator, a function or closure bound to a name first, an accumulator copied to another
//!   variable before the addition, and a fold that multiplies (`fold(1.0, f64::mul)`) are not
//!   seen.
//! - `Sum::sum` and `Product::product` called directly, and a `sum` whose result type is a
//!   generic parameter, are not seen.
//! - An integration test is recognised by its path, `tests/` below the package root. A test
//!   target that the manifest places elsewhere is linted like library code.

use clippy_utils::diagnostics::span_lint;
use clippy_utils::{fn_def_id, is_in_test};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{AssignOpKind, BinOpKind, BodyId, Expr, ExprKind, HirId, PatKind, QPath};
use rustc_lint::{LateContext, Lint};
use rustc_middle::ty::{self, Ty, TyCtxt};
use rustc_span::{Symbol, sym};
use std::path::Path;

/// The directory, below the package root, where Cargo looks for integration tests.
const INTEGRATION_TEST_DIRECTORY: &str = "tests";

/// The environment variable in which Cargo passes the package root to the compiler.
const PACKAGE_ROOT_VARIABLE: &str = "CARGO_MANIFEST_DIR";

/// The diagnostic name of the trait that declares `rfold` and `try_rfold`. The compiler has no
/// predefined symbol for it.
const DOUBLE_ENDED_ITERATOR: &str = "DoubleEndedIterator";

const SUM_MESSAGE: &str = "naive float sum; use `kernels::reduce::sum_fixed`, or \
                           `sum_compensated` for conservation totals (ADR-0005)";

const PRODUCT_MESSAGE: &str = "naive float product; `kernels::reduce` has no deterministic \
                               product yet; ask for one with `cargo xtask issue file --kind \
                               guardrail-gap` (ADR-0005)";

/// Reports `expression` if it is a naive float sum or product outside test code.
pub fn check_expr<'tcx>(cx: &LateContext<'tcx>, lint: &'static Lint, expression: &'tcx Expr<'tcx>) {
    let (arguments, span) = match expression.kind {
        ExprKind::MethodCall(_, _, arguments, span) => (arguments, span),
        ExprKind::Call(_, arguments) => (arguments, expression.span),
        _ => return,
    };
    let Some(callee) = fn_def_id(cx, expression) else {
        return;
    };
    if !is_iterator_method(cx.tcx, callee) {
        return;
    }
    let Some(message) = naive_reduction(cx, callee, expression, arguments) else {
        return;
    };
    if !is_test_code(cx.tcx, expression.hir_id) {
        span_lint(cx, lint, span, message);
    }
}

/// True if `callee` is a method of `Iterator` or of `DoubleEndedIterator`.
fn is_iterator_method(tcx: TyCtxt<'_>, callee: DefId) -> bool {
    let Some(owner) = tcx.trait_of_assoc(callee) else {
        return false;
    };
    let iterator = tcx.get_diagnostic_item(sym::Iterator);
    let double_ended = tcx.get_diagnostic_item(Symbol::intern(DOUBLE_ENDED_ITERATOR));
    iterator == Some(owner) || double_ended == Some(owner)
}

/// The message for `expression`, a call of the iterator method `callee`, if it is a naive float
/// reduction. `arguments` are the call's arguments; the function of a fold is the last one.
fn naive_reduction<'tcx>(
    cx: &LateContext<'tcx>,
    callee: DefId,
    expression: &'tcx Expr<'tcx>,
    arguments: &'tcx [Expr<'tcx>],
) -> Option<&'static str> {
    let typeck = cx.typeck_results();
    let result = typeck.expr_ty(expression);
    let float_result = || is_float_or_fallible_float(cx.tcx, result);
    // The names are compared as text: the compiler has no predefined symbols for them.
    let (is_naive, message) = match cx.tcx.item_name(callee).as_str() {
        "sum" => (float_result(), SUM_MESSAGE),
        "product" => (float_result(), PRODUCT_MESSAGE),
        "fold" | "rfold" => (result.is_floating_point() && combines(cx, arguments), SUM_MESSAGE),
        "reduce" => (float_result() && combines(cx, arguments), SUM_MESSAGE),
        "try_fold" | "try_rfold" => {
            // The initial value comes just before the function, in both call forms.
            let initial = arguments.len().checked_sub(2).and_then(|index| arguments.get(index));
            let float_initial =
                initial.is_some_and(|value| typeck.expr_ty(value).is_floating_point());
            (float_initial && combines(cx, arguments), SUM_MESSAGE)
        }
        _ => (false, SUM_MESSAGE),
    };
    is_naive.then_some(message)
}

/// True if `ty` is a float, or an `Option` or `Result` whose value is a float.
fn is_float_or_fallible_float<'tcx>(tcx: TyCtxt<'tcx>, ty: Ty<'tcx>) -> bool {
    if ty.is_floating_point() {
        return true;
    }
    let ty::Adt(definition, generic_arguments) = ty.kind() else {
        return false;
    };
    let is_fallible =
        matches!(tcx.get_diagnostic_name(definition.did()), Some(sym::Option | sym::Result));
    is_fallible && generic_arguments.type_at(0).is_floating_point()
}

/// True if the last of `arguments`, the function of a fold, adds to or subtracts from the
/// accumulator: a closure that does so with its first parameter, or a path to `Add` or `Sub`.
fn combines<'tcx>(cx: &LateContext<'tcx>, arguments: &'tcx [Expr<'tcx>]) -> bool {
    let Some(function) = arguments.last() else {
        return false;
    };
    match function.kind {
        ExprKind::Closure(closure) => adds_to_accumulator(cx.tcx, closure.body),
        ExprKind::Path(ref path) => is_add_or_sub_method(cx, path, function.hir_id),
        _ => false,
    }
}

/// True if `path` names the method of `Add` or `Sub`, in the trait or in an implementation.
fn is_add_or_sub_method(cx: &LateContext<'_>, path: &QPath<'_>, id: HirId) -> bool {
    let tcx = cx.tcx;
    let Res::Def(DefKind::AssocFn, method) = cx.qpath_res(path, id) else {
        return false;
    };
    let owner = tcx
        .trait_of_assoc(method)
        .or_else(|| tcx.trait_impl_of_assoc(method).map(|block| tcx.impl_trait_id(block)));
    let lang_items = tcx.lang_items();
    owner.is_some() && (owner == lang_items.add_trait() || owner == lang_items.sub_trait())
}

/// True if the closure with body `body` adds to or subtracts from its first parameter, the
/// accumulator.
fn adds_to_accumulator(tcx: TyCtxt<'_>, body: BodyId) -> bool {
    let body = tcx.hir_body(body);
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

/// Looks for `accumulator + _`, `_ + accumulator`, `accumulator += _`, and the same with `-`.
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
            ExprKind::Binary(operator, left, right)
                if matches!(operator.node, BinOpKind::Add | BinOpKind::Sub) =>
            {
                self.is_accumulator(left) || self.is_accumulator(right)
            }
            ExprKind::AssignOp(operator, target, _)
                if matches!(operator.node, AssignOpKind::AddAssign | AssignOpKind::SubAssign) =>
            {
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
