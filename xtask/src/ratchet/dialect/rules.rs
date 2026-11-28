//! The dialect of a rule module, and the `check_*` call sites read from it.
//!
//! A call site is keyed by where it is and how it calls: `(Type::method, self.check_x)` for a
//! method call on `self` inside an `impl` of `Type`, and `(function, check_x)` for a call of a
//! top-level function. Each must resolve to a definition in the same file: an inherent method
//! of `Type`, or a top-level function. Every other use of a `check_*` name, such as a call
//! through another receiver or path, a trait method, a local function or a mention inside a
//! macro, is outside the dialect, so a call cannot be redirected to a look-alike.

use super::{
    TABLES, identifiers, is_doc, is_test_module, outside, parse, path_text, without_tests,
};
use std::collections::BTreeSet;
use syn::visit::{self, Visit};

/// The macros a rule module may use: standard expression macros that take expressions and
/// cannot define an item or hide a string.
pub const MACROS: [&str; 14] = [
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

/// The attributes allowed on an item, a field or a variant.
const ITEM_ATTRIBUTES: [&str; 5] = ["doc", "derive", "must_use", "inline", "expect"];

/// The prefix of the names whose call sites are kept.
const CHECK: &str = "check_";

/// One `check_*` call site: the enclosing function, as `Type::method` or `function`, and the
/// call, as `self.check_x` or `check_x`.
pub type CallSite = (String, String);

/// The `check_*` call sites of a rule module, after checking that it is in the dialect.
///
/// # Errors
///
/// No verdict: the module does not parse, or is outside the dialect.
pub fn call_sites(relative: &str, text: &str) -> Result<BTreeSet<CallSite>, String> {
    let file = parse(relative, text)?;
    let items: Vec<&syn::Item> = file
        .items
        .iter()
        .filter(|item| !matches!(item, syn::Item::Mod(module) if is_test_module(module)))
        .collect();
    let definitions = definitions(&items);
    let mut reader = Reader {
        definitions: &definitions,
        approved: BTreeSet::new(),
        impl_type: None,
        function: Vec::new(),
        sites: BTreeSet::new(),
        accounted: 0,
        problem: None,
    };
    for attribute in &file.attrs {
        reader.approve_if(attribute, is_doc(attribute), "a file attribute");
    }
    for item in &items {
        reader.visit_item(item);
    }
    if let Some(problem) = reader.problem {
        return Err(outside(relative, &problem));
    }
    let mentioned = identifiers(without_tests(relative, text)?)
        .iter()
        .filter(|ident| ident.to_string().starts_with(CHECK))
        .count();
    let defined = definitions.functions.len() + definitions.methods.len();
    if mentioned != defined + reader.accounted {
        return Err(outside(
            relative,
            "a `check_*` name used in another form than a top-level function, a method of an \
             inherent `impl`, or a call `self.check_*(..)` or `check_*(..)` of one",
        ));
    }
    Ok(reader.sites)
}

/// The string literals of a rule module outside its tests, macro bodies included.
///
/// # Errors
///
/// No verdict: the module does not parse.
pub fn literals(relative: &str, text: &str) -> Result<BTreeSet<String>, String> {
    Ok(super::string_literals(without_tests(relative, text)?).into_iter().collect())
}

/// The `check_*` functions and inherent methods defined at the top of a module.
struct Definitions {
    functions: BTreeSet<String>,
    /// `(type, method)`.
    methods: BTreeSet<(String, String)>,
}

/// The name of the type an `impl` is for: the last segment of its path.
fn type_name(self_ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = self_ty else {
        return None;
    };
    path.path.segments.last().map(|segment| segment.ident.to_string())
}

fn definitions(items: &[&syn::Item]) -> Definitions {
    let mut found = Definitions { functions: BTreeSet::new(), methods: BTreeSet::new() };
    for item in items {
        if let syn::Item::Fn(function) = item
            && function.sig.ident.to_string().starts_with(CHECK)
        {
            let _new = found.functions.insert(function.sig.ident.to_string());
        }
        if let syn::Item::Impl(block) = item
            && block.trait_.is_none()
            && let Some(name) = type_name(&block.self_ty)
        {
            found.methods.extend(checking_methods(block).map(|method| (name.clone(), method)));
        }
    }
    found
}

/// The names of the `check_*` methods of an `impl` block.
fn checking_methods(block: &syn::ItemImpl) -> impl Iterator<Item = String> + '_ {
    block.items.iter().filter_map(|member| {
        let syn::ImplItem::Fn(method) = member else {
            return None;
        };
        let name = method.sig.ident.to_string();
        name.starts_with(CHECK).then_some(name)
    })
}

