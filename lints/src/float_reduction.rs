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
//! The function adds to or subtracts from the accumulator if it is a path to the method of
//! `Add` or `Sub` (`f64::add`, `core::ops::Sub::sub`), or a closure whose body contains one of
//! these with its first parameter, the accumulator:
//!
//! - a `+` or `-` with the accumulator as an operand, or a `+=` or `-=` on it;
//! - a call of `Add::add` or `Sub::sub` with the accumulator as an operand, in any spelling
//!   (`total.add(value)`, `f64::sub(total, value)`);
//! - a `mul_add` with the accumulator as the addend (`a.mul_add(b, total)`).
//!
//! Every expression of the body counts, discarded statements such as `{ total + value; value }`
//! too, except where a maximum, minimum, argmin or argmax compares values: the condition of an
//! `if` or `while` (an `if let` initialiser still counts), a match guard, and the operands of a
//! comparison or a logical operator. A local's initialiser counts where the local is named
//! outside those positions, or at its `let` if it is never named. So the `-` in an argmin's
//! `if value - best < 0.0 { value } else { best }` does not count (see `ResultFlow`). A
//! closure with more than 10 000 expressions to trace is reported as too complex.
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
//!   variable before the addition, a call of a helper function that adds, and a fold that
//!   multiplies (`fold(1.0, f64::mul)`) are not seen.
//! - A sum used only in a condition, a guard or a comparison is not seen, by design: that is
//!   where an argmin compares. `if total + value > limit { total } else { value }` passes.
//! - `Sum::sum` and `Product::product` called directly, and a `sum` whose result type is a
//!   generic parameter, are not seen.
//! - An integration test is recognised by its path, `tests/` below the package root. A test
//!   target that the manifest places elsewhere is linted like library code.

use clippy_utils::diagnostics::span_lint;
use clippy_utils::{fn_def_id, is_in_test};
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{
    Arm, AssignOpKind, BinOpKind, BodyId, Expr, ExprKind, HirId, LetStmt, PatKind, QPath,
};
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
                               product yet; say in the PR description that you need one, and \
                               the maintainer decides (ADR-0005)";

const TOO_COMPLEX_MESSAGE: &str = "fold closure too complex for `nami_float_reduction` to check \
                                   that it is no naive float sum (over 10 000 expressions); \
                                   simplify the closure (ADR-0005)";

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
    let is_float_fold = match cx.tcx.item_name(callee).as_str() {
        "sum" => return float_result().then_some(SUM_MESSAGE),
        "product" => return float_result().then_some(PRODUCT_MESSAGE),
        "fold" | "rfold" => result.is_floating_point(),
        "reduce" => float_result(),
        "try_fold" | "try_rfold" => {
            // The initial value comes just before the function, in both call forms.
            let initial = arguments.len().checked_sub(2).and_then(|index| arguments.get(index));
            initial.is_some_and(|value| typeck.expr_ty(value).is_floating_point())
        }
        _ => false,
    };
    if !is_float_fold {
        return None;
    }
    match combines(cx, arguments) {
        Verdict::Silent => None,
        Verdict::Sum => Some(SUM_MESSAGE),
        Verdict::TooComplex => Some(TOO_COMPLEX_MESSAGE),
    }
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

/// What the function of a fold does with the accumulator.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// No sum or difference with the accumulator was found.
    Silent,
    /// The function adds to or subtracts from the accumulator.
    Sum,
    /// The closure has more than `TRACED_EXPRESSIONS_MAX` expressions to trace.
    TooComplex,
}

/// Whether the last of `arguments`, the function of a fold, adds to or subtracts from the
/// accumulator: a closure that does so with its first parameter, or a path to `Add` or `Sub`.
fn combines<'tcx>(cx: &LateContext<'tcx>, arguments: &'tcx [Expr<'tcx>]) -> Verdict {
    let Some(function) = arguments.last() else {
        return Verdict::Silent;
    };
    match function.kind {
        ExprKind::Closure(closure) => trace_closure(cx, closure.body),
        ExprKind::Path(ref path) => match cx.qpath_res(path, function.hir_id) {
            Res::Def(DefKind::AssocFn, method) if is_add_or_sub_method(cx.tcx, method) => {
                Verdict::Sum
            }
            _ => Verdict::Silent,
        },
        _ => Verdict::Silent,
    }
}

/// True if `method` is the method of `Add` or `Sub`, in the trait or in an implementation.
fn is_add_or_sub_method(tcx: TyCtxt<'_>, method: DefId) -> bool {
    let owner = tcx
        .trait_of_assoc(method)
        .or_else(|| tcx.trait_impl_of_assoc(method).map(|block| tcx.impl_trait_id(block)));
    let lang_items = tcx.lang_items();
    owner.is_some() && (owner == lang_items.add_trait() || owner == lang_items.sub_trait())
}

