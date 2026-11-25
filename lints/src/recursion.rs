//! `nami_recursion`: no function may reach itself through calls (ADR-0006).
//!
//! The lint works on function instances: a function of this crate together with the generic
//! arguments it is called with. For every function it searches breadth-first from that function,
//! with its own generic parameters as arguments, and reports it if the search comes back to it
//! with any arguments.
//!
//! One step of the search takes a call in the body of the current instance, substitutes the
//! instance's arguments into the generic arguments of the call, and resolves the result:
//!
//! - a free function, an inherent method, or a trait method whose impl is now known: the search
//!   continues in that function, with the resolved arguments;
//! - a trait method that is still too generic to name an impl, or a call through `dyn`: the
//!   search continues in the trait's own declaration. A default body there is taken to be the
//!   one that runs, so a default method that calls itself is found; a method without a body
//!   ends the path;
//! - a function of another crate: the path ends.
//!
//! So `fn through<P: Probe>(p: &P) { p.value() }` has no cycle on its own, but
//! `impl Probe for Cycle` whose `value` calls `through(self)` does: from there `P` is `Cycle`,
//! and `p.value()` resolves to the function the search started from. A generic decorator
//! (`impl<T: Flux> Flux for Wrapper<T>` calling `self.inner.flux()`) is not reported, however
//! deeply it is nested, because for a generic `T` no impl is known.
//!
//! Closures count as part of the function that defines them, whether or not they are called.
//!
//! The search from one function is bounded in instances and in call depth. A function whose
//! search hits a bound is reported too, because it was not shown to be free of recursion. That
//! happens when a callee calls itself with generic arguments that grow with every call.
//!
//! # Known gaps
//!
//! - A cycle that crosses crates through a generic callback is not seen. If a function of this
//!   crate calls a generic function of another crate, which calls back through a trait impl or
//!   a closure of this crate, the path ends at the foreign function: its body is not available
//!   under `cargo check`. Closures still count for the function that defines them.
//! - Calls through `dyn Trait` are followed only into the trait's default methods, and calls
//!   through function pointers and `Fn` bounds are not followed at all. Kernels ban `dyn` and
//!   function pointers (ADR-0003), so this gap is open only in the other crates.
//! - A function that passes itself by name to a generic function (`apply(walk, n)`) is not
//!   reported, because naming a function is not a call.
//! - Only calls written as calls are edges: `f(x)`, `T::f(x)` and `x.f()`. Calls that the
//!   compiler inserts are not: overloaded operators (`a + b`, `a[i]`, `*a`), implicit derefs,
//!   and `Drop`.
//! - A generic function is searched with its own parameters, so it is reported only if its
//!   cycle does not depend on them. If only some instantiation is recursive, the report is at
//!   the function whose call chooses that instantiation, when that function is in this crate.

use clippy_utils::diagnostics::span_lint;
use rustc_data_structures::Limit;
use rustc_data_structures::fx::{FxIndexMap, FxIndexSet};
use rustc_hir::def::{DefKind, Namespace, Res};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Body, Expr, ExprKind};
use rustc_lint::{LateContext, Lint};
use rustc_middle::ty::print::{FmtPrinter, Printer};
use rustc_middle::ty::{
    EarlyBinder, GenericArgs, GenericArgsRef, Instance, InstanceKind, TyCtxt, TypeckResults,
    TypingEnv,
};
use rustc_span::def_id::{DefId, LocalDefId};

/// Upper bound on the instances expanded in the search from one function.
const INSTANCES_PER_ROOT_MAX: usize = 100_000;

/// Upper bound on the calls between the start of a search and any instance it expands. Generic
/// arguments can grow with every call, and the compiler recurses over them when it resolves a
/// call, so this bound also keeps the compiler's own stack shallow.
const CALL_DEPTH_MAX: usize = 500;

/// Upper bound on the types printed for one function in a message. The rest is elided.
const TYPES_PER_NAME_MAX: usize = 32;

/// A call in a function body: the declared callee and the call's generic arguments, written in
/// the generic parameters of the function that contains the call.
type CallSite<'tcx> = (DefId, GenericArgsRef<'tcx>);

/// The call sites of every function of the crate that has a body. Closures are merged into the
/// function that defines them.
type CallSites<'tcx> = FxIndexMap<LocalDefId, FxIndexSet<CallSite<'tcx>>>;

/// A function of this crate and the generic arguments it is called with, written in the generic
/// parameters of the function the search started from.
type FunctionInstance<'tcx> = (LocalDefId, GenericArgsRef<'tcx>);

