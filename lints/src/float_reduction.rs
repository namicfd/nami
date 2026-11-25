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
//! `Add` or `Sub` (`f64::add`, `core::ops::Sub::sub`), or a closure that the operand rule
//! reports and the selection exemption does not cover.
//!
//! **Operand rule.** The closure is reported if anywhere in its body, closures nested in it
//! included, its first parameter, the accumulator, is an operand of a sum or difference:
//!
//! - a `+` or `-` with the accumulator as an operand;
//! - a `+=` or `-=` with the accumulator as the target or the value (`value += total`);
//! - a call of `Add::add`, `Sub::sub`, `AddAssign::add_assign` or `SubAssign::sub_assign` with
//!   the accumulator as one of the two operands, in any spelling (`total.add(value)`,
//!   `f64::sub(total, value)`, `value.add_assign(total)`);
//! - a `mul_add` with the accumulator as the addend (`a.mul_add(b, total)`).
//!
//! **Selection exemption, for differences only.** An argmin or argmax compares a difference
//! with the accumulator (`if value - best < 0.0 { value } else { best }`). A closure that the
//! operand rule reports is silent only if all of these hold (see `is_exempt_selection`):
//!
//! - every sum the operand rule found is a difference: `-`, `-=`, `sub` or `sub_assign`. An
//!   addition (`+`, `+=`, `add`, `add_assign`, `mul_add`) is always reported, so everything
//!   the base lint reported (only additions) is still reported;
//! - every such difference is only compared: it is a direct operand of `==`, `!=`, `<`, `<=`,
//!   `>` or `>=`, or it initialises a `let` with one binding that is read, and read only as
//!   such an operand;
//! - every value the closure can return is a path to a binding of one of its parameters, a
//!   field of one, or a tuple, struct or enum variant (`Some(best)`) built only from those. The
//!   values it can return are its body, every `return` and `break` value, and from each of
//!   these the tail of a block and every branch of an `if` or `match`, up to 10 000 of them;
//! - the closure changes nothing except by `parameter = parameter` (`best = value`): its body
//!   contains no nested closure, no compound assignment, no other assignment, no `&mut` borrow
//!   (written, or added by autoref as in `value.sub_assign(total)`), and no call of a function
//!   or method that takes a `&mut` argument or receiver.
//!
//! So `if value - best < 0.0 { value } else { best }` is silent, and
//! `if total + value > limit { total } else { value }` or `{ total - value; value }` is
//! reported.
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
//! - The selection exemption, by design: a closure that only compares differences with the
//!   accumulator and returns only its parameters, without changing them, passes. It takes the
//!   difference only to choose a parameter, so it carries no sum into the accumulator.
//! - `Sum::sum` and `Product::product` called directly, and a `sum` whose result type is a
//!   generic parameter, are not seen.
//! - An integration test is recognised by its path, `tests/` below the package root. A test
//!   target that the manifest places elsewhere is linted like library code.

use clippy_utils::diagnostics::span_lint;
use clippy_utils::{fn_def_id, is_in_test};
use rustc_data_structures::fx::FxHashSet;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{
    AssignOpKind, BinOpKind, Body, Expr, ExprKind, HirId, Mutability, Node, Pat, PatKind, QPath,
};
use rustc_lint::{LateContext, Lint};
use rustc_middle::ty::adjustment::{
    Adjust, AutoBorrow, AutoBorrowMutability, DerefAdjustKind, OverloadedDeref,
};
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
    combines(cx, arguments).then_some(SUM_MESSAGE)
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
/// accumulator: a path to `Add::add` or `Sub::sub`, or a closure that the operand rule reports
/// and that the selection exemption does not cover.
fn combines<'tcx>(cx: &LateContext<'tcx>, arguments: &'tcx [Expr<'tcx>]) -> bool {
    let Some(function) = arguments.last() else {
        return false;
    };
    match function.kind {
        ExprKind::Closure(closure) => {
            let body = cx.tcx.hir_body(closure.body);
            let sums = sums_with_accumulator(cx, body);
            !sums.is_empty() && !is_exempt_selection(cx, body, &sums)
        }
        ExprKind::Path(ref path) => match cx.qpath_res(path, function.hir_id) {
            Res::Def(DefKind::AssocFn, method) => {
                matches!(arithmetic_method(cx.tcx, method), Some(Arithmetic::Add | Arithmetic::Sub))
            }
            _ => false,
        },
        _ => false,
    }
}

