//! Rules that need the syntax tree, parsed with `syn`.
//!
//! The visitor walks the syntax tree recursively through `syn::visit`. That is the tooling
//! exception to the no-recursion rule: the depth is bounded by the nesting of one source file,
//! and the recursion happens inside `syn`, not in a function of this crate. Everything written
//! here is a loop over a bounded range.
//!
//! `syn` does not parse the arguments of a macro invocation, so the rules that can be decided on
//! tokens (the bans and `magic-tolerance`) also read macro bodies through [`super::tokens`].

use super::registry::{self, MACRO_BANS, PATH_BANS};
use super::tokens;
use super::{Scope, Violation};
use proc_macro2::{Ident, Span, TokenTree};
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Expr, Signature, Type, UseTree, Visibility};

/// Hard limit on physical lines per function, closures included (`TIGER_STYLE.md`).
const FUNCTION_LINES_MAX: u32 = 70;

/// Functions up to this many lines are trivial and excluded from the density average.
const FUNCTION_LINES_TRIVIAL_MAX: u32 = 5;

/// Functions longer than this must contain at least one assertion (ADR-0004).
const FUNCTION_LINES_UNASSERTED_MAX: u32 = 10;

/// The only crate that may contain `unsafe` code (ADR-0014).
const UNSAFE_ALLOWED_CRATE: &str = "crates/alloc-guard";

/// Macros that count as assertions.
const ASSERTION_MACROS: [&str; 6] =
    ["assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne"];

/// Largest float literal that is taken for a tolerance, epsilon or floor. A literal in
/// `(0, MAGIC_TOLERANCE_MAX]` must be a documented constant (`TIGER_STYLE.md`, Floating point).
/// Physical values this small are rare in nami's units; a real one is named as a constant too.
const MAGIC_TOLERANCE_MAX: f64 = 1.0e-6;

/// Functions in `core::iter` whose iterator never ends, or ends only when a closure says so.
const UNBOUNDED_SOURCES: [&str; 4] = ["from_fn", "repeat", "repeat_with", "successors"];

/// The iterator method that repeats its receiver forever.
const UNBOUNDED_METHOD: &str = "cycle";

/// Upper bound on the steps of the walk over the iterable of a `for` loop.
const ITERABLE_STEPS_MAX: u32 = 256;

/// Upper bound on the subtrees of one `use` item, so the walk is bounded.
const USE_TREES_MAX: u32 = 100_000;

/// The directory of a crate's integration tests, which are test code as a whole.
const TESTS_DIRECTORY: &str = "tests";

/// Assertion counts over the non-trivial, non-test functions of a file or crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssertionStats {
    /// Number of non-trivial functions.
    pub functions: u32,
    /// Number of assertions in those functions.
    pub assertions: u32,
}

impl AssertionStats {
    /// No functions, no assertions.
    #[must_use]
    pub const fn zero() -> Self {
        Self { functions: 0, assertions: 0 }
    }

    /// Adds another set of counts to this one.
    pub fn add(&mut self, other: Self) {
        self.functions += other.functions;
        self.assertions += other.assertions;
    }

    /// True if the average is at least two assertions per non-trivial function.
    #[must_use]
    pub fn meets_density(self) -> bool {
        u64::from(self.assertions) >= 2 * u64::from(self.functions)
    }
}

/// The result of checking one file.
pub struct Report {
    pub violations: Vec<Violation>,
    pub stats: AssertionStats,
}

/// Parses `text` and runs every syntax rule that applies to `scope`.
pub fn check(path: &Path, text: &str, scope: Scope) -> Report {
    let file = match syn::parse_file(text) {
        Ok(file) => file,
        Err(error) => {
            let line = line_of(error.span());
            let message = format!("cannot parse this file: {error}");
            let violations = vec![Violation::new(path, line, "parse", message)];
            return Report { violations, stats: AssertionStats::zero() };
        }
    };
    let mut visitor = Visitor {
        path,
        scope,
        test_depth: 0,
        test_function_depth: 0,
        is_test_file: path.components().any(|part| part.as_os_str() == TESTS_DIRECTORY),
        in_trait_impl: false,
        const_depth: 0,
        const_holds_tolerance: false,
        violations: Vec::new(),
        stats: AssertionStats::zero(),
    };
    visitor.visit_file(&file);
    assert_eq!(visitor.test_depth, 0, "every test module that was entered was left");
    assert_eq!(visitor.const_depth, 0, "every constant that was entered was left");
    Report { violations: visitor.violations, stats: visitor.stats }
}

struct Visitor<'a> {
    path: &'a Path,
    scope: Scope,
    /// Depth of `#[cfg(test)]` modules around the current item.
    test_depth: u32,
    /// Depth of `#[test]` functions around the current item.
    test_function_depth: u32,
    /// True if the file lies under a `tests/` directory.
    is_test_file: bool,
    /// True while visiting the items of an `impl Trait for Type` block.
    in_trait_impl: bool,
    /// Depth of `const` items around the current expression.
    const_depth: u32,
    /// True once the innermost `const` item around the current expression holds a tolerance.
    const_holds_tolerance: bool,
    violations: Vec<Violation>,
    stats: AssertionStats,
}

/// The parts of a function with a body that the rules look at.
struct Function<'a> {
    attributes: &'a [Attribute],
    signature: &'a Signature,
    block: &'a Block,
}

