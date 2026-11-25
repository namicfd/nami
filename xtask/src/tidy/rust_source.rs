//! Rules that need the syntax tree, parsed with `syn`.
//!
//! The visitor walks the syntax tree recursively through `syn::visit`. That is the tooling
//! exception to the no-recursion rule: the depth is bounded by the nesting of one source file,
//! and the rule's checker (dylint, ADR-0006) applies to nami's crates, not to `xtask`.

use super::{Scope, Violation};
use proc_macro2::Span;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Block, Signature, Type, Visibility};

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
        violations: Vec::new(),
        stats: AssertionStats::zero(),
    };
    visitor.visit_file(&file);
    Report { violations: visitor.violations, stats: visitor.stats }
}

struct Visitor<'a> {
    path: &'a Path,
    scope: Scope,
    /// Depth of `#[cfg(test)]` modules around the current item.
    test_depth: u32,
    violations: Vec<Violation>,
    stats: AssertionStats,
}

/// The parts of any function item that the rules look at.
struct Function<'a> {
    attributes: &'a [Attribute],
    signature: &'a Signature,
    block: &'a Block,
    public: bool,
}

impl Visitor<'_> {
    fn report(&mut self, span: Span, rule: &'static str, message: impl Into<String>) {
        self.violations.push(Violation::new(self.path, line_of(span), rule, message));
    }

    fn in_tests(&self) -> bool {
        self.test_depth > 0
    }

    fn usize_restricted(&self) -> bool {
        self.scope.is_nami() && self.scope != Scope::IndexModule
    }

    fn check_function(&mut self, function: &Function<'_>) {
        let name = &function.signature.ident;
        let first = line_of(function.signature.fn_token.span);
        let last = line_of_end(function.block.brace_token.span.close());
        let lines = last - first + 1;
        if lines > FUNCTION_LINES_MAX {
            let message = format!(
                "function `{name}` has {lines} lines; the limit is 70. Split it: push ifs up and \
                 fors down, keep leaf functions pure (TIGER_STYLE.md)"
            );
            self.report(function.signature.fn_token.span, "function-length", message);
        }
        if let syn::Safety::Unsafe(unsafety) = &function.signature.safety {
            self.report_unsafe(unsafety.span);
        }
        if let Some(asyncness) = &function.signature.asyncness {
            let message =
                "async functions are banned; functions run to completion (TIGER_STYLE.md)";
            self.report(asyncness.span, "async", message);
        }
        if function.public
            && self.usize_restricted()
            && signature_mentions_usize(function.signature)
        {
            let message = format!(
                "`usize` in the signature of public function `{name}`; use a typed index or count \
                 such as `CellIndex` or `CellCount` (ADR-0002)"
            );
            self.report(function.signature.fn_token.span, "usize", message);
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
            let message = "`usize` in a struct field; use a typed index or count (ADR-0002)";
            self.report(field_type.span(), "usize", message);
        }
        if self.scope.is_nami() && is_vec(field_type) {
            let message = "`Vec` in a struct field; storage that must not grow is `Box<[T]>`, \
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
        let public = is_public(&node.vis);
        let function =
            Function { attributes: &node.attrs, signature: &node.sig, block: &node.block, public };
        self.check_function(&function);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let public = is_public(&node.vis);
        let function =
            Function { attributes: &node.attrs, signature: &node.sig, block: &node.block, public };
        self.check_function(&function);
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if let Some(block) = &node.default {
            let function =
                Function { attributes: &node.attrs, signature: &node.sig, block, public: false };
            self.check_function(&function);
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

    fn visit_expr_async(&mut self, node: &'ast syn::ExprAsync) {
        let message = "async blocks are banned; functions run to completion (TIGER_STYLE.md)";
        self.report(node.async_token.span, "async", message);
        visit::visit_expr_async(self, node);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.check_derives(&node.attrs);
        for field in &node.fields {
            self.check_field_type(&field.ty);
        }
        visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.check_derives(&node.attrs);
        visit::visit_item_enum(self, node);
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
        visit::visit_item_impl(self, node);
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.check_public_item_type(&node.vis, &node.ty);
        visit::visit_item_const(self, node);
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

/// Finds `usize` anywhere inside a type.
struct UsizeFinder {
    found: bool,
}

impl<'ast> Visit<'ast> for UsizeFinder {
    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        self.found |= node.path.is_ident("usize");
        visit::visit_type_path(self, node);
    }
}

fn type_mentions_usize(node: &Type) -> bool {
    let mut finder = UsizeFinder { found: false };
    finder.visit_type(node);
    finder.found
}

/// Checks parameters and the return type. Const generic parameters may be `usize`, because
/// array lengths have to be.
fn signature_mentions_usize(signature: &Signature) -> bool {
    let mut finder = UsizeFinder { found: false };
    for input in &signature.inputs {
        finder.visit_fn_arg(input);
    }
    finder.visit_return_type(&signature.output);
    finder.found
}

fn is_vec(node: &Type) -> bool {
    matches!(node, Type::Path(path) if last_segment_is(&path.path, "Vec"))
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

    fn rules(text: &str, scope: Scope) -> Vec<&'static str> {
        let report = check(Path::new("fixture.rs"), text, scope);
        report.violations.iter().map(|violation| violation.rule).collect()
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
        assert_eq!(rules(&at_limit, Scope::Tooling), Vec::<&str>::new());
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
        assert_eq!(rules(&function_with_lines("a", 9, 0), Scope::Tooling), Vec::<&str>::new());
    }

    // Goal: `loop`, `while` and `async` are rejected everywhere.
    #[test]
    fn control_flow_rules_fire() {
        assert_eq!(rules("fn a() { loop {} }", Scope::Tooling), ["unbounded-loop"]);
        assert_eq!(rules("fn a() { while true {} }", Scope::Tooling), ["unbounded-loop"]);
        assert_eq!(rules("async fn a() {}", Scope::Tooling), ["async"]);
        assert_eq!(rules("fn a() { for _ in 0..3 {} }", Scope::Tooling), Vec::<&str>::new());
    }

    // Goal: `usize` is rejected in public signatures, fields and public items, except in the
    // index module; private helpers and const generics stay allowed.
    #[test]
    fn usize_rule_fires() {
        assert_eq!(rules("pub fn a(n: usize) {}", Scope::Crate), ["usize"]);
        assert_eq!(rules("struct S { n: usize }", Scope::Crate), ["usize"]);
        assert_eq!(rules("pub const N: usize = 1;", Scope::Crate), ["usize"]);
        assert_eq!(rules("type I = usize;", Scope::Crate), ["usize"]);
        assert_eq!(rules("pub fn a(n: usize) {}", Scope::IndexModule), Vec::<&str>::new());
        assert_eq!(rules("fn a(n: usize) {}", Scope::Crate), Vec::<&str>::new());
        assert_eq!(rules("const N: usize = 1;", Scope::Crate), Vec::<&str>::new());
        assert_eq!(rules("pub fn a<const N: usize>() {}", Scope::Crate), Vec::<&str>::new());
        assert_eq!(rules("pub fn a(n: usize) {}", Scope::Tooling), Vec::<&str>::new());
    }

    // Goal: `Default` derives and impls, and `Vec` fields, are rejected in nami crates.
    #[test]
    fn default_and_vec_rules_fire() {
        assert_eq!(rules("#[derive(Debug, Default)]\nstruct S;", Scope::Crate), ["default"]);
        assert_eq!(rules("impl Default for S {}", Scope::Crate), ["default"]);
        assert_eq!(rules("struct S { v: Vec<f64> }", Scope::Crate), ["vec-field"]);
        assert_eq!(rules("struct S { v: Box<[f64]> }", Scope::Crate), Vec::<&str>::new());
        assert_eq!(rules("#[derive(Default)]\nstruct S;", Scope::Tooling), Vec::<&str>::new());
    }

    // Goal: kernels reject `dyn` and function pointers, outside tests only.
    #[test]
    fn kernel_indirection_rule_fires() {
        assert_eq!(rules("fn a(f: &dyn Fn()) {}", Scope::Kernels), ["kernel-indirection"]);
        assert_eq!(rules("fn a(f: fn(f64) -> f64) {}", Scope::Kernels), ["kernel-indirection"]);
        assert_eq!(rules("fn a(f: &dyn Fn()) {}", Scope::Crate), Vec::<&str>::new());
        let in_tests = "#[cfg(test)]\nmod tests { fn a(f: &dyn Fn()) {} }";
        assert_eq!(rules(in_tests, Scope::Kernels), Vec::<&str>::new());
    }

    // Goal: `unsafe` is rejected everywhere except in the allocation guard crate.
    #[test]
    fn unsafe_rule_fires() {
        assert_eq!(rules("fn a() { unsafe {} }", Scope::Tooling), ["unsafe"]);
        assert_eq!(rules("unsafe fn a() {}", Scope::Crate), ["unsafe"]);
        assert_eq!(rules("unsafe impl Send for S {}", Scope::Crate), ["unsafe"]);
        assert_eq!(rules("unsafe trait T {}", Scope::Crate), ["unsafe"]);
        let guard =
            check(Path::new("crates/alloc-guard/src/lib.rs"), "unsafe fn a() {}", Scope::Crate);
        assert_eq!(guard.violations, Vec::new());
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
        assert_eq!(rules("#[ignore = \"slow\"]\nfn a() {}", Scope::Tooling), Vec::<&str>::new());
    }
}