/// The method of an arithmetic trait that a call can resolve to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arithmetic {
    Add,
    Sub,
    AddAssign,
    SubAssign,
}

/// Which arithmetic trait `method` is the method of, in the trait or in an implementation.
fn arithmetic_method(tcx: TyCtxt<'_>, method: DefId) -> Option<Arithmetic> {
    let owner = tcx
        .trait_of_assoc(method)
        .or_else(|| tcx.trait_impl_of_assoc(method).map(|block| tcx.impl_trait_id(block)))?;
    let lang_items = tcx.lang_items();
    let traits = [
        (lang_items.add_trait(), Arithmetic::Add),
        (lang_items.sub_trait(), Arithmetic::Sub),
        (lang_items.add_assign_trait(), Arithmetic::AddAssign),
        (lang_items.sub_assign_trait(), Arithmetic::SubAssign),
    ];
    traits.into_iter().find(|(trait_id, _)| *trait_id == Some(owner)).map(|(_, kind)| kind)
}

/// True if `method` is the inherent `mul_add` of `f32` or `f64`.
fn is_float_mul_add(tcx: TyCtxt<'_>, method: DefId) -> bool {
    // The name is compared as text: the compiler has no predefined symbol for it.
    tcx.item_name(method).as_str() == "mul_add"
        && tcx
            .impl_of_assoc(method)
            .is_some_and(|block| tcx.type_of(block).skip_binder().is_floating_point())
}

/// The bindings of `pattern`.
fn bindings(pattern: &Pat<'_>) -> FxHashSet<HirId> {
    let mut bindings = FxHashSet::default();
    pattern.each_binding(|_, binding, _, _| {
        let _new = bindings.insert(binding);
    });
    bindings
}

/// True if `expression`, without borrows, is a path to one of `locals`.
fn is_path_to(locals: &FxHashSet<HirId>, expression: &Expr<'_>) -> bool {
    let expression = expression.peel_drop_temps().peel_borrows();
    matches!(
        expression.kind,
        ExprKind::Path(QPath::Resolved(None, path))
            if matches!(path.res, Res::Local(local) if locals.contains(&local))
    )
}

/// Whether a sum that the operand rule found adds or subtracts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combination {
    /// `+`, `+=`, `add`, `add_assign` or `mul_add`: always reported, as the base lint did.
    Addition,
    /// `-`, `-=`, `sub` or `sub_assign`: reported unless the selection exemption applies.
    Difference,
}

/// A sum or difference with the accumulator as an operand.
struct Found<'tcx> {
    expression: &'tcx Expr<'tcx>,
    combination: Combination,
}

/// The operand rule: every sum or difference in `body`, closures nested in it included, that
/// has the accumulator (a binding of the first parameter) as an operand.
fn sums_with_accumulator<'tcx>(cx: &LateContext<'tcx>, body: &'tcx Body<'tcx>) -> Vec<Found<'tcx>> {
    let Some(accumulator) = body.params.first() else {
        return Vec::new();
    };
    let accumulators = bindings(accumulator.pat);
    assert!(
        accumulators.iter().all(|binding| binding.owner == body.value.hir_id.owner),
        "the accumulator is a binding of the fold's closure"
    );
    let mut finder = AdditionFinder { cx, accumulators, sums: Vec::new() };
    finder.visit_expr(body.value);
    assert!(
        finder.sums.iter().all(|sum| sum.expression.hir_id.owner == body.value.hir_id.owner),
        "every sum found lies in the body of the fold's closure"
    );
    finder.sums
}

/// Looks for the accumulator as an operand of a sum or difference:
///
/// - `accumulator ± _`, `_ ± accumulator`;
/// - `accumulator ±= _` and `_ ±= accumulator`;
/// - `Add::add`, `Sub::sub`, `AddAssign::add_assign` or `SubAssign::sub_assign` with the
///   accumulator as one of the two operands, in any spelling;
/// - `mul_add` with the accumulator as the addend.
struct AdditionFinder<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    /// The bindings of the closure's first parameter.
    accumulators: FxHashSet<HirId>,
    sums: Vec<Found<'tcx>>,
}