/// How the search first reached an instance.
#[derive(Clone, Copy)]
struct Reach {
    /// Index of the calling instance, in the order reached. The start is its own caller.
    caller: usize,
    /// Number of calls from the start of the search.
    depth: usize,
}

/// What the search from one function found.
enum Outcome<'tcx> {
    /// The function cannot reach itself.
    Acyclic,
    /// One shortest call path from the function back to itself, both ends included.
    Cycle(Vec<FunctionInstance<'tcx>>),
    /// The search hit `INSTANCES_PER_ROOT_MAX` or `CALL_DEPTH_MAX`, in this function.
    Unbounded(LocalDefId),
}

/// Reports every function of the crate that can reach itself.
pub fn check_crate(cx: &LateContext<'_>, lint: &'static Lint) {
    let tcx = cx.tcx;
    let sites = call_sites(tcx);
    for &root in sites.keys() {
        let name = tcx.def_path_str(root.to_def_id());
        let message = match search(tcx, &sites, root) {
            Outcome::Acyclic => continue,
            Outcome::Cycle(path) => {
                let names: Vec<String> =
                    path.iter().map(|&instance| instance_name(tcx, instance)).collect();
                format!(
                    "`{name}` is recursive: {}. Rewrite it as a bounded loop (ADR-0006)",
                    names.join(" -> ")
                )
            }
            Outcome::Unbounded(stopped_in) => format!(
                "`{name}` cannot be shown to be free of recursion: its calls reach more than \
                 {INSTANCES_PER_ROOT_MAX} function instances, or nest more than {CALL_DEPTH_MAX} \
                 deep. The search stopped in `{}`, which probably calls itself with generic \
                 arguments that grow with every call. Rewrite it as a bounded loop (ADR-0006)",
                tcx.def_path_str(stopped_in.to_def_id())
            ),
        };
        span_lint(cx, lint, tcx.def_span(root), message);
    }
}

/// Prints an instance as a path. Generic arguments are shown only where the search filled them
/// in, so a function with its own parameters prints as its plain path.
fn instance_name<'tcx>(tcx: TyCtxt<'tcx>, (function, args): FunctionInstance<'tcx>) -> String {
    let own = tcx.erase_and_anonymize_regions(GenericArgs::identity_for_item(tcx, function));
    if args == own {
        return tcx.def_path_str(function.to_def_id());
    }
    let limit = Limit::new(TYPES_PER_NAME_MAX);
    let mut printer = FmtPrinter::new_with_limit(tcx, Namespace::ValueNS, limit);
    printer.print_def_path(function.to_def_id(), args).expect("printing to a string cannot fail");
    printer.into_buffer()
}

fn call_sites(tcx: TyCtxt<'_>) -> CallSites<'_> {
    let mut sites = CallSites::default();
    for owner in tcx.hir_body_owners() {
        if !matches!(tcx.def_kind(owner), DefKind::Fn | DefKind::AssocFn | DefKind::Closure) {
            continue;
        }
        let root = tcx.typeck_root_def_id_local(owner);
        let mut collector =
            CallCollector { tcx, typeck: tcx.typeck(owner), sites: FxIndexSet::default() };
        collector.visit_body(tcx.hir_body_owned_by(owner));
        sites.entry(root).or_default().extend(collector.sites);
    }
    sites
}

struct CallCollector<'tcx> {
    tcx: TyCtxt<'tcx>,
    typeck: &'tcx TypeckResults<'tcx>,
    sites: FxIndexSet<CallSite<'tcx>>,
}

impl<'tcx> CallCollector<'tcx> {
    fn add_call(&mut self, callee: DefId, args: GenericArgsRef<'tcx>) {
        // Desugared calls (`for` loops, `?`) can carry no recorded arguments. Resolving those
        // would crash the compiler, so they are dropped. Their callees are lang items of the
        // standard library, never functions of this crate.
        let complete = args.len() == self.tcx.generics_of(callee).count();
        assert!(
            complete || !callee.is_local(),
            "a call to a function of this crate records all its generic arguments"
        );
        if complete {
            let _newly_added = self.sites.insert((callee, args));
        }
    }
}