struct Reader<'d> {
    definitions: &'d Definitions,
    /// The attributes already allowed in their position, by address.
    approved: BTreeSet<usize>,
    /// The type of the `impl` around the current item, if any.
    impl_type: Option<String>,
    /// The functions around the current point, outermost first.
    function: Vec<String>,
    sites: BTreeSet<CallSite>,
    /// The `check_*` names in calls that resolve.
    accounted: usize,
    problem: Option<String>,
}

impl Reader<'_> {
    fn flag(&mut self, problem: String) {
        if self.problem.is_none() {
            self.problem = Some(problem);
        }
    }

    fn approve_if(&mut self, attribute: &syn::Attribute, allowed: bool, position: &str) {
        if allowed {
            let _new = self.approved.insert(std::ptr::from_ref(attribute).addr());
        } else {
            let name = path_text(attribute.path());
            self.flag(format!("the attribute `#[{name}]` on {position}"));
        }
    }

    fn item_attributes(&mut self, attributes: &[syn::Attribute], position: &str) {
        for attribute in attributes {
            let allowed = ITEM_ATTRIBUTES.iter().any(|name| attribute.path().is_ident(name));
            self.approve_if(attribute, allowed, position);
        }
    }

    /// The enclosing function, as `Type::function` or `function`.
    fn enclosing(&self) -> String {
        let function = self.function.join("::");
        match &self.impl_type {
            Some(name) => format!("{name}::{function}"),
            None => function,
        }
    }

    fn within(&mut self, name: &syn::Ident, visit_body: impl FnOnce(&mut Self)) {
        self.function.push(name.to_string());
        visit_body(self);
        let _left = self.function.pop();
    }

    /// A binding of `ident`: a table name is refused, so it cannot shadow the registry's.
    fn binding(&mut self, ident: &syn::Ident, what: &str) {
        if TABLES.iter().any(|table| ident == table) || ident == "registry" {
            self.flag(format!("{what} named `{ident}`, which could shadow the registry's"));
        }
    }
}

/// Upper bound on the subtrees of one `use` item, so the walk is bounded.
const USE_TREES_MAX: u32 = 10_000;

/// The import prefixes that name the registry module.
const REGISTRY_PATHS: [&[&str]; 2] = [&["super", "registry"], &["crate", "tidy", "registry"]];

impl Reader<'_> {
    /// Refuses glob and renamed imports, and a table or `registry` imported from anywhere
    /// but the registry.
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
                syn::UseTree::Rename(rename) => {
                    self.flag(format!("the renamed import `{}`", rename.rename));
                }
                syn::UseTree::Name(name) => self.imported(&prefix, &name.ident),
            }
        }
        panic!("a `use` item has more than {USE_TREES_MAX} subtrees");
    }

    fn imported(&mut self, prefix: &[String], ident: &syn::Ident) {
        let is_registry = |path: &[String]| {
            REGISTRY_PATHS
                .iter()
                .any(|registry| path.iter().map(String::as_str).eq(registry.iter().copied()))
        };
        let full: Vec<String> = prefix.iter().cloned().chain([ident.to_string()]).collect();
        let fine = if TABLES.iter().any(|table| ident == table) {
            is_registry(prefix)
        } else if ident == "registry" {
            is_registry(&full)
        } else if ident == "self" {
            prefix.last().is_none_or(|last| last != "registry") || is_registry(prefix)
        } else {
            true
        };
        if !fine {
            self.flag(format!("the import `{}`, which is not the registry's", full.join("::")));
        }
    }
}

impl Reader<'_> {
    /// What is wrong with an item in its position, if anything.
    fn item_problem(&self, node: &syn::Item) -> Option<String> {
        if let syn::Item::Mod(module) = node {
            return Some(format!("the module `{}`", module.ident));
        }
        if let syn::Item::Macro(item) = node {
            return Some(format!("the item macro `{}!`", path_text(&item.mac.path)));
        }
        if let syn::Item::Impl(_) = node
            && !self.function.is_empty()
        {
            return Some("a local `impl` block".to_owned());
        }
        let unread = matches!(
            node,
            syn::Item::ExternCrate(_)
                | syn::Item::ForeignMod(_)
                | syn::Item::TraitAlias(_)
                | syn::Item::Verbatim(_)
        );
        unread.then(|| "an item the ratchet does not read".to_owned())
    }
}