impl<'tcx> AdditionFinder<'_, 'tcx> {
    fn is_accumulator(&self, operand: &Expr<'_>) -> bool {
        is_path_to(&self.accumulators, operand)
    }

    /// Whether `expression` adds or subtracts with the accumulator as an operand.
    fn combination(&self, expression: &'tcx Expr<'tcx>) -> Option<Combination> {
        let (combination, left, right) = match expression.kind {
            ExprKind::Binary(operator, left, right) => match operator.node {
                BinOpKind::Add => (Combination::Addition, left, right),
                BinOpKind::Sub => (Combination::Difference, left, right),
                _ => return None,
            },
            ExprKind::AssignOp(operator, target, value) => match operator.node {
                AssignOpKind::AddAssign => (Combination::Addition, target, value),
                AssignOpKind::SubAssign => (Combination::Difference, target, value),
                _ => return None,
            },
            ExprKind::Call(function, arguments) => {
                let method = match function.kind {
                    ExprKind::Path(ref path) => callee_def_id(self.cx, path, function.hir_id),
                    _ => None,
                };
                return self.call_combination(method?, arguments.iter().collect());
            }
            ExprKind::MethodCall(_, receiver, arguments, _) => {
                let method = self.cx.typeck_results().type_dependent_def_id(expression.hir_id);
                let operands = std::iter::once(receiver).chain(arguments).collect();
                return self.call_combination(method?, operands);
            }
            _ => return None,
        };
        (self.is_accumulator(left) || self.is_accumulator(right)).then_some(combination)
    }

    /// Whether a call of `method` with `operands`, the receiver first, adds or subtracts with
    /// the accumulator as an operand.
    fn call_combination(
        &self,
        method: DefId,
        operands: Vec<&'tcx Expr<'tcx>>,
    ) -> Option<Combination> {
        let tcx = self.cx.tcx;
        let has_accumulator =
            |count: usize| operands.iter().take(count).any(|operand| self.is_accumulator(operand));
        match arithmetic_method(tcx, method) {
            Some(Arithmetic::Add | Arithmetic::AddAssign) => {
                has_accumulator(2).then_some(Combination::Addition)
            }
            Some(Arithmetic::Sub | Arithmetic::SubAssign) => {
                has_accumulator(2).then_some(Combination::Difference)
            }
            None if is_float_mul_add(tcx, method) => operands
                .get(2)
                .is_some_and(|addend| self.is_accumulator(addend))
                .then_some(Combination::Addition),
            None => None,
        }
    }
}

impl<'tcx> Visitor<'tcx> for AdditionFinder<'_, 'tcx> {
    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        if let Some(combination) = self.combination(expression) {
            self.sums.push(Found { expression, combination });
        }
        if let ExprKind::Closure(closure) = expression.kind {
            // A nested closure shares the typeck results of the fold's closure.
            self.visit_expr(self.cx.tcx.hir_body(closure.body).value);
        } else {
            intravisit::walk_expr(self, expression);
        }
    }
}

/// The selection exemption, for argmin and argmax folds such as
/// `if value - best < 0.0 { value } else { best }`. True if every one of `sums`, the sums the
/// operand rule found in `body`, is a difference that is only compared, and the closure only
/// selects one of its inputs (`is_selection`). An addition is never exempt.
fn is_exempt_selection<'tcx>(
    cx: &LateContext<'tcx>,
    body: &'tcx Body<'tcx>,
    sums: &[Found<'tcx>],
) -> bool {
    assert!(!sums.is_empty(), "the exemption applies only to a closure the operand rule reports");
    sums.iter().all(|sum| {
        sum.combination == Combination::Difference && is_compared_sum(cx.tcx, body, sum.expression)
    }) && is_selection(cx, body)
}

/// Upper bound on the return positions that `is_selection` checks. A closure past it is not a
/// selection, so it is reported.
const SELECTION_EXPRESSIONS_MAX: u32 = 10_000;