impl<'tcx> Visitor<'tcx> for CallCollector<'tcx> {
    fn visit_expr(&mut self, expression: &'tcx Expr<'tcx>) {
        match expression.kind {
            ExprKind::Call(callee, _) => {
                if let ExprKind::Path(path) = &callee.kind
                    && let Res::Def(DefKind::Fn | DefKind::AssocFn, def_id) =
                        self.typeck.qpath_res(path, callee.hir_id)
                {
                    self.add_call(def_id, self.typeck.node_args(callee.hir_id));
                }
            }
            ExprKind::MethodCall(..) => {
                if let Some(def_id) = self.typeck.type_dependent_def_id(expression.hir_id) {
                    self.add_call(def_id, self.typeck.node_args(expression.hir_id));
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, expression);
    }

    fn visit_body(&mut self, body: &Body<'tcx>) {
        intravisit::walk_body(self, body);
    }
}

/// Breadth-first search over the instances that `root` can reach, starting from `root` with its
/// own generic parameters, until one of them is `root` again with any arguments.
fn search<'tcx>(tcx: TyCtxt<'tcx>, sites: &CallSites<'tcx>, root: LocalDefId) -> Outcome<'tcx> {
    let typing_env = TypingEnv::post_analysis(tcx, root);
    let own = tcx.erase_and_anonymize_regions(GenericArgs::identity_for_item(tcx, root));
    // Every instance reached so far, in the order reached, which is also the order of depth.
    let mut reached: FxIndexMap<FunctionInstance<'tcx>, Reach> = FxIndexMap::default();
    reached.insert((root, own), Reach { caller: 0, depth: 0 });
    for index in 0..=INSTANCES_PER_ROOT_MAX {
        let Some((&(function, args), &Reach { depth, .. })) = reached.get_index(index) else {
            return Outcome::Acyclic;
        };
        if index == INSTANCES_PER_ROOT_MAX || depth == CALL_DEPTH_MAX {
            return Outcome::Unbounded(function);
        }
        for &site in sites.get(&function).into_iter().flatten() {
            let Some(callee) = callee_instance(tcx, typing_env, args, site) else {
                continue;
            };
            if callee.0 == root {
                return Outcome::Cycle(cycle_path(&reached, index, callee));
            }
            reached.entry(callee).or_insert(Reach { caller: index, depth: depth + 1 });
        }
    }
    unreachable!("the search returns at the latest when it reaches the instance bound");
}

/// The instance that a call site reaches when the function around it is instantiated with
/// `caller_args`, or `None` if the callee has no body in this crate.
fn callee_instance<'tcx>(
    tcx: TyCtxt<'tcx>,
    typing_env: TypingEnv<'tcx>,
    caller_args: GenericArgsRef<'tcx>,
    (declared, call_args): CallSite<'tcx>,
) -> Option<FunctionInstance<'tcx>> {
    let call_args = EarlyBinder::bind(tcx, call_args);
    let normalized =
        tcx.try_instantiate_and_normalize_erasing_regions(caller_args, typing_env, call_args);
    let (callee, args) = match normalized {
        Ok(args) => match Instance::try_resolve(tcx, typing_env, declared, args) {
            // A function with a body, or a call through `dyn`, which names the trait's method.
            Ok(Some(Instance {
                def: InstanceKind::Item(callee) | InstanceKind::Virtual(callee, _),
                args,
            })) => (callee, args),
            // An intrinsic or a compiler-generated shim: no body in this crate.
            Ok(Some(_)) => return None,
            // Too generic to name an impl: the trait's own declaration stands in.
            Ok(None) | Err(_) => (declared, args),
        },
        // Resolving needs normalized arguments, so the declared callee stands in.
        Err(_) => (declared, call_args.instantiate(tcx, caller_args).skip_normalization()),
    };
    let root = tcx.typeck_root_def_id(callee).as_local()?;
    let generics = tcx.generics_of(root);
    assert!(args.len() >= generics.count(), "an instance has an argument for every parameter");
    Some((root, args.truncate_to(tcx, generics)))
}

/// Walks back from the instance at `last` to the start of the search, then appends `closing`,
/// the instance that closed the cycle. A self-call gives `[root, root]`.
fn cycle_path<'tcx>(
    reached: &FxIndexMap<FunctionInstance<'tcx>, Reach>,
    last: usize,
    closing: FunctionInstance<'tcx>,
) -> Vec<FunctionInstance<'tcx>> {
    let mut reversed = vec![closing];
    let mut index = last;
    for _ in 0..reached.len() {
        let (&instance, reach) =
            reached.get_index(index).expect("every caller is a reached instance");
        reversed.push(instance);
        if index == 0 {
            break;
        }
        assert!(reach.caller < index, "an instance is reached after its caller");
        index = reach.caller;
    }
    reversed.reverse();
    assert_eq!(reversed[0].0, closing.0, "the path starts and ends at the same function");
    reversed
}