impl<'ast> Visit<'ast> for Reader<'_> {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        if !self.approved.contains(&std::ptr::from_ref(node).addr()) {
            let name = path_text(node.path());
            self.flag(format!("the attribute `#[{name}]` in a position that takes none"));
        }
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        if let Some(problem) = self.item_problem(node) {
            self.flag(problem);
        }
        if let Some(attributes) = item_attrs(node) {
            self.item_attributes(attributes, "an item");
        }
        visit::visit_item(self, node);
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        match node {
            syn::ImplItem::Fn(method) => self.item_attributes(&method.attrs, "a method"),
            syn::ImplItem::Const(constant) => {
                self.item_attributes(&constant.attrs, "an associated constant");
                self.binding(&constant.ident, "an associated constant");
            }
            syn::ImplItem::Type(alias) => self.item_attributes(&alias.attrs, "an associated type"),
            syn::ImplItem::Macro(_) | syn::ImplItem::Verbatim(_) | _ => {
                self.flag("an `impl` member the ratchet does not read".to_owned());
            }
        }
        visit::visit_impl_item(self, node);
    }

    fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
        match node {
            syn::TraitItem::Fn(method) => self.item_attributes(&method.attrs, "a trait method"),
            syn::TraitItem::Const(constant) => {
                self.item_attributes(&constant.attrs, "a trait constant");
                self.binding(&constant.ident, "a trait constant");
            }
            syn::TraitItem::Type(alias) => self.item_attributes(&alias.attrs, "a trait type"),
            syn::TraitItem::Macro(_) | syn::TraitItem::Verbatim(_) | _ => {
                self.flag("a trait member the ratchet does not read".to_owned());
            }
        }
        visit::visit_trait_item(self, node);
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        self.item_attributes(&node.attrs, "a field");
        visit::visit_field(self, node);
    }

    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        self.item_attributes(&node.attrs, "a variant");
        visit::visit_variant(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let outer = std::mem::replace(&mut self.impl_type, type_name(&node.self_ty));
        visit::visit_item_impl(self, node);
        self.impl_type = outer;
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.within(&node.sig.ident, |reader| visit::visit_item_fn(reader, node));
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.within(&node.sig.ident, |reader| visit::visit_impl_item_fn(reader, node));
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.imports(&node.tree);
        visit::visit_item_use(self, node);
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.binding(&node.ident, "a constant");
        visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.binding(&node.ident, "a static");
        visit::visit_item_static(self, node);
    }

    fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
        self.binding(&node.ident, "a binding");
        visit::visit_pat_ident(self, node);
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        let segments: Vec<String> =
            node.segments.iter().map(|segment| segment.ident.to_string()).collect();
        if let Some((last, prefix)) = segments.split_last()
            && TABLES.contains(&last.as_str())
        {
            let fine = prefix.is_empty()
                || prefix == ["registry"]
                || REGISTRY_PATHS
                    .iter()
                    .any(|registry| prefix.iter().map(String::as_str).eq(registry.iter().copied()));
            if !fine {
                self.flag(format!(
                    "the path `{}`, which is not the registry's",
                    segments.join("::")
                ));
            }
        }
        visit::visit_path(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let allowed = node
            .path
            .get_ident()
            .is_some_and(|name| MACROS.iter().any(|macro_name| name == macro_name));
        if !allowed {
            self.flag(format!("the macro `{}!`", path_text(&node.path)));
        }
        let only_strings = node
            .parse_body_with(
                syn::punctuated::Punctuated::<syn::LitStr, syn::Token![,]>::parse_terminated,
            )
            .is_ok();
        if node.path.is_ident("concat") && !only_strings {
            self.flag("a `concat!` of something other than string literals".to_owned());
        }
        visit::visit_macro(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        let on_self =
            matches!(&*node.receiver, syn::Expr::Path(path) if path.path.is_ident("self"));
        if method.starts_with(CHECK)
            && on_self
            && let Some(name) = &self.impl_type
            && self.definitions.methods.contains(&(name.clone(), method.clone()))
        {
            let _new = self.sites.insert((self.enclosing(), format!("self.{method}")));
            self.accounted += 1;
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func
            && path.qself.is_none()
            && let Some(ident) = path.path.get_ident()
            && ident.to_string().starts_with(CHECK)
            && self.definitions.functions.contains(&ident.to_string())
        {
            let _new = self.sites.insert((self.enclosing(), ident.to_string()));
            self.accounted += 1;
        }
        visit::visit_expr_call(self, node);
    }
}

/// The attributes of an item.
fn item_attrs(item: &syn::Item) -> Option<&Vec<syn::Attribute>> {
    match item {
        syn::Item::Const(inner) => Some(&inner.attrs),
        syn::Item::Enum(inner) => Some(&inner.attrs),
        syn::Item::Fn(inner) => Some(&inner.attrs),
        syn::Item::Impl(inner) => Some(&inner.attrs),
        syn::Item::Static(inner) => Some(&inner.attrs),
        syn::Item::Struct(inner) => Some(&inner.attrs),
        syn::Item::Trait(inner) => Some(&inner.attrs),
        syn::Item::Type(inner) => Some(&inner.attrs),
        syn::Item::Use(inner) => Some(&inner.attrs),
        syn::Item::Union(inner) => Some(&inner.attrs),
        syn::Item::ExternCrate(_)
        | syn::Item::ForeignMod(_)
        | syn::Item::Macro(_)
        | syn::Item::Mod(_)
        | syn::Item::TraitAlias(_)
        | syn::Item::Verbatim(_)
        | _ => None,
    }
}
