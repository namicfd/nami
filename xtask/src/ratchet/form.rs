//! The forms the `syn` checks can read soundly (review of #57, thread 1).
//!
//! The rule-set check reads the head's registry and the `check_*` call sites of its rule
//! modules as data. That is sound only if the compiled code is the code that was read.
//!
//! - A `#[cfg]` or `#[cfg_attr]` can remove a table or a call that the reader still sees.
//! - An item macro or `macro_rules!` can generate one that the reader never sees.
//! - A second item named like a table can shadow the real one.
//!
//! So in the head's rule-path Rust files these forms give no verdict:
//!
//! - any `cfg` or `cfg_attr` attribute other than `#[cfg(test)]`;
//! - any item macro, `macro_rules!` included;
//! - any other macro than the expression macros of [`EXPRESSION_MACROS`];
//! - any item, import or binding named `RULES`, `PATH_BANS` or `MACRO_BANS`, except the
//!   registry's own top-level tables.
//!
//! The lint library declares its lints with item macros, so only the `cfg` rule applies to it.

use std::fs;
use std::path::Path;
use syn::visit::{self, Visit};

/// The names of the registry's tables.
pub const TABLES: [&str; 3] = ["RULES", "PATH_BANS", "MACRO_BANS"];

/// The standard expression macros that the rule modules use. They take expressions and cannot
/// define an item that outlives them.
const EXPRESSION_MACROS: [&str; 14] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "concat",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "format",
    "matches",
    "panic",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// What a file may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strictness {
    /// The registry: tables only at the top, no macros beyond expression macros, no `cfg`.
    Registry,
    /// A rule module: no tables, no macros beyond expression macros, no `cfg`.
    RuleModule,
    /// The lint library: no `cfg`; its lints are declared with item macros.
    LintLibrary,
}

/// The first form in `text` that the readers cannot vouch for, if any.
///
/// # Errors
///
/// Returns the problem: a file that does not parse, or a form listed in the module docs.
pub fn check(relative: &str, text: &str, strictness: Strictness) -> Result<(), String> {
    let file =
        syn::parse_file(text).map_err(|error| format!("{relative} does not parse: {error}"))?;
    let mut finder = Finder { strictness, depth: 0, problem: None };
    finder.visit_file(&file);
    match finder.problem {
        Some(problem) => Err(format!(
            "{relative} holds {problem}, which the ratchet cannot read soundly from source; the \
             rule-set check gives no verdict, so the code owner decides (review of #57)"
        )),
        None => Ok(()),
    }
}

/// Checks the forms of every head rule-path Rust file that the `syn` readers use.
///
/// # Errors
///
/// No verdict: a file that cannot be read, or a form that cannot be read soundly.
pub fn check_head(head: &Path) -> Result<(), String> {
    let mut files: Vec<(String, Strictness)> =
        vec![(super::source::REGISTRY.to_owned(), Strictness::Registry)];
    files.extend(
        super::source::RULE_MODULES
            .iter()
            .map(|module| ((*module).to_owned(), Strictness::RuleModule)),
    );
    let lint_sources = head.join("lints/src");
    if lint_sources.is_dir() {
        let entries = fs::read_dir(&lint_sources)
            .map_err(|error| format!("cannot read lints/src: {error}"))?;
        for entry in entries {
            let name =
                entry.map_err(|error| format!("cannot read lints/src: {error}"))?.file_name();
            let name = name.to_string_lossy();
            if Path::new(name.as_ref()).extension().is_some_and(|extension| extension == "rs") {
                files.push((format!("lints/src/{name}"), Strictness::LintLibrary));
            }
        }
    }
    for (relative, strictness) in files {
        if let Some(text) = super::source::read_optional(head, &relative)? {
            check(&relative, &text, strictness)?;
        }
    }
    Ok(())
}

struct Finder {
    strictness: Strictness,
    /// Depth of items around the current one; 0 at the top of the file.
    depth: u32,
    problem: Option<String>,
}

impl Finder {
    fn flag(&mut self, problem: String) {
        if self.problem.is_none() {
            self.problem = Some(problem);
        }
    }

    fn name(&mut self, ident: &syn::Ident, what: &str) {
        let is_table = TABLES.iter().any(|table| ident == table);
        let allowed =
            self.strictness == Strictness::Registry && self.depth == 1 && what == "a constant";
        if is_table && !allowed && self.strictness != Strictness::LintLibrary {
            self.flag(format!("{what} named `{ident}`, which could shadow the registry's table"));
        }
    }

    fn mac(&mut self, mac: &syn::Macro) {
        if self.strictness == Strictness::LintLibrary {
            return;
        }
        let name = mac.path.segments.last().map_or_default(|segment| segment.ident.to_string());
        let is_expression =
            mac.path.segments.len() == 1 && EXPRESSION_MACROS.contains(&name.as_str());
        if !is_expression {
            self.flag(format!(
                "the macro `{name}!`, which could generate what the reader cannot see"
            ));
        }
    }
}

/// Upper bound on the subtrees of one `use` item, so the walk is bounded.
const USE_TREES_MAX: u32 = 10_000;