/// True if the closure with body `body` only selects one of its inputs, as a maximum,
/// minimum, argmin or argmax does.
///
/// Every value the closure can return must be a selection: a path to a binding of one of its
/// parameters, a field of a selection, or a tuple, struct or enum variant built only from
/// selections. The values the closure can return are its body, every `return` value and every
/// `break` value in it, and, from each of these, the tail of a block and every branch of an
/// `if` or a `match`. And the body must change nothing except by assigning a parameter binding
/// to a parameter binding (`best = value`): it contains no nested closure, no compound
/// assignment, no other assignment, no `&mut` borrow (written or added by autoref), and no
/// call of a function or method that takes a `&mut` argument.
fn is_selection<'tcx>(cx: &LateContext<'tcx>, body: &'tcx Body<'tcx>) -> bool {
    assert!(!body.params.is_empty(), "the function of a fold takes at least the accumulator");
    let mut parameters = FxHashSet::default();
    for parameter in body.params {
        parameters.extend(bindings(parameter.pat));
    }
    let mut exits = ExitFinder { cx, parameters: &parameters, exits: Vec::new(), clean: true };
    exits.visit_expr(body.value);
    if !exits.clean {
        return false;
    }
    let mut pending = exits.exits;
    pending.push(body.value);
    for _ in 0..SELECTION_EXPRESSIONS_MAX {
        let Some(value) = pending.pop() else {
            return true;
        };
        match value.kind {
            ExprKind::DropTemps(inner) | ExprKind::Field(inner, _) => pending.push(inner),
            ExprKind::Block(block, _) => pending.extend(block.expr),
            ExprKind::If(_, then, otherwise) => {
                pending.push(then);
                pending.extend(otherwise);
            }
            ExprKind::Match(_, arms, _) => pending.extend(arms.iter().map(|arm| arm.body)),
            ExprKind::Tup(elements) => pending.extend(elements),
            ExprKind::Struct(_, fields, _) => pending.extend(fields.iter().map(|field| field.expr)),
            ExprKind::Call(function, arguments) if is_constructor(cx, function) => {
                pending.extend(arguments);
            }
            // A `return`, a `break` and a loop diverge or take their value from a `return` or
            // `break`, which are checked as exits.
            ExprKind::Ret(_) | ExprKind::Break(..) | ExprKind::Loop(..) => {}
            _ if is_path_to(&parameters, value) => {}
            _ => return false,
        }
    }
    false
}

/// True if `function`, the callee of a call, is the constructor of a tuple struct or of an
/// enum variant, such as `Some`.
fn is_constructor(cx: &LateContext<'_>, function: &Expr<'_>) -> bool {
    matches!(
        function.kind,
        ExprKind::Path(ref path)
            if matches!(cx.qpath_res(path, function.hir_id), Res::Def(DefKind::Ctor(..), _))
    )
}

/// True if `function` takes a `&mut` argument (or receiver), or one containing a `&mut`.
fn takes_mutable_reference(tcx: TyCtxt<'_>, function: DefId) -> bool {
    let signature = tcx.fn_sig(function).skip_binder().skip_binder();
    signature.inputs().iter().any(|input| {
        input.walk().any(|part| part.as_type().is_some_and(|part| part.is_mutable_ptr()))
    })
}

/// True if typeck adds a mutable borrow to `expression`, as autoref does for
/// `value.sub_assign(total)`.
fn is_mutably_adjusted(cx: &LateContext<'_>, expression: &Expr<'_>) -> bool {
    cx.typeck_results().expr_adjustments(expression).iter().any(|adjustment| {
        matches!(
            adjustment.kind,
            Adjust::Borrow(AutoBorrow::Ref(AutoBorrowMutability::Mut { .. }))
                | Adjust::Borrow(AutoBorrow::RawPtr(Mutability::Mut))
                | Adjust::Borrow(AutoBorrow::Pin(Mutability::Mut))
                | Adjust::Deref(DerefAdjustKind::Overloaded(OverloadedDeref {
                    mutbl: Mutability::Mut,
                    ..
                }))
        )
    })
}

/// Collects the `return` and `break` values of a closure, and finds what a selection may not
/// contain: anything that can change a value other than `parameter = parameter`.
struct ExitFinder<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    parameters: &'a FxHashSet<HirId>,
    exits: Vec<&'tcx Expr<'tcx>>,
    /// False once a nested closure, a compound assignment, an assignment other than of a
    /// parameter binding to a parameter binding, a `&mut` borrow, or a call that takes a
    /// `&mut` argument was found.
    clean: bool,
}

