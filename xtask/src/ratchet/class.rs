//! Whether a rule-path diff only adds rule code or edits existing rule code (ADR-0022).
//!
//! Every honest loosening that the review of #57 found edited existing rule code together
//! with its tests. Mutation testing cannot see that, because the edited tests pin the edited
//! behaviour; the independent review caught each one. So the ratchet sorts each diff into one
//! of two classes, and the class decides what stands behind a pass:
//!
//! - **Additive**: the diff adds rules and leaves every existing item as it is. It merges on
//!   the ratchet's proof alone: the corpus, end to end, mutants on the new code, and the data
//!   checks. The review gate needs no review by the reviewer app, only that none of its
//!   requests for changes or threads is open ([`super::review`]).
//! - **Edit**: anything else in the rule paths. The ratchet still runs every check, and the
//!   review gate also needs an approval of the head commit by the reviewer app; until the app
//!   exists (#59), a code owner's approval.
//!
//! A diff is additive when each change in the rule paths is one of these:
//!
//! - (a) a new top-level item of a Rust file, new or existing: a function, a struct, enum,
//!   union or trait, a lint or lint pass declared with `declare_lint!`, `declare_tool_lint!`
//!   or `declare_lint_pass!`, a test module, and in a new file an import without a glob, in an
//!   existing file a file module `mod name;`. Its attributes are only those of [`ATTRIBUTES`];
//!   it names none of [`SENSITIVE`] and uses none of [`INPUT_MACROS`]; it holds no `impl`,
//!   trait, module or `macro_rules!` nested inside; and in an existing file its name is no
//!   identifier of the base's file, so that it shadows nothing the existing code names (a
//!   prelude item such as `drop`, an import, a glob). A new `impl` is additive only for a type
//!   new in the same file, named by one plain segment, with no generics on the `impl`, and of a
//!   trait new in the same file or one of [`ALLOWED_TRAITS`]. Any other `impl` (of an existing
//!   type, in another file, generic or blanket, of a visitor trait) can override a trait method
//!   or change which method an existing call resolves to, so it is an edit;
//! - (b) a dispatch call ([`dispatch`]): a statement inserted at the top level of an existing
//!   function's body that calls a new function with shared references and literals only, hands
//!   such a call's result to `extend`, or registers a new lint or lint pass; each new item is
//!   dispatched once;
//! - (c) new registry rows, and changed rows that the data checks prove only tighten: no
//!   changed field outside those the rule-set check compares or that only document
//!   ([`super::dialect::registry::unproven_rows`]);
//! - (d) new fixtures, UI cases and other new files in the rule paths, and new items inside an
//!   existing test module whose existing items stay as they are, if they use none of
//!   [`INPUT_MACROS`];
//! - (e) a changed documentation field of a registry row, and a changed doc comment anywhere;
//! - (f) the regenerated `docs/RULES.md`.
//!
//! Everything else is an edit: a changed or removed function, method, constant, static, type,
//! `impl`, trait, import, macro or test; a new member of an existing `impl`; a new constant,
//! static, macro or inline module; a changed or deleted fixture, UI case or other file. Items
//! are compared token for token, with doc comments left out.

pub mod dispatch;

use super::dialect;
use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use syn::spanned::Spanned;

/// Names that reach the environment, processes or files: `std::env`, `env!`, `option_env!`,
/// `std::process`, `std::fs`, `std::io`, and the methods of `Path` that ask the file system.
/// `File` and `OpenOptions` are reached through `fs`, so `syn::File` is no such name. New code
/// that names one can behave differently in CI than where the ratchet observes it, so it is an
/// edit.
pub const SENSITIVE: [&str; 17] = [
    "symlink_metadata",
    "env",
    "option_env",
    "process",
    "Command",
    "fs",
    "io",
    "read_to_string",
    "read_dir",
    "read_link",
    "exists",
    "try_exists",
    "is_file",
    "is_dir",
    "is_symlink",
    "metadata",
    "canonicalize",
];

/// The attributes a new item may carry.
pub const ATTRIBUTES: [&str; 6] = ["doc", "derive", "must_use", "inline", "expect", "test"];

/// The macros that may declare a new lint or lint pass as an item.
const LINT_MACROS: [&str; 3] = ["declare_lint", "declare_tool_lint", "declare_lint_pass"];

/// The macro that declares a lint pass, a new type.
const PASS_MACRO: &str = "declare_lint_pass";

/// The generated rule list, which is additive whatever changed in it.
const RULES_DOC: &str = "docs/RULES.md";

/// Upper bound on the token trees of one item, so that every walk is bounded.
const TOKENS_MAX: u32 = 2_000_000;

/// The class of a rule-path diff, with what makes it an edit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Class {
    /// One line per change to existing rule code; empty for an additive diff.
    pub edits: Vec<String>,
    /// The head lines of every dispatch call, by file: the new rule's case shows that it runs,
    /// so a hunk of them needs no mutant of its own.
    pub dispatch: BTreeMap<String, Vec<(u32, u32)>>,
}

impl Class {
    /// True if the diff edits existing rule code.
    #[must_use]
    pub fn is_edit(&self) -> bool {
        !self.edits.is_empty()
    }

    /// The class's name, as the verdict lines print it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        if self.is_edit() { "edit" } else { "additive" }
    }
}

/// Sorts the changed rule paths `rule_paths` into additive or edit, reading both trees as data.
///
/// # Errors
///
/// No verdict: a file that cannot be read, or a Rust file that does not parse.
pub fn classify(base: &Path, head: &Path, rule_paths: &[String]) -> Result<Class, String> {
    let mut class = Class::default();
    let mut code = Vec::new();
    for path in rule_paths {
        let before = super::source::read_optional(base, path)?;
        let after = super::source::read_optional(head, path)?;
        if is_code(path) {
            code.push(Code::read(path, before, after)?);
        } else if path != RULES_DOC && before.is_some() {
            let what = if after.is_some() { "changed" } else { "deleted" };
            class.edits.push(format!("{path}: an existing file of the rule paths is {what}"));
        }
    }
    let new = new_names(&code)?;
    let mut dispatched = BTreeSet::new();
    for file in &code {
        file.judge(&new, &mut class, &mut dispatched)?;
    }
    if rule_paths.iter().any(|path| path == super::source::REGISTRY) {
        let read = |root: &Path| super::source::read_optional(root, super::source::REGISTRY);
        if let (Some(before), Some(after)) = (read(base)?, read(head)?) {
            class.edits.extend(dialect::registry::unproven_rows(&before, &after)?);
        }
    }
    assert!(class.dispatch.keys().all(|path| rule_paths.contains(path)), "dispatch is changed");
    Ok(class)
}

/// True for a rule-path Rust file that is code: the lint library and the rule modules, not a
/// UI case or a fixture.
fn is_code(path: &str) -> bool {
    let rust = Path::new(path).extension().is_some_and(|extension| extension == "rs");
    rust && !path.starts_with("lints/ui/") && !path.starts_with("xtask/fixtures/")
}

/// One side of a changed Rust file: its text, which the spans point into, and its syntax.
struct Side {
    text: String,
    file: syn::File,
}

/// One changed Rust file of the rule paths: its two sides, parsed.
struct Code {
    path: String,
    base: Option<Side>,
    head: Option<Side>,
}

impl Code {
    fn read(path: &str, base: Option<String>, head: Option<String>) -> Result<Self, String> {
        let parse = |side: Option<String>| -> Result<Option<Side>, String> {
            let Some(text) = side else {
                return Ok(None);
            };
            let file = syn::parse_file(&text)
                .map_err(|error| format!("{path} does not parse: {error}"))?;
            Ok(Some(Side { text, file }))
        };
        let code = Self { path: path.to_owned(), base: parse(base)?, head: parse(head)? };
        assert!(code.base.is_some() || code.head.is_some(), "a changed file is on one side");
        Ok(code)
    }

    /// The head's top-level items that the base does not have, by key: every item of a new
    /// file. The registry's tables are judged as data, not here.
    fn new_items(&self) -> Result<Vec<(String, &syn::Item)>, String> {
        let Some(head) = &self.head else {
            return Ok(Vec::new());
        };
        let after = keyed(&self.path, head)?;
        let before = match &self.base {
            Some(base) => keyed(&self.path, base)?,
            None => Vec::new(),
        };
        let known: BTreeSet<&String> = before.iter().map(|(key, _)| key).collect();
        Ok(after.into_iter().filter(|(key, _)| !known.contains(key)).collect())
    }