/// True if `method` is the inherent `mul_add` of `f32` or `f64`.
fn is_float_mul_add(tcx: TyCtxt<'_>, method: DefId) -> bool {
    // The name is compared as text: the compiler has no predefined symbol for it.
    tcx.item_name(method).as_str() == "mul_add"
        && tcx
            .impl_of_assoc(method)
            .is_some_and(|block| tcx.type_of(block).skip_binder().is_floating_point())
}

/// Whether the closure with body `body` adds to or subtracts from its first parameter, the
/// accumulator.
fn trace_closure<'tcx>(cx: &LateContext<'tcx>, body: BodyId) -> Verdict {
    let body = cx.tcx.hir_body(body);
    let Some(accumulator) = body.params.first() else {
        return Verdict::Silent;
    };
    let mut accumulators = FxHashSet::default();
    accumulator.pat.walk_always(|pattern| {
        if let PatKind::Binding(_, binding, _, _) = pattern.kind {
            let _new = accumulators.insert(binding);
        }
    });
    let mut locals = LocalFinder { locals: FxHashMap::default(), named: FxHashSet::default() };
    locals.visit_expr(body.value);
    let mut flow = ResultFlow {
        cx,
        accumulators,
        locals: locals.locals,
        named: locals.named,
        expanded: FxHashSet::default(),
        expressions: 0,
        verdict: Verdict::Silent,
    };
    flow.visit_expr(body.value);
    flow.verdict
}

/// Upper bound on the expressions traced in one closure. Each initialiser is traced at most
/// once, so a closure written by hand stays far below it; past it the closure is reported as
/// too complex, never passed.
const TRACED_EXPRESSIONS_MAX: u32 = 10_000;

/// Walks a fold closure's body and looks for a sum or difference with the accumulator: a `+`
/// or `-` with it as an operand, a `+=` or `-=` on it, a call of `Add::add` or `Sub::sub` with
/// it as an operand, or a `mul_add` with it as the addend.
///
/// Every expression is traced, including discarded statements, except in the positions where
/// a maximum, minimum, argmin or argmax compares values: the condition of an `if` or `while`
/// (but the initialiser of an `if let` or `while let` is traced), a match guard, and the
/// operands of a comparison or a logical operator, which yield a `bool`, never the accumulator.
/// A local's initialiser is traced where the local is named outside those positions, or at the
/// `let` if none of its bindings is named anywhere.
struct ResultFlow<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    /// The bindings of the closure's first parameter.
    accumulators: FxHashSet<HirId>,
    /// Every binding of a `let` with an initialiser in the closure, and that initialiser.
    locals: FxHashMap<HirId, &'tcx Expr<'tcx>>,
    /// The locals named anywhere in the closure.
    named: FxHashSet<HirId>,
    /// The initialisers traced already.
    expanded: FxHashSet<HirId>,
    expressions: u32,
    verdict: Verdict,
}

impl<'tcx> ResultFlow<'_, 'tcx> {
    /// True if `expression` itself adds to or subtracts from the accumulator.
    fn combines_at(&self, expression: &'tcx Expr<'tcx>) -> bool {
        match expression.kind {
            ExprKind::Binary(operator, left, right) => {
                matches!(operator.node, BinOpKind::Add | BinOpKind::Sub)
                    && (self.is_accumulator(left) || self.is_accumulator(right))
            }
            ExprKind::AssignOp(operator, target, _) => {
                matches!(operator.node, AssignOpKind::AddAssign | AssignOpKind::SubAssign)
                    && self.is_accumulator(target)
            }
            ExprKind::Call(function, arguments) => {
                let method = match function.kind {
                    ExprKind::Path(ref path) => callee_def_id(self.cx, path, function.hir_id),
                    _ => None,
                };
                self.method_combines(method, arguments.iter())
            }
            ExprKind::MethodCall(_, receiver, arguments, _) => {
                let method = self.cx.typeck_results().type_dependent_def_id(expression.hir_id);
                self.method_combines(method, std::iter::once(receiver).chain(arguments))
            }
            _ => false,
        }
    }

    /// True if a call of `method` with `operands`, the receiver first, adds to or subtracts
    /// from the accumulator: `Add::add` or `Sub::sub` with the accumulator as an operand, or
    /// `mul_add` with the accumulator as the addend.
    fn method_combines(
        &self,
        method: Option<DefId>,
        mut operands: impl Iterator<Item = &'tcx Expr<'tcx>>,
    ) -> bool {
        let Some(method) = method else {
            return false;
        };
        let tcx = self.cx.tcx;
        if is_add_or_sub_method(tcx, method) {
            return operands.take(2).any(|operand| self.is_accumulator(operand));
        }
        is_float_mul_add(tcx, method)
            && operands.nth(2).is_some_and(|addend| self.is_accumulator(addend))
    }

    fn is_accumulator(&self, operand: &Expr<'_>) -> bool {
        let operand = operand.peel_drop_temps().peel_borrows();
        matches!(
            operand.kind,
            ExprKind::Path(QPath::Resolved(None, path))
                if matches!(path.res, Res::Local(local) if self.accumulators.contains(&local))
        )
    }

    /// Traces the initialiser of `local`, once, if it is a `let` binding of the closure.
    fn expand(&mut self, local: HirId) {
        if let Some(&initialiser) = self.locals.get(&local)
            && self.expanded.insert(initialiser.hir_id)
        {
            self.visit_expr(initialiser);
        }
    }

    /// Traces the initialisers of the `let`s in `condition`, an `if` or `while` condition, and
    /// nothing else in it.
    fn visit_condition(&mut self, condition: &'tcx Expr<'tcx>) {
        let mut pending = vec![condition];
        for _ in 0..TRACED_EXPRESSIONS_MAX {
            let Some(part) = pending.pop() else {
                return;
            };
            match part.kind {
                ExprKind::DropTemps(inner) => pending.push(inner),
                ExprKind::Binary(operator, left, right) if operator.node == BinOpKind::And => {
                    pending.extend([left, right]);
                }
                ExprKind::Let(binding) => self.visit_expr(binding.init),
                _ => {}
            }
        }
        self.verdict = Verdict::TooComplex;
    }
}

