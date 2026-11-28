//! The assertion counts behind `assertion-density` (ADR-0004), and the count of assertions in
//! one body that `unasserted-function` uses.
//!
//! This is trusted core, not a rule path (ADR-0022). `assertion-density` is a workspace rule
//! with no fixture, so the ratchet could not prove that a change to its threshold, its cut-off
//! or its counting only tightens. The density walk is its own
//! visitor, so that the source rules in `rust_source.rs` cannot change which functions it
//! counts. A test in `tidy.rs` keeps the two walks' test-code exemptions in step.

use super::Scope;
use proc_macro2::Span;
use syn::visit::{self, Visit};
use syn::{Attribute, Block};

/// Functions up to this many lines are trivial and excluded from the density average.
const FUNCTION_LINES_TRIVIAL_MAX: u32 = 5;

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

/// The number of assertion macros in a function body, at any depth.
#[must_use]
pub fn count_in(block: &Block) -> u32 {
    let mut counter = Counter { count: 0 };
    counter.visit_block(block);
    counter.count
}

/// The density counts of one file: every function with a body in a nami crate that is longer
/// than [`FUNCTION_LINES_TRIVIAL_MAX`] lines, outside `#[cfg(test)]` modules and `#[test]`
/// functions. A file that does not parse counts as nothing; the `parse` rule reports it.
#[must_use]
pub fn stats(text: &str, scope: Scope) -> AssertionStats {
    if !scope.is_nami() {
        return AssertionStats::zero();
    }
    let Ok(file) = syn::parse_file(text) else {
        return AssertionStats::zero();
    };
    let mut walk = Density { test_depth: 0, stats: AssertionStats::zero() };
    walk.visit_file(&file);
    assert_eq!(walk.test_depth, 0, "every test module that was entered was left");
    walk.stats
}

/// Counts assertion macros inside one function body.
struct Counter {
    count: u32,
}

impl<'ast> Visit<'ast> for Counter {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let name = node.path.segments.last().map(|segment| segment.ident.to_string());
        if name.is_some_and(|name| ASSERTION_MACROS.contains(&name.as_str())) {
            self.count += 1;
        }
        visit::visit_macro(self, node);
    }
}

/// Walks the functions of one file for the density counts.
struct Density {
    /// Depth of `#[cfg(test)]` modules around the current item.
    test_depth: u32,
    stats: AssertionStats,
}

impl Density {
    fn function(&mut self, attributes: &[Attribute], fn_token: Span, block: &Block) {
        let is_test = attributes.iter().any(|attribute| attribute.path().is_ident("test"));
        if self.test_depth > 0 || is_test {
            return;
        }
        let first = line(fn_token.start().line);
        let last = line(block.brace_token.span.close().end().line);
        assert!(last >= first, "a function ends after it starts");
        if last - first + 1 > FUNCTION_LINES_TRIVIAL_MAX {
            self.stats.add(AssertionStats { functions: 1, assertions: count_in(block) });
        }
    }
}

impl<'ast> Visit<'ast> for Density {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let is_test_module = is_cfg_test(&node.attrs);
        self.test_depth += u32::from(is_test_module);
        visit::visit_item_mod(self, node);
        self.test_depth -= u32::from(is_test_module);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.function(&node.attrs, node.sig.fn_token.span, &node.block);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.function(&node.attrs, node.sig.fn_token.span, &node.block);
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if let Some(block) = &node.default {
            self.function(&node.attrs, node.sig.fn_token.span, block);
        }
        visit::visit_trait_item_fn(self, node);
    }
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

fn line(line: usize) -> u32 {
    u32::try_from(line).expect("source files have fewer than 2^32 lines")
}

#[cfg(test)]
mod tests {
    use super::{AssertionStats, Scope, stats};

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

    // Goal: density counts only non-trivial functions in nami crates, with every assertion
    // macro, and skips test modules, test functions and tooling.
    #[test]
    fn density_counts_non_trivial_functions() {
        assert_eq!(stats(&function_with_lines("a", 3, 0), Scope::Crate), AssertionStats::zero());
        assert_eq!(stats(&function_with_lines("a", 4, 0), Scope::Crate).functions, 1, "6 lines");
        let dense = stats(&function_with_lines("a", 8, 2), Scope::Crate);
        assert_eq!(dense, AssertionStats { functions: 1, assertions: 2 });
        assert!(dense.meets_density(), "two assertions meet the density");
        let macros = "fn a() {\n assert_eq!(1, 1);\n assert_ne!(1, 2);\n debug_assert!(true);\n \
                      debug_assert_eq!(1, 1);\n debug_assert_ne!(1, 2);\n println!(\"x\");\n}\n";
        assert_eq!(stats(macros, Scope::Kernels).assertions, 5, "println! is no assertion");
        let tests = format!(
            "#[cfg(test)]\nmod tests {{ {} }}\n#[test]\n{}impl A {{ {} }}\ntrait T {{ {} }}",
            function_with_lines("a", 8, 0),
            function_with_lines("b", 8, 0),
            function_with_lines("c", 8, 1),
            function_with_lines("d", 8, 1),
        );
        assert_eq!(stats(&tests, Scope::Crate), AssertionStats { functions: 2, assertions: 2 });
        let modules = format!(
            "mod inner {{ {} }}\n#[cfg(unix)]\nmod unix {{ {} }}",
            function_with_lines("e", 8, 1),
            function_with_lines("f", 8, 0),
        );
        let counted = stats(&modules, Scope::Crate);
        assert_eq!(counted, AssertionStats { functions: 2, assertions: 1 }, "other modules count");
        assert_eq!(stats(&function_with_lines("a", 8, 0), Scope::Tooling), AssertionStats::zero());
        assert_eq!(stats("fn a( {", Scope::Crate), AssertionStats::zero(), "does not parse");
    }

    // Goal: the density threshold is exactly two assertions per function, and counts add up.
    #[test]
    fn density_threshold_is_two() {
        assert!(AssertionStats { functions: 2, assertions: 4 }.meets_density(), "4 / 2 meets");
        assert!(!AssertionStats { functions: 2, assertions: 3 }.meets_density(), "3 / 2 fails");
        let mut sum = AssertionStats { functions: 1, assertions: 1 };
        sum.add(AssertionStats { functions: 2, assertions: 5 });
        assert_eq!(sum, AssertionStats { functions: 3, assertions: 6 });
    }
}