    /// Judges the file's changes, and records each edit and dispatch call in `class`.
    fn judge(
        &self,
        new: &NewNames,
        class: &mut Class,
        dispatched: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        let Some(head) = &self.head else {
            class.edits.push(format!("{}: an existing rule-path file is deleted", self.path));
            return Ok(());
        };
        let base_names = match &self.base {
            Some(base) => Some(file_identifiers(&self.path, &base.text)?),
            None => None,
        };
        for (key, item) in self.new_items()? {
            let named = identifiers_at(&self.path, &head.text, item.span())?;
            let shadowed = base_names.as_ref();
            let tokens = tokens_at(&self.path, &head.text, item.span())?;
            let found = NewItem { path: &self.path, item, named: &named, tokens };
            if let Some(problem) = found.problem(shadowed, new) {
                class.edits.push(format!("{}: the new item `{key}` {problem}", self.path));
            }
        }
        let Some(base) = &self.base else {
            return Ok(());
        };
        let mut existing = Existing { code: self, base, head, new, class, dispatched };
        existing.compare()
    }
}

/// The new items of the diff that a dispatch call or an `impl` may name, by the file that
/// defines them: a name alone would let a new type in one file vouch for an `impl` of an
/// existing type of the same name in another (review of #57, round 5).
#[derive(Debug, Default)]
pub struct NewNames {
    /// `(file, function)`: new top-level functions.
    pub functions: BTreeSet<(String, String)>,
    /// `(file, type)`: new structs, enums, unions, traits and lint passes.
    pub types: BTreeSet<(String, String)>,
    /// New lints, by their declared name, which is unique in the lint library.
    pub lints: BTreeSet<String>,
}

/// The new functions, types and lints of every changed file.
fn new_names(code: &[Code]) -> Result<NewNames, String> {
    let mut new = NewNames::default();
    for file in code {
        for (_, item) in file.new_items()? {
            new.add(&file.path, item);
        }
    }
    Ok(new)
}

impl NewNames {
    /// Records what the new `item` of the file `path` defines.
    fn add(&mut self, path: &str, item: &syn::Item) {
        let before = self.functions.len() + self.types.len() + self.lints.len();
        let at = |name: String| (path.to_owned(), name);
        if let syn::Item::Fn(function) = item {
            let _new = self.functions.insert(at(function.sig.ident.to_string()));
        } else if let syn::Item::Macro(declared) = item
            && let Some(name) = lint_macro_name(&declared.mac)
        {
            let segments = &declared.mac.path.segments;
            let is_pass = segments.last().is_some_and(|last| last.ident == PASS_MACRO);
            let _new = if is_pass { self.types.insert(at(name)) } else { self.lints.insert(name) };
        } else if let Some(name) = type_name(item) {
            let _new = self.types.insert(at(name));
        }
        let after = self.functions.len() + self.types.len() + self.lints.len();
        assert!(after <= before + 1, "an item defines at most one name");
    }
}

/// The name of a type an item defines and an `impl` may be for. A type alias is none: an
/// `impl` of an alias is an `impl` of the type it names.
fn type_name(item: &syn::Item) -> Option<String> {
    let ident = match item {
        syn::Item::Struct(item) => &item.ident,
        syn::Item::Enum(item) => &item.ident,
        syn::Item::Union(item) => &item.ident,
        syn::Item::Trait(item) => &item.ident,
        syn::Item::Const(_)
        | syn::Item::ExternCrate(_)
        | syn::Item::Fn(_)
        | syn::Item::ForeignMod(_)
        | syn::Item::Impl(_)
        | syn::Item::Macro(_)
        | syn::Item::Mod(_)
        | syn::Item::Static(_)
        | syn::Item::TraitAlias(_)
        | syn::Item::Type(_)
        | syn::Item::Use(_)
        | syn::Item::Verbatim(_)
        | _ => return None,
    };
    Some(ident.to_string())
}

/// The name a lint-declaring macro defines: the lint of `declare_lint! { pub NAME, .. }`, the
/// last name before the first comma, or the pass of `declare_lint_pass!(Name => [..])`, the
/// first name. `None` for any other macro.
fn lint_macro_name(mac: &syn::Macro) -> Option<String> {
    let macro_name = mac.path.segments.last()?.ident.to_string();
    if !LINT_MACROS.contains(&macro_name.as_str()) {
        return None;
    }
    let mut names = Vec::new();
    for tree in mac.tokens.clone() {
        match tree {
            TokenTree::Punct(punct) if punct.as_char() == ',' => break,
            TokenTree::Ident(ident) if ident != "pub" => names.push(ident.to_string()),
            TokenTree::Punct(_)
            | TokenTree::Group(_)
            | TokenTree::Ident(_)
            | TokenTree::Literal(_) => {}
        }
    }
    if macro_name == PASS_MACRO { names.first().cloned() } else { names.last().cloned() }
}

/// A new top-level item of a changed file, as the class judges it.
struct NewItem<'a> {
    /// The file that holds it.
    path: &'a str,
    item: &'a syn::Item,
    /// Its identifiers, macro bodies included.
    named: &'a BTreeSet<String>,
    /// Its tokens.
    tokens: TokenStream,
}

impl NewItem<'_> {
    /// What makes the item an edit, if anything. `shadowed` are the identifiers of the base's
    /// file, for an existing file.
    fn problem(&self, shadowed: Option<&BTreeSet<String>>, new: &NewNames) -> Option<String> {
        if let Some(input) = input_macro(self.tokens.clone()) {
            return Some(format!(
                "uses `{input}!`, which reads the source tree or the environment as it compiles, \
                 so a test can pin the file rather than the rule"
            ));
        }
        if let syn::Item::Mod(module) = self.item
            && dialect::is_test_module(module)
        {
            return None;
        }
        let attributes = item_attributes(self.item);
        if let Some(attribute) = attributes.iter().find(|attribute| !is_allowed(attribute)) {
            let name = dialect::path_text(attribute.path());
            return Some(format!("carries the attribute `{name}`, which can change what it does"));
        }
        if let Some(sensitive) = SENSITIVE.iter().find(|name| self.named.contains(**name)) {
            return Some(format!(
                "names `{sensitive}`: new code that reads the environment, a process or a file \
                 can behave differently in CI than where the ratchet observes it"
            ));
        }
        if let Some(nested) = nested_item(self.item) {
            return Some(format!(
                "holds {nested} inside it, which can reach existing items from where the class \
                 does not look"
            ));
        }
        if let Some(problem) = kind_problem(self.item, self.path, shadowed.is_some(), new) {
            return Some(problem);
        }
        let defined = defined_name(self.item)?;
        let shadows = shadowed.is_some_and(|names| names.contains(&defined));
        shadows.then(|| {
            format!(
                "is named `{defined}`, a name the base's code of this file uses, so it can \
                 shadow what that code names"
            )
        })
    }
}

/// The macros that read the source tree or the environment as code compiles. A new item,
/// a test among them, may not use them: a test that pins `include_str!(file!())` kills every
/// mutant of the file without testing a rule (review of #57, round 5).
pub const INPUT_MACROS: [&str; 6] =
    ["include", "include_str", "include_bytes", "file", "env", "option_env"];

/// The first of [`INPUT_MACROS`] that `stream` invokes, macro bodies included.
fn input_macro(stream: TokenStream) -> Option<String> {
    let mut pending: Vec<Vec<TokenTree>> = vec![stream.into_iter().collect()];
    for _ in 0..TOKENS_MAX {
        let sequence = pending.pop()?;
        for pair in sequence.windows(2) {
            if let [TokenTree::Ident(name), TokenTree::Punct(bang)] = pair
                && bang.as_char() == '!'
                && INPUT_MACROS.iter().any(|input| name == input)
            {
                return Some(name.to_string());
            }
        }
        for tree in sequence {
            if let TokenTree::Group(group) = tree {
                pending.push(group.stream().into_iter().collect());
            }
        }
    }
    panic!("an item has more than {TOKENS_MAX} token sequences");
}

/// What `item` holds inside it that the class does not judge: an `impl`, a trait, a module or
/// a `macro_rules!` nested in a function body, at any depth.
fn nested_item(item: &syn::Item) -> Option<&'static str> {
    let mut finder = Nested { depth: 0, found: None };
    syn::visit::Visit::visit_item(&mut finder, item);
    assert_eq!(finder.depth, 0, "every item that was entered was left");
    finder.found
}

/// Finds an item kind that may not nest, below the top-level item.
struct Nested {
    depth: u32,
    found: Option<&'static str>,
}

impl<'ast> syn::visit::Visit<'ast> for Nested {
    fn visit_item(&mut self, node: &'ast syn::Item) {
        let mut kind = None;
        if let syn::Item::Impl(_) = node {
            kind = Some("an `impl`");
        } else if let syn::Item::Trait(_) = node {
            kind = Some("a trait");
        } else if let syn::Item::Mod(_) = node {
            kind = Some("a module");
        } else if let syn::Item::Macro(declared) = node
            && declared.mac.path.is_ident("macro_rules")
        {
            kind = Some("a `macro_rules!`");
        }
        if self.depth > 0 && self.found.is_none() {
            self.found = kind;
        }
        self.depth += 1;
        syn::visit::visit_item(self, node);
        self.depth -= 1;
    }
}

