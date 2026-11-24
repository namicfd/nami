//! `nami_recursion`: no function may reach itself through calls (ADR-0006).
//!
//! The lint builds the crate's call graph from resolved calls: free functions, inherent and trait
//! methods (resolved to the concrete impl where the types are known), and closures, which count
//! as part of the function that defines them. It then reports every function that can reach
//! itself. Cargo forbids dependency cycles between crates, and kernels bans `dyn` and
//! function pointers (ADR-0003), so checking each crate on its own finds every cycle there.

use clippy_utils::diagnostics::span_lint;
use rustc_data_structures::fx::{FxIndexMap, FxIndexSet};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Body, Expr, ExprKind};
use rustc_lint::{LateContext, Lint};
use rustc_middle::ty::{self, GenericArgsRef, Instance, TyCtxt, TypeckResults};
use rustc_span::def_id::{DefId, LocalDefId};

/// Upper bound on the functions in one crate, so the cycle search is bounded.
const FUNCTIONS_MAX: usize = 1_000_000;

/// Reports every function of the crate that can reach itself.
pub fn check_crate(cx: &LateContext<'_>, lint: &'static Lint) {
    let graph = call_graph(cx.tcx);
    for (&function, path) in &cycles(&graph) {
        let names: Vec<String> =
            path.iter().map(|&node| cx.tcx.def_path_str(node.to_def_id())).collect();
        let message = format!(
            "`{}` is recursive: {}. Rewrite it as a bounded loop (ADR-0006)",
            cx.tcx.def_path_str(function.to_def_id()),
            names.join(" -> ")
        );
        span_lint(cx, lint, cx.tcx.def_span(function), message);
    }
}

/// Call graph over the crate's functions. Closures are merged into the function defining them.
type CallGraph = FxIndexMap<LocalDefId, FxIndexSet<LocalDefId>>;

fn call_graph(tcx: TyCtxt<'_>) -> CallGraph {
    let mut graph = CallGraph::default();
    for owner in tcx.hir_body_owners() {
        if !matches!(tcx.def_kind(owner), DefKind::Fn | DefKind::AssocFn | DefKind::Closure) {
            continue;
        }
        let root = tcx.typeck_root_def_id(owner.to_def_id()).expect_local();
        let mut collector =
            CallCollector { tcx, typeck: tcx.typeck(owner), owner, callees: FxIndexSet::default() };
        collector.visit_body(tcx.hir_body_owned_by(owner));
        graph.entry(root).or_default().extend(collector.callees);
    }
    graph
}

struct CallCollector<'tcx> {
    tcx: TyCtxt<'tcx>,
    typeck: &'tcx TypeckResults<'tcx>,
    owner: LocalDefId,
    callees: FxIndexSet<LocalDefId>,
}

impl<'tcx> CallCollector<'tcx> {
    fn add_call(&mut self, callee: DefId, args: GenericArgsRef<'tcx>) {
        // Desugared calls (`for` loops, `?`) can carry no recorded arguments. Resolving those
        // would crash the compiler, so they keep their declared callee, which is never local.
        let complete = args.len() == self.tcx.generics_of(callee).count();
        let typing_env = ty::TypingEnv::post_analysis(self.tcx, self.owner);
        let resolved =
            match complete.then(|| Instance::try_resolve(self.tcx, typing_env, callee, args)) {
                Some(Ok(Some(instance))) => instance.def_id(),
                _ => callee,
            };
        if let Some(local) = resolved.as_local() {
            let root = self.tcx.typeck_root_def_id(local.to_def_id()).expect_local();
            let _newly_added = self.callees.insert(root);
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

/// For every function that can reach itself, one shortest cycle through it.
fn cycles(graph: &CallGraph) -> FxIndexMap<LocalDefId, Vec<LocalDefId>> {
    assert!(graph.len() <= FUNCTIONS_MAX, "crate has more functions than the search bound");
    let mut found = FxIndexMap::default();
    for &start in graph.keys() {
        if let Some(path) = shortest_cycle(graph, start) {
            found.insert(start, path);
        }
    }
    found
}

/// Breadth-first search from `start` back to `start`. Returns the cycle, starting and ending
/// at `start`.
fn shortest_cycle(graph: &CallGraph, start: LocalDefId) -> Option<Vec<LocalDefId>> {
    let mut parent: FxIndexMap<LocalDefId, LocalDefId> = FxIndexMap::default();
    let mut frontier = vec![start];
    for _ in 0..=graph.len() {
        let mut next = Vec::new();
        for &node in &frontier {
            for &callee in graph.get(&node).into_iter().flatten() {
                if callee == start {
                    return Some(cycle_path(&parent, start, node));
                }
                if !parent.contains_key(&callee) {
                    parent.insert(callee, node);
                    next.push(callee);
                }
            }
        }
        if next.is_empty() {
            return None;
        }
        frontier = next;
    }
    None
}

fn cycle_path(
    parent: &FxIndexMap<LocalDefId, LocalDefId>,
    start: LocalDefId,
    last: LocalDefId,
) -> Vec<LocalDefId> {
    // Walk back from `last` to `start` through the search tree. A self-call gives [start, start].
    let mut reversed = vec![start, last];
    let mut node = last;
    for _ in 0..parent.len() {
        if node == start {
            break;
        }
        let previous = *parent.get(&node).expect("every reached function has a parent");
        reversed.push(previous);
        node = previous;
    }
    reversed.reverse();
    reversed
}