impl Visitor<'_> {
    fn report(&mut self, span: Span, rule: &'static str, message: impl Into<String>) {
        self.violations.push(Violation::new(self.path, line_of(span), rule, message));
    }

    fn in_tests(&self) -> bool {
        self.test_depth > 0
    }

    /// Test code: `#[cfg(test)]` modules, `#[test]` functions and files under `tests/`.
    fn in_test_code(&self) -> bool {
        self.in_tests() || self.test_function_depth > 0 || self.is_test_file
    }

    fn usize_restricted(&self) -> bool {
        self.scope.is_nami() && self.scope != Scope::IndexModule
    }

    /// Rules about a signature. `dictated_by_trait` is true for a method of an
    /// `impl Trait for Type` block, whose parameter types the trait decides.
    fn check_signature(&mut self, signature: &Signature, dictated_by_trait: bool) {
        let name = &signature.ident;
        if let syn::Safety::Unsafe(unsafety) = &signature.safety {
            self.report_unsafe(unsafety.span);
        }
        if let Some(asyncness) = &signature.asyncness {
            let message =
                "async functions are banned; functions run to completion (TIGER_STYLE.md)";
            self.report(asyncness.span, "async", message);
        }
        if self.usize_restricted() && !dictated_by_trait && signature_mentions_usize(signature) {
            let message = format!(
                "`usize` in the signature of function `{name}`; use a typed index or count such \
                 as `CellIndex` or `CellCount` (ADR-0002)"
            );
            self.report(signature.fn_token.span, "usize", message);
        }
    }

    /// Rules about a function body: its length and its assertions.
    fn check_body(&mut self, function: &Function<'_>) {
        let name = &function.signature.ident;
        let first = line_of(function.signature.fn_token.span);
        let last = line_of_end(function.block.brace_token.span.close());
        assert!(last >= first, "a function ends after it starts");
        let lines = last - first + 1;
        if lines > FUNCTION_LINES_MAX {
            let message = format!(
                "function `{name}` has {lines} lines; the limit is 70. Split it: push ifs up and \
                 fors down, keep leaf functions pure (TIGER_STYLE.md)"
            );
            self.report(function.signature.fn_token.span, "function-length", message);
        }
        let is_test = self.in_tests() || has_attribute(function.attributes, "test");
        if self.scope.is_nami() && !is_test {
            self.count_assertions(function, lines);
        }
    }

    fn count_assertions(&mut self, function: &Function<'_>, lines: u32) {
        let mut counter = AssertionCounter { count: 0 };
        counter.visit_block(function.block);
        if lines > FUNCTION_LINES_UNASSERTED_MAX && counter.count == 0 {
            let message = format!(
                "function `{}` has {lines} lines and no assertions; assert its arguments, results \
                 and invariants (ADR-0004)",
                function.signature.ident
            );
            self.report(function.signature.fn_token.span, "unasserted-function", message);
        }
        if lines > FUNCTION_LINES_TRIVIAL_MAX {
            self.stats.add(AssertionStats { functions: 1, assertions: counter.count });
        }
    }

    fn check_derives(&mut self, attributes: &[Attribute]) {
        if !self.scope.is_nami() {
            return;
        }
        for attribute in attributes.iter().filter(|attribute| derives_default(attribute)) {
            let message = "`Default` is banned in nami crates; spell out every field at the call \
                           site, especially numerical configuration (TIGER_STYLE.md)";
            self.report(attribute.span(), "default", message);
        }
    }

    fn check_field_type(&mut self, field_type: &Type) {
        if self.usize_restricted() && type_mentions_usize(field_type) {
            let message = "`usize` in a field; use a typed index or count (ADR-0002)";
            self.report(field_type.span(), "usize", message);
        }
        if self.scope.is_nami() && type_mentions_vec(field_type) {
            let message = "`Vec` in a field type; storage that must not grow is `Box<[T]>`, \
                           allocated once in init (ADR-0003, TIGER_STYLE.md)";
            self.report(field_type.span(), "vec-field", message);
        }
    }

    fn check_public_item_type(&mut self, visibility: &Visibility, item_type: &Type) {
        if is_public(visibility) && self.usize_restricted() && type_mentions_usize(item_type) {
            let message = "`usize` in a public constant, static or alias; use a typed index or \
                           count, or `u32` (ADR-0002)";
            self.report(item_type.span(), "usize", message);
        }
    }

    /// Applies [`PATH_BANS`] to the first segment of a path, a `use` or an `extern crate`.
    fn check_path_start(&mut self, segment: &Ident) {
        for ban in PATH_BANS {
            let applies = ban.scope.includes(self.scope);
            if segment == ban.segment && applies && !registry::is_allowed(ban.allowed, self.path) {
                self.report(segment.span(), ban.id, ban.message);
            }
        }
    }

    /// Applies [`MACRO_BANS`] to the name of an invoked macro.
    fn check_macro_name(&mut self, name: &Ident) {
        for ban in MACRO_BANS {
            let is_banned = ban.names.iter().any(|banned| name == banned);
            if is_banned && !registry::is_allowed(ban.allowed, self.path) {
                self.report(name.span(), ban.id, ban.message);
            }
        }
    }

    /// A small float literal is a tolerance, an epsilon or a floor. It must be a named,
    /// documented constant, so that its origin is written down exactly once.
    fn check_tolerance(&mut self, literal: &syn::LitFloat) {
        if !self.scope.is_nami() || self.in_test_code() {
            return;
        }
        let Ok(value) = literal.base10_parse::<f64>() else {
            return;
        };
        if !is_tolerance(value) {
            return;
        }
        if self.const_depth > 0 {
            self.const_holds_tolerance = true;
            return;
        }
        let message = format!(
            "magic tolerance `{literal}`; name it as a documented constant that says where it \
             comes from. If it floors or clamps a state, that is banned outright \
             (TIGER_STYLE.md, Floating point; Physical admissibility)"
        );
        self.report(literal.span(), "magic-tolerance", message);
    }

    /// Enters a `const` item. Returns the state to hand back to [`Self::leave_const`].
    fn enter_const(&mut self) -> bool {
        self.const_depth += 1;
        std::mem::replace(&mut self.const_holds_tolerance, false)
    }

    /// Leaves a `const` item: a constant that holds a tolerance must say where it comes from.
    fn leave_const(&mut self, outer_holds_tolerance: bool, name: &Ident, attributes: &[Attribute]) {
        assert!(self.const_depth > 0, "a constant is left only after it was entered");
        self.const_depth -= 1;
        if self.const_holds_tolerance && !has_doc_comment(attributes) {
            let message = format!(
                "constant `{name}` holds a tolerance but has no doc comment; add one that says \
                 where the value comes from (TIGER_STYLE.md, Floating point)"
            );
            self.report(name.span(), "magic-tolerance", message);
        }
        self.const_holds_tolerance = outer_holds_tolerance;
    }

    /// Applies the rules that can be decided on tokens to one token sequence of a macro body.
    fn check_macro_tokens(&mut self, sequence: &[TokenTree]) {
        let found = tokens::findings(sequence);
        for segment in found.path_starts {
            self.check_path_start(segment);
        }
        for name in found.macro_calls {
            self.check_macro_name(name);
        }
        let literals = found.literals.into_iter().cloned().map(syn::Lit::new);
        for literal in literals {
            if let syn::Lit::Float(float) = literal {
                self.check_tolerance(&float);
            }
        }
    }

    fn report_unsafe(&mut self, span: Span) {
        if !self.path.starts_with(UNSAFE_ALLOWED_CRATE) {
            let message = "`unsafe` outside crates/alloc-guard; nami has no other unsafe \
                           code. Find a safe formulation, or propose an ADR (ADR-0014)";
            self.report(span, "unsafe", message);
        }
    }

    fn report_kernel_indirection(&mut self, span: Span, what: &str) {
        if self.scope == Scope::Kernels && !self.in_tests() {
            let message = format!(
                "{what} in kernels; use generics so every call resolves statically \
                 (ADR-0003, ADR-0006)"
            );
            self.report(span, "kernel-indirection", message);
        }
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let is_test_module = is_cfg_test(&node.attrs);
        self.test_depth += u32::from(is_test_module);
        visit::visit_item_mod(self, node);
        self.test_depth -= u32::from(is_test_module);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.check_signature(&node.sig, false);
        self.check_body(&Function {
            attributes: &node.attrs,
            signature: &node.sig,
            block: &node.block,
        });
        let is_test = has_attribute(&node.attrs, "test");
        self.test_function_depth += u32::from(is_test);
        visit::visit_item_fn(self, node);
        self.test_function_depth -= u32::from(is_test);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.check_signature(&node.sig, self.in_trait_impl);
        self.check_body(&Function {
            attributes: &node.attrs,
            signature: &node.sig,
            block: &node.block,
        });
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.check_signature(&node.sig, false);
        if let Some(block) = &node.default {
            self.check_body(&Function { attributes: &node.attrs, signature: &node.sig, block });
        }
        visit::visit_trait_item_fn(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        let message = "`loop` is unbounded; use `for _ in 0..LIMIT` and assert after the loop \
                       (TIGER_STYLE.md, Put a limit on everything)";
        self.report(node.loop_token.span, "unbounded-loop", message);
        visit::visit_expr_loop(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        let message = "`while` is unbounded; use `for _ in 0..LIMIT` and assert after the loop \
                       (TIGER_STYLE.md, Put a limit on everything)";
        self.report(node.while_token.span, "unbounded-loop", message);
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        if iterates_without_bound(&node.expr) {
            let message = "`for` over an iterator without an end (`start..`, `repeat`, \
                           `repeat_with`, `from_fn`, `successors`, `cycle`) is unbounded; bound \
                           it with `.take(LIMIT)`, `zip` it with a bounded iterator, use \
                           `repeat_n(x, LIMIT)`, or give a range an end (`0..LIMIT`), and assert \
                           after the loop (TIGER_STYLE.md, Put a limit on everything)";
            self.report(node.for_token.span, "unbounded-loop", message);
        }
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_async(&mut self, node: &'ast syn::ExprAsync) {
        let message = "async blocks are banned; functions run to completion (TIGER_STYLE.md)";
        self.report(node.async_token.span, "async", message);
        visit::visit_expr_async(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.check_derives(&node.attrs);
        visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.check_derives(&node.attrs);
        visit::visit_item_enum(self, node);
    }

    // Fields of structs, enum variants and unions.
    fn visit_field(&mut self, node: &'ast syn::Field) {
        self.check_field_type(&node.ty);
        visit::visit_field(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.report_unsafe(node.unsafe_token.span);
        visit::visit_expr_unsafe(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if let Some(unsafety) = &node.unsafety {
            self.report_unsafe(unsafety.span);
        }
        visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if let Some(unsafety) = &node.unsafety {
            self.report_unsafe(unsafety.span);
        }
        let implements_default =
            node.trait_.as_ref().is_some_and(|(path, _)| last_segment_is(path, "Default"));
        if self.scope.is_nami() && implements_default {
            let message = "`impl Default` is banned in nami crates; spell out every field at the \
                           call site (TIGER_STYLE.md)";
            self.report(node.impl_token.span, "default", message);
        }
        let outer = std::mem::replace(&mut self.in_trait_impl, node.trait_.is_some());
        visit::visit_item_impl(self, node);
        self.in_trait_impl = outer;
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.check_public_item_type(&node.vis, &node.ty);
        let outer = self.enter_const();
        visit::visit_item_const(self, node);
        self.leave_const(outer, &node.ident, &node.attrs);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        let outer = self.enter_const();
        visit::visit_impl_item_const(self, node);
        self.leave_const(outer, &node.ident, &node.attrs);
    }

    fn visit_trait_item_const(&mut self, node: &'ast syn::TraitItemConst) {
        let outer = self.enter_const();
        visit::visit_trait_item_const(self, node);
        self.leave_const(outer, &node.ident, &node.attrs);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.check_public_item_type(&node.vis, &node.ty);
        visit::visit_item_static(self, node);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if self.usize_restricted() && type_mentions_usize(&node.ty) {
            let message = "type alias to `usize`; use a typed index or count (ADR-0002)";
            self.report(node.type_token.span, "usize", message);
        }
        visit::visit_item_type(self, node);
    }

    fn visit_type_trait_object(&mut self, node: &'ast syn::TypeTraitObject) {
        self.report_kernel_indirection(node.span(), "`dyn` trait object");
        visit::visit_type_trait_object(self, node);
    }

    fn visit_type_fn_ptr(&mut self, node: &'ast syn::TypeFnPtr) {
        self.report_kernel_indirection(node.fn_token.span, "function pointer");
        visit::visit_type_fn_ptr(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        if node.path().is_ident("ignore") && matches!(node.meta, syn::Meta::Path(_)) {
            let message = "`#[ignore]` without a reason; write `#[ignore = \"why\"]`, or fix the \
                           test (ADR-0006)";
            self.report(node.span(), "ignore-reason", message);
        }
        visit::visit_attribute(self, node);
    }

    // Every path in expressions, types, patterns and macro names. A lone identifier is not a
    // path into a crate, so only paths of two or more segments are checked.
    fn visit_path(&mut self, node: &'ast syn::Path) {
        if node.segments.len() > 1
            && let Some(first) = node.segments.first()
        {
            self.check_path_start(&first.ident);
        }
        visit::visit_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        for root in use_roots(&node.tree) {
            self.check_path_start(root);
        }
        visit::visit_item_use(self, node);
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        self.check_path_start(&node.ident);
        visit::visit_item_extern_crate(self, node);
    }

    fn visit_lit_float(&mut self, node: &'ast syn::LitFloat) {
        self.check_tolerance(node);
        visit::visit_lit_float(self, node);
    }

    // `syn` keeps macro arguments as tokens, so the token rules read them here.
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if let Some(name) = node.path.segments.last() {
            self.check_macro_name(&name.ident);
        }
        for sequence in tokens::sequences(node.tokens.clone()) {
            self.check_macro_tokens(&sequence);
        }
        visit::visit_macro(self, node);
    }
}

/// Counts assertion macros inside one function body.
struct AssertionCounter {
    count: u32,
}

impl<'ast> Visit<'ast> for AssertionCounter {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let name = node.path.segments.last().map(|segment| segment.ident.to_string());
        if name.is_some_and(|name| ASSERTION_MACROS.contains(&name.as_str())) {
            self.count += 1;
        }
        visit::visit_macro(self, node);
    }
}

/// Finds a path that satisfies `matches` anywhere inside a type or signature.
struct PathFinder {
    matches: fn(&syn::Path) -> bool,
    found: bool,
}

impl<'ast> Visit<'ast> for PathFinder {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        self.found |= (self.matches)(node);
        visit::visit_path(self, node);
    }
}

fn is_usize(path: &syn::Path) -> bool {
    path.is_ident("usize")
}

fn is_vec(path: &syn::Path) -> bool {
    last_segment_is(path, "Vec")
}

fn type_mentions_usize(node: &Type) -> bool {
    let mut finder = PathFinder { matches: is_usize, found: false };
    finder.visit_type(node);
    finder.found
}

/// True for `Vec` anywhere inside the type: `Vec<f64>`, `Option<Vec<f64>>`, `[Vec<f64>; 3]`.
fn type_mentions_vec(node: &Type) -> bool {
    let mut finder = PathFinder { matches: is_vec, found: false };
    finder.visit_type(node);
    finder.found
}

/// Checks parameters and the return type. Const generic parameters may be `usize`, because
/// array lengths have to be.
fn signature_mentions_usize(signature: &Signature) -> bool {
    let mut finder = PathFinder { matches: is_usize, found: false };
    for input in &signature.inputs {
        finder.visit_fn_arg(input);
    }
    finder.visit_return_type(&signature.output);
    finder.found
}

/// How two iterators combine: `zip` ends with the shorter, `chain` runs both.
#[derive(Clone, Copy)]
enum Combine {
    /// `a.zip(b)` or `iter::zip(a, b)`: unbounded only if both are.
    Zip,
    /// `a.chain(b)` or `iter::chain(a, b)`: unbounded if either is.
    Chain,
}

/// One step of the walk over an iterable.
enum Step<'ast> {
    /// The iterable is known: unbounded (`true`) or not.
    Known(bool),
    /// The iterable is bounded exactly when this expression is: a wrapper or an adaptor.
    Through(&'ast Expr),
    /// The iterable combines two iterators.
    Pair(Combine, &'ast Expr, &'ast Expr),
}

/// Work left in [`iterates_without_bound`]: an expression to look at, or two results to combine.
enum Pending<'ast> {
    Visit(&'ast Expr),
    Combine(Combine),
}

/// True if a `for` loop over `iterable` has no upper bound. The iterable starts from an
/// unbounded source (see [`step`]) under parentheses, references and adaptors, and no `take`
/// follows it. A `zip` is unbounded only if both sides are, a `chain` if either side is.
fn iterates_without_bound(iterable: &Expr) -> bool {
    // An explicit stack instead of recursion: expressions to visit, and results to combine.
    let mut pending = vec![Pending::Visit(iterable)];
    let mut results: Vec<bool> = Vec::new();
    for _ in 0..ITERABLE_STEPS_MAX {
        let Some(work) = pending.pop() else {
            assert_eq!(results.len(), 1, "the walk leaves exactly one result");
            return results.pop().is_some_and(|unbounded| unbounded);
        };
        match work {
            Pending::Visit(expression) => match step(expression) {
                Step::Known(unbounded) => results.push(unbounded),
                Step::Through(inner) => pending.push(Pending::Visit(inner)),
                Step::Pair(combine, left, right) => {
                    pending.push(Pending::Combine(combine));
                    pending.push(Pending::Visit(left));
                    pending.push(Pending::Visit(right));
                }
            },
            Pending::Combine(combine) => {
                let right = results.pop().expect("a pair visits its right side");
                let left = results.pop().expect("a pair visits its left side");
                results.push(match combine {
                    Combine::Zip => left && right,
                    Combine::Chain => left || right,
                });
            }
        }
    }
    // Deeper than any real iterable: it cannot be shown to be bounded, so it is reported.
    true
}

/// Classifies one expression of an iterable. The unbounded sources are a range without an end
/// (`start..`), `iterator.cycle()`, and a call to one of [`UNBOUNDED_SOURCES`] in `core::iter`
/// (see [`is_iter_function`]).
fn step(expression: &Expr) -> Step<'_> {
    if let Expr::Paren(paren) = expression {
        return Step::Through(&paren.expr);
    }
    if let Expr::Group(group) = expression {
        return Step::Through(&group.expr);
    }
    if let Expr::Reference(reference) = expression {
        return Step::Through(&reference.expr);
    }
    if let Expr::Range(range) = expression {
        return Step::Known(range.end.is_none());
    }
    if let Expr::MethodCall(call) = expression {
        return method_step(call);
    }
    if let Expr::Call(call) = expression
        && let Expr::Path(function) = &*call.func
    {
        return function_step(&function.path, call);
    }
    Step::Known(false)
}

/// [`step`] for `receiver.method(..)`: `cycle` never ends, `take` ends, `zip` and `chain`
/// combine the receiver with their argument, and any other adaptor passes the receiver through.
fn method_step(call: &syn::ExprMethodCall) -> Step<'_> {
    if call.method == UNBOUNDED_METHOD {
        return Step::Known(true);
    }
    if call.method == "take" {
        return Step::Known(false);
    }
    match (combine_of(&call.method), call.args.first()) {
        (Some(combine), Some(argument)) => Step::Pair(combine, &call.receiver, argument),
        _ => Step::Through(&call.receiver),
    }
}

/// [`step`] for a call of the function at `path`: `iter::zip(a, b)` and `iter::chain(a, b)`
/// combine their arguments, and the [`UNBOUNDED_SOURCES`] never end. Only functions that can
/// be in `core::iter` count (see [`is_iter_function`]).
fn function_step<'ast>(path: &syn::Path, call: &'ast syn::ExprCall) -> Step<'ast> {
    if !is_iter_function(path) {
        return Step::Known(false);
    }
    let combine = path.segments.last().and_then(|segment| combine_of(&segment.ident));
    let mut arguments = call.args.iter();
    match (combine, arguments.next(), arguments.next(), arguments.next()) {
        (Some(combine), Some(left), Some(right), None) => Step::Pair(combine, left, right),
        _ => Step::Known(UNBOUNDED_SOURCES.iter().any(|name| last_segment_is(path, name))),
    }
}