/// The std traits that a new type may implement: none of them can change which method a call
/// on an existing type resolves to. A visitor trait is not among them, and neither is any
/// trait with methods an existing call could resolve to.
pub const ALLOWED_TRAITS: [&str; 7] =
    ["Debug", "Clone", "Copy", "PartialEq", "Eq", "Default", "Display"];

/// What makes a new `impl` in the file `path` an edit, if anything. It is additive only for a
/// type new in the same file, named by a single segment without arguments, with no generics
/// on the `impl`, and of a trait new in the same file or one of [`ALLOWED_TRAITS`]. An `impl`
/// of an existing type, one elsewhere, a generic or blanket one (`impl<T> Quiet for T`, even
/// when `T` shadows the new type's name) can override a trait method or change which method an
/// existing call resolves to (review of #57, round 5).
fn impl_problem(block: &syn::ItemImpl, path: &str, new: &NewNames) -> Option<String> {
    let is_new = |name: &str| new.types.contains(&(path.to_owned(), name.to_owned()));
    let own = plain_self_type(&block.self_ty).is_some_and(|name| is_new(&name));
    let generic = !block.generics.params.is_empty() || block.generics.where_clause.is_some();
    if !own || generic {
        return Some(
            "is an `impl` that is not of a type new in this file, written plainly and without \
             generics; such an `impl` can override a trait method or change which method an \
             existing call resolves to"
                .to_owned(),
        );
    }
    let (trait_path, _) = block.trait_.as_ref()?;
    let plain = trait_path.leading_colon.is_none()
        && trait_path.segments.iter().all(|segment| segment.arguments.is_none());
    let name = trait_path.segments.last().map_or_else(String::new, |last| last.ident.to_string());
    let own_trait = trait_path.segments.len() == 1 && is_new(&name);
    let allowed = ALLOWED_TRAITS.contains(&name.as_str());
    (!(plain && (own_trait || allowed))).then(|| {
        format!(
            "implements `{name}`, a trait that is neither new in this file nor one of \
             {ALLOWED_TRAITS:?}; a visitor trait's default methods are rule code"
        )
    })
}

/// The name of the type an `impl` is for, if it is a single plain segment.
fn plain_self_type(self_ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = self_ty else {
        return None;
    };
    let [segment] = path.path.segments.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let plain = path.qself.is_none() && path.path.leading_colon.is_none();
    (plain && segment.arguments.is_none()).then(|| segment.ident.to_string())
}

/// What makes a new item of this kind in the file `path` an edit, if anything.
fn kind_problem(
    item: &syn::Item,
    path: &str,
    existing_file: bool,
    new: &NewNames,
) -> Option<String> {
    match item {
        syn::Item::Fn(_)
        | syn::Item::Struct(_)
        | syn::Item::Enum(_)
        | syn::Item::Union(_)
        | syn::Item::Trait(_) => None,
        syn::Item::Impl(block) => impl_problem(block, path, new),
        syn::Item::Macro(item) if lint_macro_name(&item.mac).is_some() => None,
        syn::Item::Use(import) if !existing_file && !is_glob(&import.tree) => None,
        syn::Item::Mod(module) if existing_file && module.content.is_none() => None,
        syn::Item::Const(_) | syn::Item::Static(_) => Some(
            "is a constant or static: mutation testing makes no mutant for its value".to_owned(),
        ),
        syn::Item::Use(_) => Some("is an import into an existing module, or a glob".to_owned()),
        syn::Item::ExternCrate(_)
        | syn::Item::ForeignMod(_)
        | syn::Item::Macro(_)
        | syn::Item::Mod(_)
        | syn::Item::TraitAlias(_)
        | syn::Item::Type(_)
        | syn::Item::Verbatim(_)
        | _ => Some("is not a function, a type, an `impl` of a new type or a lint".to_owned()),
    }
}

/// The name of the type an `impl` is for, if it is a plain path: its last segment.
fn self_type(self_ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = self_ty else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    path.path.segments.last().map(|segment| segment.ident.to_string())
}

/// True if an import tree holds a glob, which can shadow names.
fn is_glob(tree: &syn::UseTree) -> bool {
    let mut pending = vec![tree];
    for _ in 0..TOKENS_MAX {
        let Some(part) = pending.pop() else {
            return false;
        };
        match part {
            syn::UseTree::Glob(_) => return true,
            syn::UseTree::Path(path) => pending.push(&path.tree),
            syn::UseTree::Group(group) => pending.extend(group.items.iter()),
            syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {}
        }
    }
    panic!("an import tree has more than {TOKENS_MAX} parts");
}

/// The name a new item binds in its module, for the shadowing check.
fn defined_name(item: &syn::Item) -> Option<String> {
    if let syn::Item::Fn(function) = item {
        return Some(function.sig.ident.to_string());
    }
    if let syn::Item::Mod(module) = item {
        return Some(module.ident.to_string());
    }
    if let syn::Item::Macro(declared) = item {
        return lint_macro_name(&declared.mac);
    }
    type_name(item)
}

/// True for an attribute a new item may carry.
fn is_allowed(attribute: &syn::Attribute) -> bool {
    ATTRIBUTES.iter().any(|name| attribute.path().is_ident(name))
}

/// The outer attributes of an item; none for a form syn does not know.
fn item_attributes(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        syn::Item::Verbatim(_) | _ => &[],
    }
}

/// The tokens of `text` at `span`.
fn tokens_at(path: &str, text: &str, span: Span) -> Result<TokenStream, String> {
    let source = text
        .get(span.byte_range())
        .ok_or_else(|| format!("{path}: a span lies outside its file"))?;
    source.parse().map_err(|error| format!("{path}: an item does not lex: {error}"))
}

/// Every identifier of the file `text`, macro bodies and attributes included.
fn file_identifiers(path: &str, text: &str) -> Result<BTreeSet<String>, String> {
    let tokens: TokenStream =
        text.parse().map_err(|error| format!("{path} does not lex: {error}"))?;
    Ok(dialect::identifiers(tokens).iter().map(ToString::to_string).collect())
}

/// Every identifier of `text` at `span`, macro bodies included.
fn identifiers_at(path: &str, text: &str, span: Span) -> Result<BTreeSet<String>, String> {
    let tokens = tokens_at(path, text, span)?;
    Ok(dialect::identifiers(tokens).iter().map(ToString::to_string).collect())
}

/// The tokens of `text` at `span` as flat text without doc comments, so that a changed comment
/// changes nothing.
fn normal_at(path: &str, text: &str, span: Span) -> Result<String, String> {
    Ok(normal(tokens_at(path, text, span)?))
}

/// One step of [`normal`]'s walk: a token tree, or the end of a group.
enum Piece {
    Tree(TokenTree),
    Close(&'static str),
}

/// `stream` as flat text, every `#[doc ..]` and `#![doc ..]` left out.
fn normal(stream: TokenStream) -> String {
    let mut pending: Vec<Piece> = stream.into_iter().map(Piece::Tree).collect();
    pending.reverse();
    let mut words: Vec<String> = Vec::new();
    for _ in 0..TOKENS_MAX {
        let Some(piece) = pending.pop() else {
            return words.join(" ");
        };
        let tree = match piece {
            Piece::Close(close) => {
                words.push(close.to_owned());
                continue;
            }
            Piece::Tree(tree) => tree,
        };
        match tree {
            TokenTree::Punct(punct) if punct.as_char() == '#' && doc_follows(&pending) > 0 => {
                let skipped = doc_follows(&pending);
                pending.truncate(pending.len() - skipped);
            }
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                words.push(open.to_owned());
                pending.push(Piece::Close(close));
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                pending.extend(inner.into_iter().rev().map(Piece::Tree));
            }
            other @ (TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_)) => {
                words.push(other.to_string());
            }
        }
    }
    panic!("an item has more than {TOKENS_MAX} token trees");
}

/// How many pieces after a `#` make up a doc attribute: 1 for `[doc ..]`, 2 for `![doc ..]`,
/// and 0 if none does.
fn doc_follows(pending: &[Piece]) -> usize {
    let is_doc = |piece: Option<&Piece>| match piece {
        Some(Piece::Tree(TokenTree::Group(group))) => {
            let first = group.stream().into_iter().next();
            group.delimiter() == Delimiter::Bracket
                && matches!(first, Some(TokenTree::Ident(ident)) if ident == "doc")
        }
        _ => false,
    };
    let last = pending.len().checked_sub(1).and_then(|index| pending.get(index));
    let second = pending.len().checked_sub(2).and_then(|index| pending.get(index));
    if is_doc(last) {
        1
    } else if matches!(last, Some(Piece::Tree(TokenTree::Punct(bang))) if bang.as_char() == '!')
        && is_doc(second)
    {
        2
    } else {
        0
    }
}