impl<'tcx> ExitFinder<'_, 'tcx> {
    /// True if `expression` may change a value in a way a selection may not.
    fn mutates(&self, expression: &'tcx Expr<'tcx>) -> bool {
        let cx = self.cx;
        let changes = match expression.kind {
            ExprKind::AssignOp(..)
            | ExprKind::Closure(..)
            | ExprKind::AddrOf(_, Mutability::Mut, _) => true,
            ExprKind::Assign(target, value, _) => {
                !(is_path_to(self.parameters, target) && is_path_to(self.parameters, value))
            }
            ExprKind::Call(function, _) => match function.kind {
                ExprKind::Path(ref path) => match cx.qpath_res(path, function.hir_id) {
                    Res::Def(DefKind::Ctor(..), _) => false,
                    Res::Def(DefKind::AssocFn | DefKind::Fn, callee) => {
                        takes_mutable_reference(cx.tcx, callee)
                    }
                    // A call of a local closure or of anything else unresolved may change
                    // anything.
                    _ => true,
                },
                _ => true,
            },
            ExprKind::MethodCall(..) => cx
                .typeck_results()
                .type_dependent_def_id(expression.hir_id)
                .is_none_or(|method| takes_mutable_reference(cx.tcx, method)),
            _ => false,
        };
        changes || is_mutably_adjusted(cx, expression)
    }
}

impl<'tcx> Visitor<'tcx> for ExitFinder<'_, 'tcx> {
    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        if let ExprKind::Ret(Some(value)) | ExprKind::Break(_, Some(value)) = expression.kind {
            self.exits.push(value);
        }
        if self.mutates(expression) {
            self.clean = false;
        }
        // A nested closure is not entered: it already rules the selection out.
        if !matches!(expression.kind, ExprKind::Closure(..)) {
            intravisit::walk_expr(self, expression);
        }
    }
}

/// True if `operator` compares its operands.
fn is_comparison(operator: BinOpKind) -> bool {
    matches!(
        operator,
        BinOpKind::Eq
            | BinOpKind::Ne
            | BinOpKind::Lt
            | BinOpKind::Le
            | BinOpKind::Gt
            | BinOpKind::Ge
    )
}

/// True if `expression` is a direct operand of a comparison.
fn is_compared(tcx: TyCtxt<'_>, expression: &Expr<'_>) -> bool {
    match tcx.parent_hir_node(expression.hir_id) {
        Node::Expr(parent) => match parent.kind {
            ExprKind::Binary(operator, left, right) => {
                assert!(
                    left.hir_id == expression.hir_id || right.hir_id == expression.hir_id,
                    "an expression whose parent is a binary operation is one of its operands"
                );
                is_comparison(operator.node)
            }
            _ => false,
        },
        _ => false,
    }
}

/// True if `sum`, a sum with the accumulator in `body`, is only compared: a direct operand of
/// a comparison, or the initialiser of a `let` with a single binding that is read at least
/// once and only as a direct operand of a comparison (`let gain = value - best; if gain > ..`).
fn is_compared_sum<'tcx>(tcx: TyCtxt<'tcx>, body: &'tcx Body<'tcx>, sum: &Expr<'_>) -> bool {
    assert!(
        sum.hir_id.owner == body.value.hir_id.owner,
        "the sum lies in the body of the fold's closure"
    );
    if is_compared(tcx, sum) {
        return true;
    }
    let Node::LetStmt(local) = tcx.parent_hir_node(sum.hir_id) else {
        return false;
    };
    assert!(
        local.init.is_some_and(|initialiser| initialiser.hir_id == sum.hir_id),
        "an expression whose parent is a `let` is its initialiser"
    );
    let PatKind::Binding(_, binding, _, None) = local.pat.kind else {
        return false;
    };
    let mut reads = ReadFinder { binding, reads: Vec::new() };
    reads.visit_expr(body.value);
    !reads.reads.is_empty() && reads.reads.iter().all(|read| is_compared(tcx, read))
}

/// Collects the paths that read `binding`.
struct ReadFinder<'tcx> {
    binding: HirId,
    reads: Vec<&'tcx Expr<'tcx>>,
}

impl<'tcx> Visitor<'tcx> for ReadFinder<'tcx> {
    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        if let ExprKind::Path(QPath::Resolved(None, path)) = expression.kind
            && path.res == Res::Local(self.binding)
        {
            self.reads.push(expression);
        }
        intravisit::walk_expr(self, expression);
    }
}

/// The function that `path`, the callee of a call, resolves to.
fn callee_def_id(cx: &LateContext<'_>, path: &QPath<'_>, id: HirId) -> Option<DefId> {
    match cx.qpath_res(path, id) {
        Res::Def(DefKind::AssocFn | DefKind::Fn, function) => Some(function),
        _ => None,
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