/// `zip` and `chain`, by name.
fn combine_of(name: &Ident) -> Option<Combine> {
    if name == "zip" {
        Some(Combine::Zip)
    } else if name == "chain" {
        Some(Combine::Chain)
    } else {
        None
    }
}

/// True for a function path that can name `core::iter`: a bare name as imported from it
/// (`repeat`), or a path whose last qualifier is `iter` (`iter::repeat`, `std::iter::repeat`).
/// Any other qualifier names something else: `array::from_fn`, `Self::repeat`.
fn is_iter_function(path: &syn::Path) -> bool {
    let mut qualifiers = path.segments.iter().rev().skip(1);
    match qualifiers.next() {
        None => path.leading_colon.is_none(),
        Some(qualifier) => qualifier.ident == "iter",
    }
}

/// The first segment of every path that a `use` item imports: `a` and `c` in
/// `use {a::b, c as d};`.
fn use_roots(tree: &UseTree) -> Vec<&Ident> {
    let mut pending = vec![tree];
    let mut roots = Vec::new();
    for _ in 0..USE_TREES_MAX {
        let Some(subtree) = pending.pop() else {
            return roots;
        };
        match subtree {
            UseTree::Path(path) => roots.push(&path.ident),
            UseTree::Name(name) => roots.push(&name.ident),
            UseTree::Rename(rename) => roots.push(&rename.ident),
            UseTree::Glob(_) => {}
            UseTree::Group(group) => pending.extend(&group.items),
        }
    }
    panic!("a `use` item has more than {USE_TREES_MAX} subtrees");
}