/// The top-level items of a side by key: `fn name`, `struct Name` and so on, an `impl` by its
/// header, the test module as such, and any other item by its tokens. A repeated key gets
/// its count. The registry's tables are left out: the data checks judge them.
fn keyed<'s>(path: &str, side: &'s Side) -> Result<Vec<(String, &'s syn::Item)>, String> {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    let mut found = Vec::new();
    for item in &side.file.items {
        if path == super::source::REGISTRY && dialect::registry::is_table(item) {
            continue;
        }
        let key = item_key(path, &side.text, item)?;
        let count = seen.entry(key.clone()).or_insert(0);
        *count += 1;
        found.push((if *count == 1 { key } else { format!("{key} #{count}") }, item));
    }
    let keys: BTreeSet<&String> = found.iter().map(|(key, _)| key).collect();
    assert_eq!(keys.len(), found.len(), "the keys of a file are unique");
    Ok(found)
}

/// The key of one item; see [`keyed`].
fn item_key(path: &str, text: &str, item: &syn::Item) -> Result<String, String> {
    let named = |kind: &str, ident: &syn::Ident| Ok(format!("{kind} {ident}"));
    match item {
        syn::Item::Fn(item) => named("fn", &item.sig.ident),
        syn::Item::Struct(item) => named("struct", &item.ident),
        syn::Item::Enum(item) => named("enum", &item.ident),
        syn::Item::Union(item) => named("union", &item.ident),
        syn::Item::Type(item) => named("type", &item.ident),
        syn::Item::Trait(item) => named("trait", &item.ident),
        syn::Item::Const(item) => named("const", &item.ident),
        syn::Item::Static(item) => named("static", &item.ident),
        syn::Item::Mod(module) if dialect::is_test_module(module) => {
            Ok("the test module".to_owned())
        }
        syn::Item::Mod(module) => named("mod", &module.ident),
        syn::Item::Impl(block) => header(path, text, block.span(), block.brace_token.span.open()),
        other @ (syn::Item::ExternCrate(_)
        | syn::Item::ForeignMod(_)
        | syn::Item::Macro(_)
        | syn::Item::TraitAlias(_)
        | syn::Item::Use(_)
        | syn::Item::Verbatim(_)
        | _) => normal_at(path, text, other.span()),
    }
}

/// The normalised text of `text` from the start of `span` to the start of `open`: an item's
/// attributes and header up to its opening brace.
fn header(path: &str, text: &str, span: Span, open: Span) -> Result<String, String> {
    let range = span.byte_range().start..open.byte_range().start;
    let source =
        text.get(range).ok_or_else(|| format!("{path}: a header lies outside its file"))?;
    let tokens: TokenStream =
        source.parse().map_err(|error| format!("{path}: a header does not lex: {error}"))?;
    Ok(normal(tokens))
}

/// A short name of an `impl` for messages: `Visitor`, or `<Visitor as Visit>`.
fn impl_name(block: &syn::ItemImpl) -> String {
    let name = self_type(&block.self_ty).unwrap_or_else(|| "a type".to_owned());
    let trait_name = block.trait_.as_ref().and_then(|(path, _)| path.segments.last());
    match trait_name {
        Some(segment) => format!("<{name} as {}>", segment.ident),
        None => name,
    }
}

/// The key of a member of an `impl`: `fn name`, `const NAME`, `type Name`, or its tokens.
fn member_key(path: &str, text: &str, member: &syn::ImplItem) -> Result<String, String> {
    match member {
        syn::ImplItem::Fn(method) => Ok(format!("fn {}", method.sig.ident)),
        syn::ImplItem::Const(constant) => Ok(format!("const {}", constant.ident)),
        syn::ImplItem::Type(alias) => Ok(format!("type {}", alias.ident)),
        other @ (syn::ImplItem::Macro(_) | syn::ImplItem::Verbatim(_) | _) => {
            normal_at(path, text, other.span())
        }
    }
}

/// The items of an inline module; none for a file module.
fn module_items(module: &syn::ItemMod) -> &[syn::Item] {
    module.content.as_ref().map_or(&[], |(_, items)| items.as_slice())
}

/// A function with a body, as the comparison sees it: its whole span and its block.
#[derive(Clone, Copy)]
struct Function<'a> {
    span: Span,
    block: &'a syn::Block,
}

/// The comparison of an existing file's two sides.
struct Existing<'a> {
    code: &'a Code,
    base: &'a Side,
    head: &'a Side,
    new: &'a NewNames,
    class: &'a mut Class,
    dispatched: &'a mut BTreeSet<String>,
}

