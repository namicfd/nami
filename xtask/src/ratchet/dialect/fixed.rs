//! The items of a rule module or of the lint library that must be token for token as in the
//! base.
//!
//! Mutation testing makes no mutant for a changed constant, and the corpus pins a limit only at
//! the values its cases happen to use. So every `const` and `static` item, wherever it is, must
//! be one of the base's; a changed or new one needs the code owner. The registry's tables are
//! the exception, because the rule-set check reads them as data and proves that they only
//! tighten ([`super::registry`]).
//!
//! Likewise, code that reads the environment, starts a process or touches files can behave
//! differently in CI than where the ratchet observes it. So every item or method whose tokens
//! name one of [`SENSITIVE`] must be one of the base's: the context that the rules load from the
//! tree today stays as it is, and a new read gives no verdict.

use super::{identifiers, is_test_module, outside, parse};
use proc_macro2::TokenStream;
use std::collections::BTreeMap;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

/// Names that reach the environment, processes or files: `std::env`, `env!`, `option_env!`,
/// `std::process`, `std::fs`, `std::io` and their usual items.
pub const SENSITIVE: [&str; 10] = [
    "env",
    "option_env",
    "process",
    "Command",
    "fs",
    "File",
    "OpenOptions",
    "io",
    "read_to_string",
    "read_dir",
];

/// Checks the head's `head_text` of `relative` against the base's `base_text`, if the base has
/// the file: its constants and statics, and its items that name [`SENSITIVE`] names, must all be
/// the base's.
///
/// # Errors
///
/// No verdict: a file that does not parse, or such an item that the base does not have.
pub fn check(relative: &str, base_text: Option<&str>, head_text: &str) -> Result<(), String> {
    let head = fixed_items(relative, head_text)?;
    let base = match base_text {
        Some(text) => fixed_items(relative, text)?,
        None => Items::default(),
    };
    let new = |after: &BTreeMap<String, String>, before: &BTreeMap<String, String>| {
        after.iter().find(|(text, _)| !before.contains_key(*text)).map(|(_, name)| name.clone())
    };
    if let Some(name) = new(&head.constants, &base.constants) {
        return Err(outside(
            relative,
            &format!(
                "a constant or static that is not the base's, `{name}`; mutation testing cannot \
                 prove a changed value"
            ),
        ));
    }
    if let Some(name) = new(&head.sensitive, &base.sensitive) {
        return Err(outside(
            relative,
            &format!(
                "code that reads the environment, a process or a file and is not the base's, in \
                 `{name}`"
            ),
        ));
    }
    Ok(())
}

/// The constants and statics of a file, and its items and methods that name a sensitive name,
/// each as normalised token text, outside the test module.
#[derive(Debug, Default)]
struct Items {
    /// Normalised text to name.
    constants: BTreeMap<String, String>,
    /// Normalised text to name.
    sensitive: BTreeMap<String, String>,
}

fn fixed_items(relative: &str, text: &str) -> Result<Items, String> {
    let file = parse(relative, text)?;
    let mut finder = Finder { text, items: Items::default(), problem: None };
    for item in &file.items {
        if matches!(item, syn::Item::Mod(module) if is_test_module(module)) {
            continue;
        }
        finder.visit_item(item);
        if !matches!(item, syn::Item::Impl(_)) {
            finder.sensitive(item.span(), &item_name(item));
        }
    }
    match finder.problem {
        Some(problem) => Err(format!("{relative}: {problem}")),
        None => Ok(finder.items),
    }
}

struct Finder<'t> {
    text: &'t str,
    items: Items,
    problem: Option<String>,
}

impl Finder<'_> {
    /// The source of `span`, as normalised token text.
    fn source(&mut self, span: proc_macro2::Span) -> Option<String> {
        let source = self.text.get(span.byte_range());
        let tokens = source.and_then(|source| source.parse::<TokenStream>().ok());
        if tokens.is_none() && self.problem.is_none() {
            self.problem = Some("an item's span lies outside its file".to_owned());
        }
        tokens.map(|tokens| tokens.to_string())
    }

    fn constant(&mut self, span: proc_macro2::Span, name: &syn::Ident) {
        if let Some(text) = self.source(span) {
            let _replaced = self.items.constants.insert(text, name.to_string());
        }
    }

    /// Records the item `name` at `span` if its tokens name a sensitive name.
    fn sensitive(&mut self, span: proc_macro2::Span, name: &str) {
        let Some(text) = self.source(span) else {
            return;
        };
        let Ok(tokens) = text.parse::<TokenStream>() else {
            return;
        };
        let names = identifiers(tokens);
        if names.iter().any(|ident| SENSITIVE.iter().any(|sensitive| ident == sensitive)) {
            let _replaced = self.items.sensitive.insert(text, name.to_owned());
        }
    }
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.constant(node.span(), &node.ident);
        visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.constant(node.span(), &node.ident);
        visit::visit_item_static(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        self.constant(node.span(), &node.ident);
        visit::visit_impl_item_const(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        for member in &node.items {
            let name = match member {
                syn::ImplItem::Fn(method) => method.sig.ident.to_string(),
                syn::ImplItem::Const(constant) => constant.ident.to_string(),
                syn::ImplItem::Type(alias) => alias.ident.to_string(),
                syn::ImplItem::Macro(_) | syn::ImplItem::Verbatim(_) | _ => "a member".to_owned(),
            };
            self.sensitive(member.span(), &name);
        }
        visit::visit_item_impl(self, node);
    }
}

/// The name of an item, for a message.
fn item_name(item: &syn::Item) -> String {
    if let syn::Item::Fn(function) = item {
        return function.sig.ident.to_string();
    }
    if let syn::Item::Use(import) = item {
        return format!("an import at line {}", import.span().start().line);
    }
    format!("the item at line {}", item.span().start().line)
}