/// True for a value in `(0, MAGIC_TOLERANCE_MAX]`. A literal carries no sign, so `-1.0e-12` is
/// seen as `1.0e-12`, and zero is exempt.
fn is_tolerance(value: f64) -> bool {
    value > 0.0 && value <= MAGIC_TOLERANCE_MAX
}

fn is_public(visibility: &Visibility) -> bool {
    !matches!(visibility, Visibility::Inherited)
}

fn last_segment_is(path: &syn::Path, name: &str) -> bool {
    path.segments.last().is_some_and(|segment| segment.ident == name)
}

fn has_attribute(attributes: &[Attribute], name: &str) -> bool {
    attributes.iter().any(|attribute| attribute.path().is_ident(name))
}

/// True for `/// ...` or `#[doc = "..."]`. `#[doc(hidden)]` documents nothing.
fn has_doc_comment(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("doc") && attribute.meta.require_name_value().is_ok()
    })
}

fn is_cfg_test(attributes: &[Attribute]) -> bool {
    attributes.iter().filter(|attribute| attribute.path().is_ident("cfg")).any(|attribute| {
        let mut found = false;
        let parsed = attribute.parse_nested_meta(|meta| {
            found |= meta.path.is_ident("test");
            Ok(())
        });
        parsed.is_ok() && found
    })
}