impl Existing<'_> {
    fn edit(&mut self, what: &str) {
        self.class.edits.push(format!("{}: {what}", self.code.path));
    }

    fn normal_base(&self, span: Span) -> Result<String, String> {
        normal_at(&self.code.path, &self.base.text, span)
    }

    fn normal_head(&self, span: Span) -> Result<String, String> {
        normal_at(&self.code.path, &self.head.text, span)
    }

    /// Compares the file's attributes, and every item of the base with the head's item of the
    /// same key.
    fn compare(&mut self) -> Result<(), String> {
        let attributes = |side: &Side| -> Result<Vec<String>, String> {
            let texts = side
                .file
                .attrs
                .iter()
                .map(|attribute| normal_at(&self.code.path, &side.text, attribute.span()));
            let texts: Vec<String> = texts.collect::<Result<_, _>>()?;
            Ok(texts.into_iter().filter(|text| !text.is_empty()).collect())
        };
        if attributes(self.base)? != attributes(self.head)? {
            self.edit("the file's attributes changed");
        }
        let before = keyed(&self.code.path, self.base)?;
        let after: BTreeMap<String, &syn::Item> =
            keyed(&self.code.path, self.head)?.into_iter().collect();
        for (key, item) in before {
            match after.get(&key) {
                Some(head_item) => self.compare_item(&key, item, head_item)?,
                None => self.edit(&format!("the item `{key}` is changed or removed")),
            }
        }
        Ok(())
    }

    fn compare_item(
        &mut self,
        key: &str,
        before: &syn::Item,
        after: &syn::Item,
    ) -> Result<(), String> {
        if self.normal_base(before.span())? == self.normal_head(after.span())? {
            return Ok(());
        }
        match (before, after) {
            (syn::Item::Fn(base), syn::Item::Fn(head)) => self.compare_function(
                &base.sig.ident.to_string(),
                Function { span: base.span(), block: &base.block },
                Function { span: head.span(), block: &head.block },
            ),
            (syn::Item::Impl(base), syn::Item::Impl(head)) => self.compare_impl(base, head),
            (syn::Item::Mod(base), syn::Item::Mod(head)) if dialect::is_test_module(base) => {
                self.compare_tests(base, head)
            }
            _ => {
                self.edit(&format!("the item `{key}` changed"));
                Ok(())
            }
        }
    }

    /// Every member of the base's `impl` must be the head's, but for dispatch calls in its
    /// methods; a new member is an edit, because it can override what the type does.
    fn compare_impl(&mut self, base: &syn::ItemImpl, head: &syn::ItemImpl) -> Result<(), String> {
        let name = impl_name(base);
        let path = self.code.path.clone();
        let mut after: BTreeMap<String, &syn::ImplItem> = BTreeMap::new();
        for member in &head.items {
            let _replaced = after.insert(member_key(&path, &self.head.text, member)?, member);
        }
        let mut before = BTreeSet::new();
        for member in &base.items {
            let key = member_key(&path, &self.base.text, member)?;
            let _new = before.insert(key.clone());
            let Some(head_member) = after.get(&key).copied() else {
                self.edit(&format!("`{key}` of `{name}` is changed or removed"));
                continue;
            };
            if self.normal_base(member.span())? == self.normal_head(head_member.span())? {
                continue;
            }
            if let (syn::ImplItem::Fn(old), syn::ImplItem::Fn(changed)) = (member, head_member) {
                self.compare_function(
                    &format!("{name}::{}", old.sig.ident),
                    Function { span: old.span(), block: &old.block },
                    Function { span: changed.span(), block: &changed.block },
                )?;
            } else {
                self.edit(&format!("`{key}` of `{name}` changed"));
            }
        }
        for key in after.keys().filter(|key| !before.contains(*key)) {
            self.edit(&format!(
                "the existing `{name}` has the new member `{key}`, which can override or \
                 shadow what the type does"
            ));
        }
        Ok(())
    }

    /// Every item of the base's test module must be the head's. New ones are additive, unless
    /// they use one of [`INPUT_MACROS`].
    fn compare_tests(&mut self, base: &syn::ItemMod, head: &syn::ItemMod) -> Result<(), String> {
        let mut before = BTreeSet::new();
        for item in module_items(base) {
            let _new = before.insert(self.normal_base(item.span())?);
        }
        let mut after = BTreeSet::new();
        for item in module_items(head) {
            let text = self.normal_head(item.span())?;
            if !before.contains(&text) {
                self.check_new_test(item)?;
            }
            let _new = after.insert(text);
        }
        for item in module_items(base) {
            if !after.contains(&self.normal_base(item.span())?) {
                let key = item_key(&self.code.path, &self.base.text, item)?;
                self.edit(&format!("the test module's `{key}` is changed or removed"));
            }
        }
        Ok(())
    }

    /// A new item of an existing test module is an edit if it uses one of [`INPUT_MACROS`].
    fn check_new_test(&mut self, item: &syn::Item) -> Result<(), String> {
        let tokens = tokens_at(&self.code.path, &self.head.text, item.span())?;
        let Some(input) = input_macro(tokens) else {
            return Ok(());
        };
        let key = item_key(&self.code.path, &self.head.text, item)?;
        self.edit(&format!(
            "the new test `{key}` uses `{input}!`, which pins the source tree or the environment \
             rather than a rule"
        ));
        Ok(())
    }

    /// The head's body must be the base's with dispatch calls inserted at its top level.
    fn compare_function(
        &mut self,
        name: &str,
        before: Function<'_>,
        after: Function<'_>,
    ) -> Result<(), String> {
        let path = self.code.path.clone();
        let base_open = before.block.brace_token.span.open();
        let head_open = after.block.brace_token.span.open();
        if header(&path, &self.base.text, before.span, base_open)?
            != header(&path, &self.head.text, after.span, head_open)?
        {
            self.edit(&format!("the signature or attributes of `{name}` changed"));
            return Ok(());
        }
        let mut index = 0;
        for statement in &after.block.stmts {
            let base_statement = before.block.stmts.get(index);
            if let Some(old) = base_statement
                && self.normal_base(old.span())? == self.normal_head(statement.span())?
            {
                index += 1;
                continue;
            }
            let sides = dispatch::Sides { base: &self.base.text, head: &self.head.text };
            let appended = base_statement
                .and_then(|old| dispatch::appended_lints(old, statement, sides, self.new));
            let found = match appended {
                Some(names) => {
                    index += 1;
                    Some(names)
                }
                None => dispatch::names(statement, &self.code.path, self.new),
            };
            let Some(names) = found else {
                self.edit(&format!(
                    "`{name}` changed beyond a dispatch call of a new rule: a statement of the \
                     head's at line {} is new or changed",
                    statement.span().start().line
                ));
                return Ok(());
            };
            self.record(name, statement, names);
        }
        if index != before.block.stmts.len() {
            self.edit(&format!("`{name}` changed: a statement of the base's is removed or moved"));
        }
        Ok(())
    }

    /// Records a dispatch call of `names` in `function`: its lines, and each name once.
    fn record(&mut self, function: &str, statement: &syn::Stmt, names: Vec<String>) {
        let line = |position: usize| {
            u32::try_from(position).expect("source files have fewer than 2^32 lines")
        };
        let span = statement.span();
        let lines = (line(span.start().line), line(span.end().line));
        assert!(lines.0 <= lines.1, "a statement ends after it starts");
        let ranges = self.class.dispatch.entry(self.code.path.clone()).or_default();
        ranges.push(lines);
        for name in names {
            if !self.dispatched.insert(name.clone()) {
                self.edit(&format!(
                    "`{function}` dispatches `{name}` a second time; a new rule has one dispatch \
                     call"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Class, classify};

    const RULES: &str = "xtask/src/tidy/rust_source.rs";

    fn real(relative: &str) -> String {
        std::fs::read_to_string(crate::workspace_root().join(relative)).unwrap()
    }

    /// `text` with the one occurrence of `from` replaced by `to`.
    fn replaced(text: &str, from: &str, to: &str) -> String {
        assert_eq!(text.matches(from).count(), 1, "the probe's anchor `{from}`");
        text.replacen(from, to, 1)
    }

    /// The class of a diff that turns each `(path, base, head)` from `base` into `head`; `None`
    /// for a file missing on that side.
    fn class_of(name: &str, files: &[(&str, Option<&str>, Option<&str>)]) -> Class {
        let scratch = crate::tidy::scratch::Scratch::new(name);
        for (path, base, head) in files {
            if let Some(text) = base {
                scratch.write(&format!("base/{path}"), text);
            }
            if let Some(text) = head {
                scratch.write(&format!("head/{path}"), text);
            }
        }
        let changed: Vec<String> = files.iter().map(|(path, _, _)| (*path).to_owned()).collect();
        let (base, head) = (scratch.root().join("base"), scratch.root().join("head"));
        classify(&base, &head, &changed).unwrap()
    }

    /// The class of the real `path` changed to `head`.
    fn edited(name: &str, path: &str, head: &str) -> Class {
        class_of(name, &[(path, Some(&real(path)), Some(head))])
    }

    /// Asserts that `class` is an edit whose first reason contains `expected`.
    fn assert_edit(class: &Class, expected: &str) {
        assert!(class.is_edit(), "an edit: {expected}");
        assert_eq!(class.name(), "edit");
        let first = class.edits.first().map_or("", String::as_str);
        assert!(first.contains(expected), "`{expected}` in {:?}", class.edits);
    }

    // Goal: round 4 of #57, thread 1: a new `Visit` override in the existing visitor, which turned
    // every rule off for a path, and a narrowed `report_unsafe`, are edits.
    #[test]
    fn a_new_visit_override_is_an_edit() {
        let base = real(RULES);
        let visit = "impl<'ast> Visit<'ast> for Visitor<'_> {\n";
        let skip = "    fn visit_item(&mut self, node: &'ast syn::Item) {\n        if \
                    !self.path.starts_with(\"crates/solver/src/io\") {\n            \
                    visit::visit_item(self, node);\n        }\n    }\n";
        let head = replaced(&base, visit, &format!("{visit}{skip}"));
        let class = edited("class-visit-item", RULES, &head);
        assert_edit(&class, "has the new member `fn visit_item`");
        let unsafe_check = "        if !self.path.starts_with(UNSAFE_ALLOWED_CRATE) {";
        let narrowed = "        if !self.path.starts_with(UNSAFE_ALLOWED_CRATE) && \
                        !self.path.starts_with(\"crates/solver/src/ffi\") {";
        let spared = replaced(&base, unsafe_check, narrowed);
        assert_edit(&edited("class-unsafe", RULES, &spared), "`Visitor::report_unsafe` changed");
    }

    // Goal: the exploits of round 3 of #57's review are edits: a private table of exceptions, an
    // early return in `check_path_start`, an exception inlined into its allowed list, and a
    // condition added in `visit_path`.
    #[test]
    fn the_round_three_exploits_are_edits() {
        let base = real(RULES);
        let start = "    fn check_path_start(&mut self, segment: &Ident) {\n";
        let early = "        if self.path.starts_with(\"crates/solver\") { return; }\n";
        let allowed = "segment == ban.segment && applies && !registry::is_allowed(ban.allowed, \
                       self.path)";
        let inline = "segment == ban.segment && applies && !registry::is_allowed(if ban.id == \
                      \"direct-libm\" { &[\"crates/kernels/src/math.rs\", \
                      \"crates/solver/src/lib.rs\"] } else { ban.allowed }, self.path)";
        let visit = "        if node.segments.len() > 1\n";
        let table = "const PATH_BAN_ROWS: &[(&str, &[&str])] = &[(\"direct-libm\", \
                     &[\"crates/solver\"])];";
        let probes = [
            (format!("{base}\n{table}\n"), "the new item `const PATH_BAN_ROWS` is a constant"),
            (replaced(&base, start, &format!("{start}{early}")), "`Visitor::check_path_start`"),
            (replaced(&base, allowed, inline), "`Visitor::check_path_start` changed"),
            (
                replaced(
                    &base,
                    visit,
                    "        if node.segments.len() > 1 && !self.path.ends_with(\"lib.rs\")\n",
                ),
                "`<Visitor as Visit>::visit_path` changed",
            ),
        ];
        for (index, (head, expected)) in probes.iter().enumerate() {
            assert_edit(&edited(&format!("class-round-three-{index}"), RULES, head), expected);
        }
    }

    // Goal: round 4, thread 2: a guard added as the first statement of `check_tolerance`, which
    // cargo-mutants' whole-body mutant "covered", is an edit.
    #[test]
    fn a_first_statement_guard_is_an_edit() {
        let first = "        if !self.scope.is_nami() || self.in_test_code() {";
        let guard = "        if self.path.starts_with(\"crates/solver/src/io\") { return; }\n";
        let head = replaced(&real(RULES), first, &format!("{guard}{first}"));
        let class = edited("class-guard", RULES, &head);
        assert_edit(&class, "`Visitor::check_tolerance` changed beyond a dispatch call");
    }

    // Goal: round 4, thread 3: a threshold scaled at its use site, with the unit test edited
    // along, is an edit, written inline or through a `let`; the edited test is one too.
    #[test]
    fn a_threshold_scaled_at_its_use_is_an_edit() {
        let base = real(RULES);
        let limit = "    value > 0.0 && value <= MAGIC_TOLERANCE_MAX\n";
        let scaled =
            replaced(&base, limit, "    value > 0.0 && value <= MAGIC_TOLERANCE_MAX * 1.0e-3\n");
        let retested = replaced(&scaled, "x < 0.000_000_5 }", "x < 0.000_000_000_5 }");
        let class = edited("class-scaled", RULES, &retested);
        assert_edit(&class, "`is_tolerance` changed");
        assert!(class.edits.iter().any(|edit| edit.contains("test module")), "{:?}", class.edits);
        let bound = "    let tolerance_max = MAGIC_TOLERANCE_MAX * 1.0e-3;\n    value > 0.0 && \
                     value <= tolerance_max\n";
        let head = replaced(&base, limit, bound);
        assert_edit(&edited("class-let", RULES, &head), "`is_tolerance` changed");
    }

    // Goal: round 4, thread 4: `function-length` lifted from every nami crate in `check_body` is
    // an edit.
    #[test]
    fn a_rule_lifted_from_the_nami_crates_is_an_edit() {
        let check = "        if lines > FUNCTION_LINES_MAX {";
        let lifted = "        if lines > FUNCTION_LINES_MAX && !self.scope.is_nami() {";
        let head = replaced(&real(RULES), check, lifted);
        let class = edited("class-length", RULES, &head);
        assert_edit(&class, "`Visitor::check_body` changed beyond a dispatch call");
    }

    // Goal: round 4, thread 5: `runtime-dependency` spared for `crates/kernels` in
    // `check_manifest` is an edit.
    #[test]
    fn a_rule_spared_for_one_crate_is_an_edit() {
        let manifest = "xtask/src/tidy/manifest.rs";
        let allowed = "    let is_allowed = |name: &str| name == \"libm\" || \
                       workspace_crates.iter().any(|ok| ok == name);";
        let spared = "    let is_kernels = relative.starts_with(\"crates/kernels\");\n    let \
                      is_allowed =\n        |name: &str| is_kernels || name == \"libm\" || \
                      workspace_crates.iter().any(|ok| ok == name);";
        let head = replaced(&real(manifest), allowed, spared);
        assert_edit(&edited("class-kernels", manifest, &head), "`check_manifest` changed");
    }

    // Goal: what the dialect used to hold to the base gives an edit now: a changed or new
    // constant or static, a new read of the environment, a new `impl` of an existing type, a
    // changed ban application, and the registry's forms outside its tables.
    #[test]
    fn changes_the_dialect_used_to_hold_are_edits() {
        let base = real(RULES);
        let probes = [
            (
                replaced(
                    &base,
                    "const MAGIC_TOLERANCE_MAX: f64 = 1.0e-6;",
                    "const MAGIC_TOLERANCE_MAX: f64 = 1.0e-9;",
                ),
                "the item `const MAGIC_TOLERANCE_MAX` changed",
            ),
            (format!("{base}\nconst LIMIT: u8 = 1;\n"), "is a constant or static"),
            (format!("{base}\nstatic LIMIT: u8 = 1;\n"), "is a constant or static"),
            (
                format!("{base}\nfn quiet() -> bool {{ std::env::var_os(\"CI\").is_some() }}\n"),
                "names `env`",
            ),
            (
                format!("{base}\nimpl Visitor<'_> {{ fn extra(&self) {{}} }}\n"),
                "is an `impl` that is not of a type new in this file",
            ),
            (format!("{base}\nstruct Vec;\n"), "is named `Vec`"),
            (
                format!("{base}\n#[export_name = \"x\"]\nfn extra() {{}}\n"),
                "the attribute `export_name`",
            ),
            (
                replaced(
                    &base,
                    "any(|banned| name == banned)",
                    "any(|banned| name == banned && name != \"debug_assert\")",
                ),
                "`Visitor::check_macro_name` changed",
            ),
            (
                replaced(&base, "fn check_macro_name(", "fn check_macro_names("),
                "`fn check_macro_name` of `Visitor` is changed or removed",
            ),
            (format!("{base}\nuse std::fmt::Write;\n"), "is an import into an existing module"),
        ];
        for (index, (head, expected)) in probes.iter().enumerate() {
            assert_edit(&edited(&format!("class-held-{index}"), RULES, head), expected);
        }
    }

    // Goal: round 5 of #57's review, thread 1: a new `struct Visitor` with its rule in
    // `text.rs` does not make an inherent `impl Visitor<'_>` in `rust_source.rs` additive, which
    // turned every rule off for a file starting with `#!`; the new rule in `text.rs` alone is.
    #[test]
    fn a_new_type_elsewhere_does_not_vouch_for_an_impl() {
        let text = "xtask/src/tidy/text.rs";
        let text_base = real(text);
        let rule = "\nstruct Visitor {\n    blank_run: u32,\n}\n\nfn \
                    check_blank_runs(lines: &[&str]) -> Vec<Violation> {\n    let run = \
                    Visitor { blank_run: 0 };\n    \
                    assert!(lines.len() < 1 << 30 || run.blank_run == 0, \"x\");\n    \
                    Vec::new()\n}\n";
        let end = "        }\n    }\n    violations\n}\n";
        let call = "        }\n    }\n    violations.extend(check_blank_runs(&lines));\n    \
                    violations\n}\n";
        let text_head = format!("{}{rule}", replaced(&text_base, end, call));
        let alone = edited("class-round-five-text", text, &text_head);
        assert_eq!(alone.edits, Vec::<String>::new());
        let rules_base = real(RULES);
        let shebang = "\nimpl Visitor<'_> {\n    fn visit_file(&mut self, file: &syn::File) \
                       {\n        if file.shebang.is_none() {\n            \
                       visit::visit_file(self, file);\n        }\n    }\n}\n";
        let rules_head = format!("{rules_base}{shebang}");
        let files = [
            (text, Some(text_base.as_str()), Some(text_head.as_str())),
            (RULES, Some(rules_base.as_str()), Some(rules_head.as_str())),
        ];
        let class = class_of("class-round-five-visitor", &files);
        assert_edit(&class, "rust_source.rs: the new item `impl Visitor < ' _ > #3` is an `impl`");
    }

    // Goal: round 5, thread 2: a blanket `impl<Quieted> Quiet for Quieted`, whose parameter
    // shadows the new struct of that name, is an edit; and so is any `impl` of a new type that
    // has generics, a path or arguments, or a trait that is neither new in its file nor allowed.
    // A plain `impl` of a new type of the same file, of a new trait or an allowed one, is
    // additive.
    #[test]
    fn only_plain_impls_of_new_types_are_additive() {
        let base = real(RULES);
        let new = "\ntrait Quiet {\n    fn quiet(&self) {}\n}\n\nstruct Quieted;\n";
        let refused = [
            "impl<Quieted> Quiet for Quieted {}",
            "impl<T> Quiet for Quieted where T: Copy {}",
            "impl Quiet for self::Quieted {}",
            "impl Quiet for Quieted<u8> {}",
            "impl<'a> Quiet for Quieted {}",
            "impl<'ast> Visit<'ast> for Quieted {}",
            "impl Iterator for Quieted { type Item = u8; fn next(&mut self) -> Option<u8> { \
             None } }",
            "impl std::fmt::Debug for Quieted where Self: Sized {}",
            "impl From<u8> for Quieted { fn from(_: u8) -> Self { Self } }",
        ];
        for (index, block) in refused.iter().enumerate() {
            let head = format!("{base}{new}\n{block}\n");
            let class = edited(&format!("class-impl-{index}"), RULES, &head);
            assert!(class.edits.iter().any(|edit| edit.contains("`impl")), "{block}: {class:?}");
        }
        let allowed = [
            "impl Quiet for Quieted {}",
            "impl Quieted {\n    fn new() -> Self {\n        Self\n    }\n}",
            "impl Clone for Quieted {\n    fn clone(&self) -> Self {\n        Self\n    }\n}",
            "impl std::fmt::Display for Quieted {\n    fn fmt(&self, f: &mut \
             std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"q\")\n    \
             }\n}",
        ];
        for (index, block) in allowed.iter().enumerate() {
            let head = format!("{base}{new}\n{block}\n");
            let class = edited(&format!("class-impl-ok-{index}"), RULES, &head);
            assert_eq!(class.edits, Vec::<String>::new(), "{block}");
        }
    }

    // Goal: an `impl`, a trait, a module or a `macro_rules!` nested in a new function's body,
    // at any depth, is an edit; a nested function or struct is not.
    #[test]
    fn items_nested_in_new_functions_are_edits() {
        let base = real(RULES);
        let nested = [
            (
                "fn extra() { impl Visitor<'_> { fn visit_file(&mut self, _: &syn::File) {} } }",
                "an `impl`",
            ),
            ("fn extra() { trait Hidden { fn hide(&self) {} } }", "a trait"),
            ("fn extra() { mod hidden {} }", "a module"),
            ("fn extra() { if true { macro_rules! hidden { () => {} } } }", "a `macro_rules!`"),
            ("struct Extra;\nimpl Extra {\n    fn extra() { impl Extra {} }\n}", "an `impl`"),
        ];
        for (index, (item, kind)) in nested.iter().enumerate() {
            let head = format!("{base}\n{item}\n");
            let class = edited(&format!("class-nested-{index}"), RULES, &head);
            assert_edit(&class, &format!("holds {kind} inside it"));
        }
        let plain = format!(
            "{base}\nfn extra() -> u8 {{\n    struct Inner;\n    fn inner() -> u8 {{ 1 \
             }}\n    let _ = Inner;\n    inner()\n}}\n"
        );
        assert_eq!(edited("class-nested-plain", RULES, &plain).edits, Vec::<String>::new());
    }

    // Goal: a new item, a test among them, may not use `include_str!`, `include_bytes!`,
    // `include!`, `file!`, `env!` or `option_env!`, in a new test module, a new test of an
    // existing test module, or a new function; any other new test is additive.
    #[test]
    fn new_items_may_not_read_the_source_tree() {
        let base = real(RULES);
        let test = "    const NONE: [&str; 0] = [];\n";
        for input in [
            "include_str!(file!())",
            "include_bytes!(\"a\")",
            "include!(\"a\")",
            "file!()",
            "env!(\"A\")",
            "option_env!(\"A\")",
        ] {
            let body =
                format!("    #[test]\n    fn pinned() {{\n        let _ = {input};\n    }}\n");
            let head = replaced(&base, test, &format!("{test}\n{body}"));
            let class = edited("class-input-test", RULES, &head);
            assert_edit(&class, "the new test `fn pinned` uses");
        }
        let plain = replaced(
            &base,
            test,
            &format!(
                "{test}\n    #[test]\n    fn plain() {{\n        assert!(NONE.is_empty(), \
                 \"x\");\n    }}\n"
            ),
        );
        assert_eq!(edited("class-input-plain", RULES, &plain).edits, Vec::<String>::new());
        let function = format!("{base}\nfn pinned() -> usize {{ include_str!(\"a\").len() }}\n");
        assert_edit(&edited("class-input-fn", RULES, &function), "uses `include_str!`");
        let path = "lints/src/extra.rs";
        let module = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn pinned() {\n        let _ \
                      = file!();\n    }\n}\n";
        assert_edit(&class_of("class-input-module", &[(path, None, Some(module))]), "uses `file!`");
    }

    // Goal: a dispatch call resolves its callee by path: a new `fn line_of` in another file does
    // not make a call of `rust_source.rs`'s existing `line_of` a dispatch; the module of a path
    // is found from the caller's file.
    #[test]
    fn dispatch_calls_resolve_by_path() {
        let base = real(RULES);
        let visit = "    visitor.visit_file(&file);\n";
        let call = "    visitor.violations.extend(line_of(&file));\n";
        let head = replaced(&base, visit, &format!("{visit}{call}"));
        let other = ("lints/src/rust_source.rs", None, Some("fn line_of(_: &u8) -> u8 { 1 }\n"));
        let files = [(RULES, Some(base.as_str()), Some(head.as_str())), other];
        assert_edit(&class_of("class-by-path", &files), "`check` changed beyond a dispatch call");
        let resolve = |caller: &str, path: &[&str]| {
            let segments: Vec<String> = path.iter().map(|segment| (*segment).to_owned()).collect();
            super::dispatch::module_file(caller, &segments)
        };
        let lib = "lints/src/lib.rs";
        assert_eq!(resolve(lib, &[]).as_deref(), Some(lib));
        assert_eq!(resolve(lib, &["extra"]).as_deref(), Some("lints/src/extra.rs"));
        assert_eq!(resolve(RULES, &[]).as_deref(), Some(RULES));
        assert_eq!(resolve(RULES, &["super", "text"]).as_deref(), Some("xtask/src/tidy/text.rs"));
        assert_eq!(
            resolve(RULES, &["crate", "tidy", "text"]).as_deref(),
            Some("xtask/src/tidy/text.rs")
        );
        assert_eq!(
            resolve(RULES, &["self", "inner"]).as_deref(),
            Some("xtask/src/tidy/rust_source/inner.rs")
        );
        assert_eq!(resolve(lib, &["super"]), None, "above the crate");
        assert_eq!(resolve(RULES, &["super", "crate"]), None, "`crate` only first");
        assert_eq!(resolve(RULES, &["inner", "self"]), None, "`self` only first");
        assert_eq!(resolve("README.md", &[]), None, "no crate");
    }

    // Goal: the registry's forms outside its tables that the dialect used to refuse are edits:
    // a variant renamed by an import or a constant, a changed scope rule, and a macro.
    #[test]
    fn registry_changes_outside_the_tables_are_edits() {
        let registry = super::super::source::REGISTRY;
        let base = real(registry);
        let scope = "use super::Scope;";
        let forms = [
            (
                replaced(
                    &base,
                    scope,
                    "use super::Scope;\nuse BanScope::NamiCrates as Everywhere;",
                ),
                "import",
            ),
            (
                replaced(
                    &base,
                    scope,
                    "use super::Scope;\nconst Everywhere: BanScope = BanScope::NamiCrates;",
                ),
                "constant",
            ),
            (
                replaced(&base, "self == Self::Everywhere || scope.is_nami()", "scope.is_nami()"),
                "changed",
            ),
            (
                format!("{base}\nmacro_rules! live {{ () => {{}} }}\nlive!();\n"),
                "is not a function",
            ),
        ];
        for (index, (head, expected)) in forms.iter().enumerate() {
            assert_edit(&edited(&format!("class-registry-{index}"), registry, head), expected);
        }
    }

    // Goal: an additive tidy rule is additive: a new function in a rule module, dispatched once
    // with shared references, beside a changed doc comment and a new unit test; the dispatch
    // call's lines are recorded. And a new function that nothing calls is additive too.
    #[test]
    fn a_new_rule_with_its_dispatch_call_is_additive() {
        let base = real(RULES);
        let visit = "    visitor.visit_file(&file);\n";
        let dispatch = "    visitor.violations.extend(extra_rule(&path, &file));\n";
        let rule = "\n/// A new rule.\nfn extra_rule(path: &Path, file: &syn::File) -> \
                    Vec<Violation> {\n    assert!(!file.items.is_empty() || path.has_root(), \
                    \"x\");\n    Vec::new()\n}\n";
        let test = "    const NONE: [&str; 0] = [];\n";
        let new_test = "    const NONE: [&str; 0] = [];\n\n    #[test]\n    fn extra() {}\n";
        let head = replaced(&base, visit, &format!("{visit}{dispatch}"));
        let head = replaced(&head, "/// Parses `text` and runs", "/// Parses `text`, and runs");
        let head = format!("{}{rule}", replaced(&head, test, new_test));
        let class = edited("class-additive", RULES, &head);
        assert_eq!(class.edits, Vec::<String>::new());
        assert_eq!(class.name(), "additive");
        let line =
            u32::try_from(base.lines().position(|l| l == visit.trim_end()).unwrap()).unwrap();
        assert_eq!(class.dispatch.get(RULES), Some(&vec![(line + 2, line + 2)]));
        let unused = format!("{base}\nfn spare() -> usize {{ PATH_BANS.len() }}\n");
        assert!(!edited("class-unused", RULES, &unused).is_edit(), "nothing calls it");
    }

    // Goal: a dispatch call that could reach existing state, or names nothing new, is an edit:
    // a `&mut` argument, a value argument, an existing function, and a second dispatch.
    #[test]
    fn look_alike_dispatch_calls_are_edits() {
        let base = real(RULES);
        let visit = "    visitor.visit_file(&file);\n";
        let rule = "\nfn extra_rule(_: &Path) -> Vec<Violation> {\n    Vec::new()\n}\n";
        let calls = [
            "    visitor.violations.extend(extra_rule(&mut visitor.violations));\n",
            "    visitor.violations.extend(extra_rule(path));\n",
            "    visitor.violations.extend(line_of(&file));\n",
            "    visitor.violations.extend(extra_rule(&path));\n    extra_rule(&path);\n",
            "    visitor.violations.clear();\n",
            "    visitor.violations.extend(extra_rule(&visitor.violations.split_off(0)));\n",
            "    visitor.take().extend(extra_rule(&path));\n",
            "    visitor.violations.extend(extra_rule(&<Path as Lookup>::ROOT));\n",
        ];
        for (index, call) in calls.iter().enumerate() {
            let head = format!("{}{rule}", replaced(&base, visit, &format!("{visit}{call}")));
            let class = edited(&format!("class-look-alike-{index}"), RULES, &head);
            assert!(class.is_edit(), "{call}");
            let found = class.edits.iter().any(|edit| edit.contains("`check`"));
            assert!(found, "{call}: {:?}", class.edits);
        }
    }

    // Goal: the parts of a new lint are additive: its own file, its declaration and pass in
    // `lib.rs`, the registration appended, and the pass registered; the module list in
    // `lib.rs`'s docs may change. Its `impl LateLintPass` is an edit: a visitor trait is not one
    // that a new type may implement on the ratchet alone (review of #57, round 5). Registering
    // it twice, another wrapping of the pass, or a new file that reads the environment, is an
    // edit.
    #[test]
    fn a_new_lint_is_additive_but_for_its_pass_impl() {
        let lib = "lints/src/lib.rs";
        let base = real(lib);
        let lints = "lint_store.register_lints(&[NAMI_RECURSION, NAMI_FLOAT_REDUCTION]);";
        let pass = "    lint_store.register_late_lint_pass(Box::new(|_| Box::new(NamiExtra)));\n";
        let declared = "\ndylint_linting::declare_lint! {\n    /// A new lint.\n    pub \
                        NAMI_EXTRA,\n    Deny,\n    \"a new lint\"\n}\n\n\
                        dylint_linting::declare_lint_pass!(NamiExtra => [NAMI_EXTRA]);\n";
        let pass_impl = "\nimpl<'tcx> LateLintPass<'tcx> for NamiExtra {\n    fn check_expr(&mut \
                         self, cx: &LateContext<'tcx>, expression: &'tcx Expr<'tcx>) {\n        \
                         extra::check_expr(cx, NAMI_EXTRA, expression);\n    }\n}\n";
        let head = replaced(&base, "mod recursion;", "mod extra;\nmod recursion;");
        let head = replaced(
            &head,
            "//! - `nami_recursion`",
            "//! - `nami_extra`: new.\n//! - `nami_recursion`",
        );
        let appended = lints.replace("NAMI_FLOAT_REDUCTION]", "NAMI_FLOAT_REDUCTION, NAMI_EXTRA]");
        let head = replaced(&head, &format!("    {lints}\n"), &format!("    {appended}\n{pass}"));
        let head = format!("{head}{declared}");
        let extra = "lints/src/extra.rs";
        let module = "use rustc_hir::Expr;\n\npub fn check_expr(_: &LateContext<'_>, _: &Lint, _: \
                      &Expr<'_>) {}\n";
        let class =
            class_of("class-lint", &[(lib, Some(&base), Some(&head)), (extra, None, Some(module))]);
        assert_eq!(class.edits, Vec::<String>::new());
        assert_eq!(class.dispatch.get(lib).map(Vec::len), Some(2), "{:?}", class.dispatch);
        let implemented = format!("{head}{pass_impl}");
        let implemented_files =
            [(lib, Some(base.as_str()), Some(implemented.as_str())), (extra, None, Some(module))];
        let with_impl = class_of("class-lint-impl", &implemented_files);
        assert_edit(&with_impl, "LateLintPass < ' tcx > for NamiExtra` is an `impl` that is not");
        assert_eq!(with_impl.edits.len(), 1, "{:?}", with_impl.edits);
        let twice = replaced(&head, pass, &format!("{pass}{pass}"));
        let doubled = class_of(
            "class-lint-twice",
            &[(lib, Some(&base), Some(&twice)), (extra, None, Some(module))],
        );
        assert_edit(&doubled, "a second time");
        let wrappings = [
            "Box::new(|_| Arc::new(NamiExtra))",
            "Arc::new(|_| Box::new(NamiExtra))",
            "Box::new(move |_| Box::new(NamiExtra))",
            "Box::new(async |_| Box::new(NamiExtra))",
            "Box::new(for<'a> |_| Box::new(NamiExtra))",
            "Box::new(#[inline] |_| Box::new(NamiExtra))",
        ];
        for wrapped in wrappings {
            let other = replaced(&head, "Box::new(|_| Box::new(NamiExtra))", wrapped);
            let files =
                [(lib, Some(base.as_str()), Some(other.as_str())), (extra, None, Some(module))];
            let wrapped_class = class_of("class-lint-wrapped", &files);
            assert_edit(&wrapped_class, "`register_lints` changed beyond a dispatch call");
        }
        let reading =
            format!("{module}fn quiet() -> bool {{ std::env::var_os(\"CI\").is_some() }}\n");
        let sensitive = class_of(
            "class-lint-env",
            &[(lib, Some(&base), Some(&head)), (extra, None, Some(&reading))],
        );
        assert_edit(&sensitive, "names `env`");
    }

    // Goal: a new file may import names, but a glob, at any depth of the import tree, is an edit,
    // because it can bring in names that shadow others.
    #[test]
    fn a_glob_import_is_an_edit() {
        let path = "lints/src/extra.rs";
        let plain = "use rustc_hir::{Expr, def::Res};\n";
        assert!(!class_of("class-import", &[(path, None, Some(plain))]).is_edit());
        for glob in ["use rustc_hir::*;\n", "use rustc_hir::{Expr, def::*};\n"] {
            let class = class_of("class-glob", &[(path, None, Some(glob))]);
            assert_edit(&class, "is an import into an existing module, or a glob");
        }
    }

    // Goal: the comparison leaves out doc comments, outer and inner, and nothing else: a changed
    // operator, an attribute or a delimiter changes the text; and a changed comparison in a
    // rule is an edit.
    #[test]
    fn only_doc_comments_are_left_out() {
        let normal = |text: &str| super::normal(text.parse().unwrap());
        assert_eq!(normal("/// A doc.\nfn a() {}"), normal("fn a() {}"));
        assert_eq!(normal("//! A doc.\n#![doc = \"x\"]\nfn a() {}"), normal("fn a() {}"));
        assert_eq!(normal("#[doc(hidden)]\nfn a() {}"), normal("fn a() {}"));
        let different = [
            ("fn a() -> bool { 1 > 2 }", "fn a() -> bool { 1 >= 2 }"),
            ("#[derive(Debug)]\nstruct A;", "struct A;"),
            ("#[inline] fn a() {}", "fn a() {}"),
            ("fn a() { (b) }", "fn a() { [b] }"),
            ("fn a() { b! }", "fn a() { b }"),
            ("#![no_std]\nfn a() {}", "fn a() {}"),
            ("m! { # ! (doc) }", "m! {}"),
            ("m! { # (doc) }", "m! {}"),
        ];
        for (before, after) in different {
            assert_ne!(normal(before), normal(after), "{before}");
        }
        let check = "        if lines > FUNCTION_LINES_MAX {";
        let head = replaced(&real(RULES), check, "        if lines >= FUNCTION_LINES_MAX {");
        assert_edit(&edited("class-operator", RULES, &head), "`Visitor::check_body` changed");
    }

    // Goal: a changed signature or attribute of an existing function is an edit, though its
    // body is the same; and a removed test module is named as such.
    #[test]
    fn headers_and_the_test_module_are_compared() {
        let base = real(RULES);
        let signature = "fn is_tolerance(value: f64) -> bool {";
        let inlined = replaced(&base, signature, &format!("#[inline]\n{signature}"));
        let expected = "the signature or attributes of `is_tolerance` changed";
        assert_edit(&edited("class-header", RULES, &inlined), expected);
        let start = base.find("#[cfg(test)]\nmod tests {").unwrap();
        let untested = base.get(..start).unwrap();
        let class = edited("class-no-tests", RULES, untested);
        assert_edit(&class, "the item `the test module` is changed or removed");
    }

    // Goal: a new module is additive only as `mod name;` in an existing file, for a new file; an
    // inline module, or a file module declared in a new file, is an edit.
    #[test]
    fn a_new_module_is_additive_only_as_a_new_file() {
        let lib = "lints/src/lib.rs";
        let base = real(lib);
        let declared = replaced(&base, "mod recursion;", "mod extra;\nmod recursion;");
        let new = ("lints/src/extra.rs", None, Some("fn extra() {}\n"));
        assert!(!class_of("class-mod", &[(lib, Some(&base), Some(&declared)), new]).is_edit());
        let inline = format!("{base}\nmod extra {{}}\n");
        assert_edit(&edited("class-mod-inline", lib, &inline), "is not a function");
        let nested = [("lints/src/extra.rs", None, Some("mod deeper;\n"))];
        assert_edit(&class_of("class-mod-nested", &nested), "is not a function");
    }

    // Goal: new fixtures and UI cases, and the regenerated rule list, are additive; a changed or
    // deleted fixture or UI case is an edit.
    #[test]
    fn data_files_are_additive_only_when_new() {
        let fixture = "xtask/fixtures/tidy/a.fires.fixture";
        let case = "lints/ui/a.stderr";
        let new = [
            (fixture, None, Some("x")),
            (case, None, Some("y")),
            ("docs/RULES.md", Some("a"), Some("b")),
        ];
        assert!(!class_of("class-data-new", &new).is_edit());
        assert_edit(
            &class_of("class-data-changed", &[(fixture, Some("x"), Some("z"))]),
            "is changed",
        );
        assert_edit(&class_of("class-data-deleted", &[(case, Some("y"), None)]), "is deleted");
    }
}