/// True if `operator` yields a `bool` from its operands: a comparison or a logical operator.
fn is_condition_operator(operator: BinOpKind) -> bool {
    matches!(
        operator,
        BinOpKind::Eq
            | BinOpKind::Ne
            | BinOpKind::Lt
            | BinOpKind::Le
            | BinOpKind::Gt
            | BinOpKind::Ge
            | BinOpKind::And
            | BinOpKind::Or
    )
}

impl<'tcx> Visitor<'tcx> for ResultFlow<'_, 'tcx> {
    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        if self.verdict != Verdict::Silent {
            return;
        }
        self.expressions += 1;
        if self.expressions > TRACED_EXPRESSIONS_MAX {
            self.verdict = Verdict::TooComplex;
            return;
        }
        if self.combines_at(expression) {
            self.verdict = Verdict::Sum;
            return;
        }
        match expression.kind {
            ExprKind::Binary(operator, _, _) if is_condition_operator(operator.node) => {}
            ExprKind::If(condition, then, otherwise) => {
                self.visit_condition(condition);
                self.visit_expr(then);
                if let Some(otherwise) = otherwise {
                    self.visit_expr(otherwise);
                }
            }
            ExprKind::Path(QPath::Resolved(None, path)) => {
                if let Res::Local(local) = path.res {
                    self.expand(local);
                }
            }
            _ => intravisit::walk_expr(self, expression),
        }
    }

    /// Traces an arm's body, not its guard.
    fn visit_arm(&mut self, arm: &'tcx Arm<'tcx>) {
        self.visit_expr(arm.body);
    }

    /// Traces a `let`'s initialiser here only if none of its bindings is named; otherwise it
    /// is traced where a binding is named. A `let ... else` block is always traced.
    fn visit_local(&mut self, local: &'tcx LetStmt<'tcx>) {
        let mut is_named = false;
        local.pat.each_binding(|_, binding, _, _| is_named |= self.named.contains(&binding));
        if let Some(initialiser) = local.init
            && !is_named
            && self.expanded.insert(initialiser.hir_id)
        {
            self.visit_expr(initialiser);
        }
        if let Some(otherwise) = local.els {
            self.visit_block(otherwise);
        }
    }
}

/// The function that `path`, the callee of a call, resolves to.
fn callee_def_id(cx: &LateContext<'_>, path: &QPath<'_>, id: HirId) -> Option<DefId> {
    match cx.qpath_res(path, id) {
        Res::Def(DefKind::AssocFn | DefKind::Fn, function) => Some(function),
        _ => None,
    }
}

/// Collects every binding of a `let` with an initialiser, and every local that is named.
struct LocalFinder<'tcx> {
    locals: FxHashMap<HirId, &'tcx Expr<'tcx>>,
    named: FxHashSet<HirId>,
}

impl<'tcx> Visitor<'tcx> for LocalFinder<'tcx> {
    fn visit_local(&mut self, local: &'tcx LetStmt<'tcx>) {
        if let Some(initialiser) = local.init {
            local.pat.each_binding(|_, binding, _, _| {
                let _new = self.locals.insert(binding, initialiser);
            });
        }
        intravisit::walk_local(self, local);
    }

    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        if let ExprKind::Path(QPath::Resolved(None, path)) = expression.kind
            && let Res::Local(local) = path.res
        {
            let _new = self.named.insert(local);
        }
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