fn derives_default(attribute: &Attribute) -> bool {
    if !attribute.path().is_ident("derive") {
        return false;
    }
    let mut found = false;
    let parsed = attribute.parse_nested_meta(|meta| {
        found |= last_segment_is(&meta.path, "Default");
        Ok(())
    });
    parsed.is_ok() && found
}

fn line_of(span: Span) -> u32 {
    u32::try_from(span.start().line).expect("source files have fewer than 2^32 lines")
}

fn line_of_end(span: Span) -> u32 {
    u32::try_from(span.end().line).expect("source files have fewer than 2^32 lines")
}

#[cfg(test)]
mod tests {
    use super::{AssertionStats, check};
    use crate::tidy::Scope;
    use std::path::Path;

    const NONE: [&str; 0] = [];

    fn rules(text: &str, scope: Scope) -> Vec<&'static str> {
        let report = check(Path::new("fixture.rs"), text, scope);
        report.violations.iter().map(|violation| violation.rule).collect()
    }

    /// The rules that fire on `text` in a file at `path`. The scope follows from the path.
    fn rules_at(path: &str, text: &str) -> Vec<&'static str> {
        let path = Path::new(path);
        let report = check(path, text, Scope::of(path));
        report.violations.iter().map(|violation| violation.rule).collect()
    }

    /// The rules that fire on `text` in an ordinary nami crate.
    fn rules_in_crate(text: &str) -> Vec<&'static str> {
        rules_at("crates/solver/src/example.rs", text)
    }

    fn function_with_lines(name: &str, body_lines: u32, assertions: u32) -> String {
        let mut text = format!("fn {name}() {{\n");
        for _ in 0..assertions {
            text.push_str("    assert!(true, \"x\");\n");
        }
        for _ in assertions..body_lines {
            text.push_str("    let _ = 1;\n");
        }
        text.push_str("}\n");
        text
    }

    // Goal: functions over 70 physical lines are flagged, functions of exactly 70 are not.
    #[test]
    fn function_length_rule_fires() {
        let at_limit = function_with_lines("a", 68, 2);
        assert_eq!(rules(&at_limit, Scope::Tooling), NONE);
        let over_limit = function_with_lines("a", 69, 2);
        assert_eq!(rules(&over_limit, Scope::Tooling), ["function-length"]);
    }

    // Goal: density counts only non-trivial functions; long unasserted functions are flagged.
    #[test]
    fn assertion_rules_fire() {
        let trivial = check(Path::new("f.rs"), &function_with_lines("a", 3, 0), Scope::Crate);
        assert_eq!(trivial.stats, AssertionStats::zero());
        let dense = check(Path::new("f.rs"), &function_with_lines("a", 8, 2), Scope::Crate);
        assert_eq!(dense.stats, AssertionStats { functions: 1, assertions: 2 });
        assert!(dense.stats.meets_density(), "two assertions meet the density");
        assert_eq!(rules(&function_with_lines("a", 9, 0), Scope::Crate), ["unasserted-function"]);
        assert_eq!(rules(&function_with_lines("a", 9, 0), Scope::Tooling), NONE);
    }

    // Goal: `loop`, `while` and `async` are rejected everywhere.
    #[test]
    fn control_flow_rules_fire() {
        assert_eq!(rules("fn a() { loop {} }", Scope::Tooling), ["unbounded-loop"]);
        assert_eq!(rules("fn a() { while true {} }", Scope::Tooling), ["unbounded-loop"]);
        assert_eq!(rules("async fn a() {}", Scope::Tooling), ["async"]);
        assert_eq!(rules("trait T { async fn a(); }", Scope::Tooling), ["async"]);
        assert_eq!(rules("fn a() { for _ in 0..3 {} }", Scope::Tooling), NONE);
    }

    // Goal: a `for` over a range without an end, or over `repeat`, `repeat_with`, `from_fn`,
    // `successors` or `cycle`, is unbounded, however it is wrapped, zipped with another unbounded
    // iterator, or chained.
    #[test]
    fn unbounded_iterators_are_reported() {
        let unbounded = [
            "for step in 0_u32.. {}",
            "for step in (0_u32..) {}",
            "for step in start.. {}",
            "for step in (0..).step_by(2) {}",
            "for step in (0..).map(|i| i * 2).filter(|i| *i > 2) {}",
            "for step in &mut (0..) {}",
            "for step in (0..).take_while(|i| *i < 3) {}",
            "for step in (0..).skip(3) {}",
            "for value in core::iter::repeat(1.0) {}",
            "for value in repeat(1.0).map(|v| v * 2.0) {}",
            "for value in std::iter::repeat_with(|| 1.0) {}",
            "for value in iter::from_fn(|| Some(1)) {}",
            "for step in successors(Some(0_u32), |step| Some(step + 1)) {}",
            "for value in values.iter().cycle() {}",
            "for value in values.iter().take(3).cycle() {}",
            "for value in &mut values.iter().cycle() {}",
            "for (step, value) in (0..).zip(repeat(1.0)) {}",
            "for (value, step) in values.iter().cycle().zip((0..).skip(1)) {}",
            "for value in ::core::iter::repeat(1.0) {}",
            "for value in cells.iter().chain(repeat(&1.0)) {}",
            "for value in (0..).chain(cells) {}",
            "for value in cells.iter().chain(other.iter().cycle()).take_while(|v| **v > 0.0) {}",
            "for (step, value) in iter::zip(0.., repeat(1.0)) {}",
            "for value in core::iter::chain(cells, repeat(&1.0)) {}",
        ];
        for body in unbounded {
            let text = format!("fn a() {{ {body} }}");
            assert_eq!(rules(&text, Scope::Tooling), ["unbounded-loop"], "{body}");
        }
    }

    // Goal: a range with an end is bounded, and `take` or `zip` bound a range or an iterator
    // without one, unless `zip` pairs two unbounded iterators. `chain` is bounded when both
    // sides are. A function that is not in `core::iter` (`array::from_fn`) is not a source.
    #[test]
    fn bounded_iterators_pass() {
        let bounded = [
            "for step in 0..10 {}",
            "for step in 0..=10 {}",
            "for step in (0..10).rev() {}",
            "for step in (0..).take(3) {}",
            "for step in (0..).step_by(2).take(3) {}",
            "for step in (0..).take(3).map(|i| i * 2) {}",
            "for (step, cell) in (0_u32..).zip(cells) {}",
            "for (cell, step) in cells.iter().zip(0..) {}",
            "for cell in cells {}",
            "for cell in cells.iter().skip(1) {}",
            "for value in core::iter::repeat(1.0).take(3) {}",
            "for value in values.iter().cycle().take(6) {}",
            "for step in successors(Some(0_u32), |step| Some(step + 1)).take(3) {}",
            "for (value, cell) in repeat(1.0).zip(cells) {}",
            "for (cell, value) in cells.iter().zip(repeat_with(|| 1.0)) {}",
            "for (step, value) in (0..).zip(repeat(1.0).take(3)) {}",
            "for value in core::iter::repeat_n(1.0, 3) {}",
            "for value in core::iter::once(1.0) {}",
            "for value in text.repeat(3).chars() {}",
            "for value in core::array::from_fn::<f64, 3, _>(|i| 1.0) {}",
            "for value in array::from_fn::<f64, 3, _>(|i| 1.0) {}",
            "for value in Self::repeat(1.0) {}",
            "for step in my_mod::successors(0) {}",
            "for value in cells.iter().chain(other) {}",
            "for value in cells.iter().chain(repeat(&1.0)).take(3) {}",
            "for (step, value) in iter::zip(0.., cells) {}",
            "for (cell, value) in zip(cells, repeat(1.0)) {}",
        ];
        for body in bounded {
            let text = format!("fn a() {{ {body} }}");
            assert_eq!(rules(&text, Scope::Tooling), NONE, "{body}");
        }
    }

    // Goal: `usize` is rejected in signatures, fields, public items and aliases, except in the
    // index module and in tooling. Private constants and const generics stay allowed.
    #[test]
    fn usize_rule_fires() {
        assert_eq!(rules("pub fn a(n: usize) {}", Scope::Crate), ["usize"]);
        assert_eq!(rules("fn a(n: usize) {}", Scope::Crate), ["usize"], "private functions too");
        assert_eq!(rules("fn a() -> usize { 0 }", Scope::Crate), ["usize"], "return types too");
        assert_eq!(rules("fn a(n: &[usize]) {}", Scope::Crate), ["usize"], "nested in a type");
        assert_eq!(rules("struct S { n: usize }", Scope::Crate), ["usize"]);
        assert_eq!(rules("enum E { A(usize) }", Scope::Crate), ["usize"], "variant fields too");
        assert_eq!(rules("pub const N: usize = 1;", Scope::Crate), ["usize"]);
        assert_eq!(rules("type I = usize;", Scope::Crate), ["usize"]);
        assert_eq!(rules("pub fn a(n: usize) {}", Scope::IndexModule), NONE);
        assert_eq!(rules("fn a(n: usize) {}", Scope::IndexModule), NONE);
        assert_eq!(rules("const N: usize = 1;", Scope::Crate), NONE);
        assert_eq!(rules("pub fn a<const N: usize>() {}", Scope::Crate), NONE);
        assert_eq!(rules("fn a() { let _n: usize = 1; }", Scope::Crate), NONE, "not a signature");
        assert_eq!(rules("pub fn a(n: usize) {}", Scope::Tooling), NONE);
        assert_eq!(rules("fn a(n: usize) {}", Scope::Tooling), NONE);
    }

    // Goal: a trait dictates the signatures of its impls, so those are exempt from the `usize`
    // rule. The trait's own declaration, inherent methods and functions nested in an impl are
    // not.
    #[test]
    fn usize_rule_exempts_trait_impls_only() {
        assert_eq!(rules("impl T for S { fn a(&self, n: usize) {} }", Scope::Crate), NONE);
        assert_eq!(rules("impl S { fn a(&self, n: usize) {} }", Scope::Crate), ["usize"]);
        assert_eq!(rules("trait T { fn a(&self, n: usize); }", Scope::Crate), ["usize"]);
        assert_eq!(rules("trait T { fn a(&self, n: usize) {} }", Scope::Crate), ["usize"]);
        let nested = "impl T for S { fn a(&self) { fn b(n: usize) {} } }";
        assert_eq!(rules(nested, Scope::Crate), ["usize"], "a nested function is not dictated");
        let inherent_inside = "impl T for S { fn a(&self) { impl R { fn b(n: usize) {} } } }";
        assert_eq!(rules(inherent_inside, Scope::Crate), ["usize"], "nor a nested inherent impl");
        let after = "impl T for S { fn a(&self) {} }\nimpl S { fn b(n: usize) {} }";
        assert_eq!(rules(after, Scope::Crate), ["usize"], "the exemption ends with the impl");
    }

    // Goal: `Default` derives and impls are rejected in nami crates.
    #[test]
    fn default_rule_fires() {
        assert_eq!(rules("#[derive(Debug, Default)]\nstruct S;", Scope::Crate), ["default"]);
        assert_eq!(rules("impl Default for S {}", Scope::Crate), ["default"]);
        assert_eq!(rules("#[derive(Default)]\nstruct S;", Scope::Tooling), NONE);
    }

    // Goal: `Vec` is rejected anywhere inside a field type, in structs, tuple structs and enum
    // variants. Types that merely contain the letters are not `Vec`.
    #[test]
    fn vec_field_rule_fires() {
        assert_eq!(rules("struct S { v: Vec<f64> }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S { v: Option<Vec<f64>> }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S { v: [Vec<f64>; 2] }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S { v: Box<[Vec<f64>]> }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S { v: (u32, std::vec::Vec<f64>) }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S(Vec<f64>);", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("enum E { A(Vec<f64>) }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("enum E { A { v: Option<Vec<f64>> } }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S { v: Box<[f64]> }", Scope::Crate), NONE);
        assert_eq!(rules("struct S { v: Option<Box<[f64]>> }", Scope::Crate), NONE);
        assert_eq!(rules("struct S { v: VecDeque<f64> }", Scope::Crate), NONE);
        assert_eq!(rules("struct S { v: Vec<f64> }", Scope::Tooling), NONE);
    }

    // Goal: kernels reject `dyn` and function pointers, outside tests only.
    #[test]
    fn kernel_indirection_rule_fires() {
        assert_eq!(rules("fn a(f: &dyn Fn()) {}", Scope::Kernels), ["kernel-indirection"]);
        assert_eq!(rules("fn a(f: fn(f64) -> f64) {}", Scope::Kernels), ["kernel-indirection"]);
        assert_eq!(rules("fn a(f: &dyn Fn()) {}", Scope::Crate), NONE);
        let in_tests = "#[cfg(test)]\nmod tests { fn a(f: &dyn Fn()) {} }";
        assert_eq!(rules(in_tests, Scope::Kernels), NONE);
    }

    // Goal: `unsafe` is rejected everywhere except in the allocation guard crate.
    #[test]
    fn unsafe_rule_fires() {
        assert_eq!(rules("fn a() { unsafe {} }", Scope::Tooling), ["unsafe"]);
        assert_eq!(rules("unsafe fn a() {}", Scope::Crate), ["unsafe"]);
        assert_eq!(rules("unsafe impl Send for S {}", Scope::Crate), ["unsafe"]);
        assert_eq!(rules("unsafe trait T {}", Scope::Crate), ["unsafe"]);
        assert_eq!(rules("trait T { unsafe fn a(); }", Scope::Crate), ["unsafe"]);
        assert_eq!(rules_at("crates/alloc-guard/src/lib.rs", "unsafe fn a() {}"), NONE);
    }

    // Goal: every kind of function body is visited, so its rules apply.
    #[test]
    fn all_function_kinds_are_visited() {
        assert_eq!(rules("impl S { fn a() { loop {} } }", Scope::Tooling), ["unbounded-loop"]);
        assert_eq!(rules("trait T { fn a() { loop {} } }", Scope::Tooling), ["unbounded-loop"]);
        assert_eq!(rules("fn a() { let _f = async {}; }", Scope::Tooling), ["async"]);
        assert_eq!(rules("pub static N: usize = 1;", Scope::Crate), ["usize"]);
        let enum_default = "#[derive(Default)]\nenum E { #[default] A }";
        assert_eq!(rules(enum_default, Scope::Crate), ["default"]);
    }

    // Goal: only `#[cfg(test)]` modules are exempt, and the exemption ends with the module.
    #[test]
    fn test_module_exemption_is_scoped() {
        let after_tests = "#[cfg(test)]\nmod t {}\nfn a(f: &dyn Fn()) {}";
        assert_eq!(rules(after_tests, Scope::Kernels), ["kernel-indirection"]);
        let other_cfg = "#[cfg(unix)]\nmod m { fn a(f: &dyn Fn()) {} }";
        assert_eq!(rules(other_cfg, Scope::Kernels), ["kernel-indirection"]);
    }

    // Goal: the density threshold is exactly two assertions per function.
    #[test]
    fn density_threshold_is_two() {
        assert!(AssertionStats { functions: 2, assertions: 4 }.meets_density(), "4 / 2 meets");
        assert!(!AssertionStats { functions: 2, assertions: 3 }.meets_density(), "3 / 2 fails");
    }

    // Goal: `#[ignore]` needs a reason.
    #[test]
    fn ignore_reason_rule_fires() {
        assert_eq!(rules("#[ignore]\nfn a() {}", Scope::Tooling), ["ignore-reason"]);
        assert_eq!(rules("#[ignore = \"slow\"]\nfn a() {}", Scope::Tooling), NONE);
    }

    // Goal: a small float literal outside a constant is a magic tolerance, in plain code and
    // in macro arguments; the threshold is exactly 1e-6, and zero and larger values pass.
    #[test]
    fn magic_tolerance_rule_fires() {
        let fires = [
            "fn a(rho: f64) -> f64 { rho.max(1.0e-12) }",
            "fn a(x: f64) -> bool { x < 1e-9 }",
            "fn a(x: f64) -> bool { x > -1.0e-12 }",
            "fn a(x: f64) -> bool { x < 1.0e-12_f64 }",
            "fn a(x: f64) -> bool { x < 0.000_000_5 }",
            "fn a(x: f64) -> bool { x < 1.0e-6 }",
            "fn a(x: f64) { assert!(x > 1.0e-12, \"positive\"); }",
            "fn a(x: f64) { assert!(f(x, [1.0e-12]), \"nested in a macro\"); }",
            "static FLOOR: f64 = 1.0e-12;",
            "fn a() -> f64 { const { 1.0e-12 } }",
        ];
        for text in fires {
            assert_eq!(rules_in_crate(text), ["magic-tolerance"], "{text}");
        }
        let passes = [
            "fn a(x: f64) -> bool { x < 1.0e-5 }",
            "fn a(x: f64) -> bool { x < 1.000_001e-6 }",
            "fn a(x: f64) -> bool { x > 0.0 && x < 1.0 }",
            "fn a(x: f64) -> bool { x < 1.0e12 }",
            "fn a(x: u32) -> bool { x < 1 }",
            "fn a(x: f64) { assert!(x > 0.5, \"large\"); }",
        ];
        for text in passes {
            assert_eq!(rules_in_crate(text), NONE, "{text}");
        }
    }

    // Goal: a tolerance is allowed in the initialiser of a `const` item, and that constant
    // must have a doc comment. `#[doc(hidden)]` is not one.
    #[test]
    fn tolerances_live_in_documented_constants() {
        let documented = "/// Relative change below round-off.\nconst TOLERANCE: f64 = 1.0e-12;";
        assert_eq!(rules_in_crate(documented), NONE);
        let derived = "/// Twice the round-off.\nconst TOLERANCE: f64 = 2.0 * (1.0e-12);";
        assert_eq!(rules_in_crate(derived), NONE);
        let associated = "impl S {\n    /// Floor of the scheme.\n    const FLOOR: f64 = 1e-9;\n}";
        assert_eq!(rules_in_crate(associated), NONE);
        let in_trait = "trait T {\n    /// Floor of the scheme.\n    const FLOOR: f64 = 1e-9;\n}";
        assert_eq!(rules_in_crate(in_trait), NONE);
        assert_eq!(rules_in_crate("const TOLERANCE: f64 = 1.0e-12;"), ["magic-tolerance"]);
        assert_eq!(rules_in_crate("impl S { const FLOOR: f64 = 1e-9; }"), ["magic-tolerance"]);
        assert_eq!(rules_in_crate("trait T { const FLOOR: f64 = 1e-9; }"), ["magic-tolerance"]);
        let hidden = "#[doc(hidden)]\nconst TOLERANCE: f64 = 1.0e-12;";
        assert_eq!(rules_in_crate(hidden), ["magic-tolerance"]);
        let large = "const CFL: f64 = 0.5;";
        assert_eq!(rules_in_crate(large), NONE, "a constant without a tolerance needs no doc");
        let after = "/// Documented.\nconst A: f64 = 1e-9;\nfn a(x: f64) -> bool { x < 1e-9 }";
        assert_eq!(rules_in_crate(after), ["magic-tolerance"], "the exemption ends with the item");
        let nested = "/// Outer.\nconst A: f64 = {\n    const B: f64 = 1e-9;\n    B\n};";
        assert_eq!(rules_in_crate(nested), ["magic-tolerance"], "the inner constant needs a doc");
    }

    // Goal: test code and tooling are exempt from the tolerance rule: `#[cfg(test)]` modules,
    // `#[test]` functions and files under `tests/`. The exemption ends with the test.
    #[test]
    fn tolerance_rule_exempts_test_code() {
        let literal = "fn a(x: f64) -> bool { x < 1e-9 }";
        assert_eq!(rules_in_crate(&format!("#[cfg(test)]\nmod tests {{ {literal} }}")), NONE);
        assert_eq!(rules_in_crate("#[test]\nfn a() { let _x = 1e-9; }"), NONE);
        assert_eq!(rules_at("crates/solver/tests/sod.rs", literal), NONE);
        assert_eq!(rules_at("xtask/src/example.rs", literal), NONE);
        let after = format!("#[test]\nfn t() {{}}\n{literal}");
        assert_eq!(rules_in_crate(&after), ["magic-tolerance"]);
        assert_eq!(rules_at("crates/solver/src/tests_helper.rs", literal), ["magic-tolerance"]);
    }

    // Goal: every way of naming `libm` is rejected outside `kernels::math`: a call, a type, a
    // `use` in each form, an `extern crate`, and a call inside macro arguments.
    #[test]
    fn direct_libm_ban_fires() {
        let fires = [
            "fn a(x: f64) -> f64 { libm::sqrt(x) }",
            "fn a(x: f64) -> f64 { ::libm::sqrt(x) }",
            "fn a(x: libm::Libm<f64>) {}",
            "use libm::sqrt;",
            "use libm;",
            "use libm as m;",
            "use ::libm::sqrt;",
            "use {core::f64::consts::PI, libm::sqrt};",
            "extern crate libm;",
            "fn a(x: f64) { assert!(libm::sqrt(x) >= 0.0, \"non-negative\"); }",
            "fn a(x: f64) -> f64 { f(&[libm::sqrt(x)]) }",
            "fn a(x: f64) -> f64 { g!(1.0, (h![libm::cbrt(x)])) }",
        ];
        for text in fires {
            assert_eq!(rules_in_crate(text), ["direct-libm"], "{text}");
            assert_eq!(rules_at("crates/kernels/src/reduce.rs", text), ["direct-libm"], "{text}");
            assert_eq!(rules_at("xtask/src/example.rs", text), ["direct-libm"], "{text}");
            assert_eq!(rules_at("crates/kernels/src/math.rs", text), NONE, "{text}");
        }
    }

    // Goal: the ban is on paths that start with `libm`, not on the letters.
    #[test]
    fn direct_libm_ban_is_exact() {
        let passes = [
            "fn a(x: f64) -> f64 { kernels::math::sqrt(x) }",
            "fn a(x: f64) -> f64 { crate::libm::sqrt(x) }",
            "fn a(libm: f64) -> f64 { libm + 1.0 }",
            "fn a(x: f64) -> f64 { libm_like::sqrt(x) }",
            "use kernels::math::sqrt;",
            "fn a(x: f64) { assert!(math::sqrt(x) >= 0.0, \"libm::sqrt is wrapped\"); }",
        ];
        for text in passes {
            assert_eq!(rules_in_crate(text), NONE, "{text}");
        }
    }

    // Goal: the `debug_assert!` family is rejected everywhere outside `crates/kernels`,
    // including in tests and nested inside another macro.
    #[test]
    fn debug_assert_ban_fires() {
        let fires = [
            "fn a(x: f64) { debug_assert!(x > 0.0, \"positive\"); }",
            "fn a(x: u32) { debug_assert_eq!(x, 1, \"one\"); }",
            "fn a(x: u32) { debug_assert_ne!(x, 1, \"not one\"); }",
            "fn a(x: f64) { core::debug_assert!(x > 0.0, \"positive\"); }",
            "fn a(x: f64) { assert!({ debug_assert!(x > 0.0, \"nested\"); true }, \"outer\"); }",
            "#[cfg(test)]\nmod tests { fn a(x: f64) { debug_assert!(x > 0.0, \"positive\"); } }",
        ];
        for text in fires {
            assert_eq!(rules_in_crate(text), ["debug-assert-placement"], "{text}");
            assert_eq!(rules_at("xtask/src/example.rs", text), ["debug-assert-placement"]);
            assert_eq!(rules_at("crates/kernels/src/update.rs", text), NONE, "{text}");
        }
        let passes = "fn a(x: f64) { assert!(x > 0.0, \"debug_assert! is for kernels\"); }";
        assert_eq!(rules_in_crate(passes), NONE);
    }
}