impl Finder {
    /// Flags glob imports, imports renamed to a table's name, and a table imported from
    /// anywhere but the registry.
    fn imports(&mut self, tree: &syn::UseTree) {
        let mut pending: Vec<(Vec<String>, &syn::UseTree)> = vec![(Vec::new(), tree)];
        for _ in 0..USE_TREES_MAX {
            let Some((prefix, next)) = pending.pop() else {
                return;
            };
            match next {
                syn::UseTree::Path(path) => {
                    let mut longer = prefix.clone();
                    longer.push(path.ident.to_string());
                    pending.push((longer, &path.tree));
                }
                syn::UseTree::Group(group) => {
                    pending.extend(group.items.iter().map(|item| (prefix.clone(), item)));
                }
                syn::UseTree::Glob(_) => self.flag("a glob import".to_owned()),
                syn::UseTree::Rename(rename) => self.name(&rename.rename, "an import"),
                syn::UseTree::Name(name) => self.imported(&prefix, &name.ident),
            }
        }
        panic!("a `use` item has more than {USE_TREES_MAX} subtrees");
    }
}

impl Finder {
    /// A name imported from `prefix`: a table name counts unless it comes from the registry.
    fn imported(&mut self, prefix: &[String], ident: &syn::Ident) {
        let from_registry = prefix.last().is_some_and(|last| last == "registry")
            || (self.strictness == Strictness::Registry && prefix == ["super"]);
        if !from_registry {
            self.name(ident, "an import");
        }
    }
}

/// True for exactly `#[cfg(test)]`.
fn is_cfg_test(attribute: &syn::Attribute) -> bool {
    let mut is_test = false;
    let parsed = attribute.parse_nested_meta(|meta| {
        is_test = meta.path.is_ident("test") && meta.input.is_empty();
        Ok(())
    });
    parsed.is_ok() && is_test
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        let path = node.path();
        if path.is_ident("cfg_attr") || (path.is_ident("cfg") && !is_cfg_test(node)) {
            self.flag("a `cfg` or `cfg_attr` attribute other than `#[cfg(test)]`".to_owned());
        }
        visit::visit_attribute(self, node);
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        self.depth += 1;
        if let syn::Item::Macro(item) = node
            && self.strictness != Strictness::LintLibrary
        {
            let name = item.mac.path.segments.last().map(|segment| segment.ident.to_string());
            self.flag(format!("the item macro `{}!`", name.unwrap_or_default()));
        }
        visit::visit_item(self, node);
        self.depth -= 1;
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.name(&node.ident, "a constant");
        visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.name(&node.ident, "a static");
        visit::visit_item_static(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        self.name(&node.ident, "an associated constant");
        visit::visit_impl_item_const(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if self.strictness != Strictness::LintLibrary {
            self.imports(&node.tree);
        }
        visit::visit_item_use(self, node);
    }

    fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
        self.name(&node.ident, "a binding");
        visit::visit_pat_ident(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.mac(node);
        visit::visit_macro(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::{Strictness, check};

    fn registry() -> String {
        std::fs::read_to_string(crate::workspace_root().join(super::super::source::REGISTRY))
            .unwrap()
    }

    // Goal: the real rule-path files are in a readable form, so the check never stops honest
    // PRs; every probe of the review gives no verdict.
    #[test]
    fn forms_that_hide_code_give_no_verdict() {
        let root = crate::workspace_root();
        assert_eq!(super::check_head(&root), Ok(()));
        let real = registry();
        let hidden = real.replacen("pub const RULES", "#[cfg(any())]\npub const RULES", 1);
        assert!(check("r.rs", &hidden, Strictness::Registry).unwrap_err().contains("`cfg`"));
        let generated = format!("{real}\nmacro_rules! live {{ () => {{}} }}\nlive!();\n");
        assert!(
            check("r.rs", &generated, Strictness::Registry).unwrap_err().contains("item macro")
        );
        let call = "fn check_body() { #[cfg(any())] self.check_float_equality(f); }";
        assert!(check("m.rs", call, Strictness::RuleModule).is_err(), "a call under cfg");
        let attr = "#[cfg_attr(test, allow(dead_code))]\nfn a() {}";
        assert!(check("m.rs", attr, Strictness::RuleModule).is_err(), "cfg_attr");
        let shadow = "fn a() { const PATH_BANS: &[u8] = &[]; }";
        assert!(check("m.rs", shadow, Strictness::RuleModule).unwrap_err().contains("`PATH_BANS`"));
        let nested = "mod m { pub const RULES: &[u8] = &[]; }";
        assert!(check("r.rs", nested, Strictness::Registry).is_err(), "a nested table");
        let import = "use other::TABLE as MACRO_BANS;";
        assert!(check("m.rs", import, Strictness::RuleModule).is_err(), "a renamed import");
        let elsewhere = "use crate::other::{PATH_BANS};";
        assert!(check("m.rs", elsewhere, Strictness::RuleModule).is_err(), "not the registry's");
        let glob = "use crate::other::*;";
        assert!(check("m.rs", glob, Strictness::RuleModule).unwrap_err().contains("glob"));
        let honest = "use super::registry::{self, MACRO_BANS, PATH_BANS};";
        assert_eq!(check("m.rs", honest, Strictness::RuleModule), Ok(()));
        let statement = "fn a() { tables!(); }";
        assert!(
            check("m.rs", statement, Strictness::RuleModule).unwrap_err().contains("`tables!`")
        );
        let fine = "#[cfg(test)]\nmod tests { fn a() { assert_eq!(format!(\"x\"), vec![1]); } }";
        assert_eq!(check("m.rs", fine, Strictness::RuleModule), Ok(()));
        let lints = "declare_lint! { pub A, Deny, \"a\" }\n#[cfg(any())]\nfn r() {}";
        assert!(check("l.rs", lints, Strictness::LintLibrary).is_err(), "cfg in the lint library");
        assert_eq!(
            check("l.rs", "declare_lint! { pub A, Deny, \"a\" }", Strictness::LintLibrary),
            Ok(())
        );
        assert!(check("m.rs", "fn a( {", Strictness::RuleModule).is_err(), "does not parse");
    }
}
