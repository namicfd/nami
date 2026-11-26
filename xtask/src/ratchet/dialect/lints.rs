//! The dialect of the lint library, and its declarations and registrations read from it.
//!
//! The rule-set check needs each enforced lint's level and its registration. rustc compiles
//! `lints/src/lib.rs` and the files it declares with `mod`, so those are exactly the files
//! there may be. A declaration is read only in the literal form `pub NAME, Level,
//! "description"` of `declare_lint!` or `declare_tool_lint!` at the top of a file, which is what
//! the macro's level and name then are. Nothing else may bind a declared name, so a
//! registration names the declaration that was read.

use super::{identifiers, is_cfg_test, is_doc, outside, parse, path_text};
use crate::tidy::lints::DylintLint;
use std::collections::BTreeSet;
use std::path::Path;
use syn::parse::{Parse, ParseStream};
use syn::visit::{self, Visit};

/// The lint library's directory and root module.
const SOURCES: &str = "lints/src/";
const ROOT: &str = "lints/src/lib.rs";

/// The macros that declare a lint, in the form the ratchet reads.
const DECLARING: [&str; 2] = ["declare_lint", "declare_tool_lint"];

/// The macros that may appear besides them: they declare no lint. Any other macro whose name
/// says `lint` gives no verdict, because it may declare one.
const LINT_MACROS: [&str; 2] = ["declare_lint_pass", "dylint_library"];

/// The macros that bring in code or items the reader cannot see.
const HIDING: [&str; 4] = ["include", "include_str", "include_bytes", "macro_rules"];

/// The levels rustc knows.
const LEVELS: [&str; 4] = ["Allow", "Warn", "Deny", "Forbid"];

/// What the lint library declares and registers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    /// Every declaration, sorted.
    pub declarations: Vec<DylintLint>,
    /// The lower-case names passed to `register_lints(&[..])`.
    pub registered: BTreeSet<String>,
    /// The files rustc compiles, relative to the root.
    pub files: Vec<String>,
}

/// Reads the lint library below `root` after checking it against the dialect. A tree without
/// `lints/src/lib.rs` and without any file below `lints/src/` has an empty library.
///
/// # Errors
///
/// No verdict: a file that cannot be read or is outside the dialect.
pub fn library(root: &Path) -> Result<Library, String> {
    let read = |relative: &str| super::super::source::read_optional(root, relative);
    let present: BTreeSet<String> = super::super::tree::entries(root)
        .map_err(|error| format!("cannot read the tree: {error}"))?
        .into_keys()
        .filter(|path| {
            path.starts_with(SOURCES)
                && Path::new(path).extension().is_some_and(|extension| extension == "rs")
        })
        .collect();
    let Some(root_text) = read(ROOT)? else {
        if present.is_empty() {
            return Ok(Library::default());
        }
        return Err(outside(SOURCES, "Rust files without `lib.rs`"));
    };
    let root_file = parse(ROOT, &root_text)?;
    let declared: Vec<String> = root_file
        .items
        .iter()
        .filter_map(|item| {
            if let syn::Item::Mod(module) = item
                && module.content.is_none()
            {
                return Some(module.ident.to_string());
            }
            None
        })
        .collect();
    let mut files = vec![(ROOT.to_owned(), root_file)];
    for name in declared {
        let relative = format!("{SOURCES}{name}.rs");
        let text = read(&relative)?.ok_or_else(|| format!("{relative} is declared and missing"))?;
        files.push((relative.clone(), parse(&relative, &text)?));
    }
    let compiled: BTreeSet<String> = files.iter().map(|(relative, _)| relative.clone()).collect();
    if let Some(stray) = present.difference(&compiled).next() {
        return Err(outside(
            stray,
            "a Rust file that `lib.rs` does not declare, so rustc never compiles it",
        ));
    }
    read_library(&files)
}

/// Checks every compiled file and reads its declarations and registrations.
fn read_library(files: &[(String, syn::File)]) -> Result<Library, String> {
    let mut library = Library::default();
    for (relative, file) in files {
        for item in &file.items {
            if let syn::Item::Macro(item) = item
                && is_declaring(&item.mac)
            {
                library.declarations.push(declaration(relative, item)?);
            }
        }
    }
    let names: BTreeSet<String> =
        library.declarations.iter().map(|lint| lint.name.to_uppercase()).collect();
    let mut calls = 0_usize;
    for (relative, file) in files {
        let mut reader = Reader {
            names: &names,
            root: relative == ROOT,
            depth: 0,
            registered: BTreeSet::new(),
            calls: 0,
            problem: None,
        };
        reader.visit_file(file);
        if let Some(problem) = reader.problem {
            return Err(outside(relative, &problem));
        }
        library.registered.extend(reader.registered);
        calls += reader.calls;
        library.files.push(relative.clone());
    }
    let unconditional = files
        .iter()
        .find(|(relative, _)| relative == ROOT)
        .map_or(0, |(_, file)| unconditional_registrations(file));
    if calls != unconditional {
        return Err(outside(
            ROOT,
            "a `register_lints` call that is not an unconditional statement of the function \
             `register_lints` at the top of `lib.rs`",
        ));
    }
    library.declarations.sort();
    Ok(library)
}

/// The `register_lints(..)` calls that are statements of their own at the top of the body of
/// the top-level function `register_lints`, which dylint calls, with no statement before them
/// that can leave the function (a `return`, a `?` or a macro): they always run.
fn unconditional_registrations(file: &syn::File) -> usize {
    let registering = file.items.iter().filter_map(|item| {
        let syn::Item::Fn(function) = item else {
            return None;
        };
        (function.sig.ident == "register_lints").then_some(function)
    });
    let mut count = 0_usize;
    for function in registering {
        let mut exits = Exits { found: false };
        for statement in &function.block.stmts {
            let registers = matches!(statement,
                syn::Stmt::Expr(syn::Expr::MethodCall(call), Some(_))
                    if call.method == "register_lints");
            if registers && !exits.found {
                count += 1;
            }
            exits.visit_stmt(statement);
        }
    }
    count
}

/// Finds a way out of a function, or code the reader cannot see: a `return`, a `?` or a macro.
struct Exits {
    found: bool,
}

impl<'ast> Visit<'ast> for Exits {
    fn visit_expr_return(&mut self, node: &'ast syn::ExprReturn) {
        self.found = true;
        visit::visit_expr_return(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.found = true;
        visit::visit_expr_try(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.found = true;
        visit::visit_macro(self, node);
    }
}

/// True for `declare_lint!` or `declare_tool_lint!`, by the last segment of the path.
fn is_declaring(mac: &syn::Macro) -> bool {
    mac.path.segments.last().is_some_and(|name| DECLARING.iter().any(|known| name.ident == known))
}

/// The lint that a top-level declaring macro declares, in the literal form only.
fn declaration(relative: &str, item: &syn::ItemMacro) -> Result<DylintLint, String> {
    if !item.attrs.is_empty() {
        return Err(outside(relative, "an attribute on a lint declaration"));
    }
    let parsed: Declaration = syn::parse2(item.mac.tokens.clone()).map_err(|_| {
        outside(relative, "a lint declaration not in the form `pub NAME, Level, \"description\"`")
    })?;
    Ok(parsed.0)
}

/// `pub NAME, Level, "description"` with doc comments before it, as one declaration.
struct Declaration(DylintLint);

impl Parse for Declaration {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attributes = input.call(syn::Attribute::parse_outer)?;
        if !attributes.iter().all(is_doc) {
            return Err(input.error("only doc comments"));
        }
        let _public: syn::Token![pub] = input.parse()?;
        let name: syn::Ident = input.parse()?;
        let _after_name: syn::Token![,] = input.parse()?;
        let level: syn::Ident = input.parse()?;
        let _after_level: syn::Token![,] = input.parse()?;
        let description: syn::LitStr = input.parse()?;
        let _trailing: Option<syn::Token![,]> = input.parse()?;
        if !input.is_empty()
            || !LEVELS.iter().any(|known| level == known)
            || !description.suffix().is_empty()
        {
            return Err(input.error("not the literal form"));
        }
        Ok(Self(DylintLint {
            name: name.to_string().to_lowercase(),
            level: level.to_string(),
            description: description.value(),
        }))
    }
}

struct Reader<'n> {
    /// The declared names, in upper case as written.
    names: &'n BTreeSet<String>,
    /// True for `lib.rs`, the only file that may declare file modules.
    root: bool,
    /// Depth of items around the current one; 0 at the top of the file.
    depth: u32,
    registered: BTreeSet<String>,
    /// The `register_lints` calls seen.
    calls: usize,
    problem: Option<String>,
}

impl Reader<'_> {
    fn flag(&mut self, problem: String) {
        if self.problem.is_none() {
            self.problem = Some(problem);
        }
    }

    fn binding(&mut self, ident: &syn::Ident, what: &str) {
        if self.names.contains(&ident.to_string()) {
            self.flag(format!("{what} named `{ident}`, like a declared lint"));
        }
    }
}

impl Reader<'_> {
    /// What is wrong with an item in its position, if anything.
    fn item_problem(&self, node: &syn::Item) -> Option<String> {
        if let syn::Item::Mod(module) = node {
            let file_module = module.content.is_none();
            if file_module && !(self.root && self.depth == 1) {
                return Some(format!("the file module `{}` outside `lib.rs`", module.ident));
            }
            if module.attrs.iter().any(is_cfg_test) && module.ident != "tests" {
                return Some(format!("`#[cfg(test)]` on the module `{}`", module.ident));
            }
        }
        if let syn::Item::Fn(function) = node
            && function.attrs.iter().any(is_cfg_test)
        {
            return Some(format!("`#[cfg(test)]` on the function `{}`", function.sig.ident));
        }
        if let syn::Item::Macro(item) = node
            && is_declaring(&item.mac)
            && self.depth != 1
        {
            return Some("a lint declaration below the top of its file".to_owned());
        }
        None
    }
}

impl<'ast> Visit<'ast> for Reader<'_> {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        let path = node.path();
        if path.is_ident("path")
            || path.is_ident("cfg_attr")
            || (path.is_ident("cfg") && !is_cfg_test(node))
        {
            self.flag(format!("the attribute `#[{}]`", path_text(path)));
        }
        visit::visit_attribute(self, node);
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        self.depth += 1;
        if let Some(problem) = self.item_problem(node) {
            self.flag(problem);
        }
        visit::visit_item(self, node);
        self.depth -= 1;
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        let declaring = node
            .mac
            .path
            .segments
            .last()
            .is_some_and(|name| DECLARING.iter().any(|d| name.ident == d));
        if declaring {
            self.flag("a lint declaration inside a function".to_owned());
        }
        visit::visit_stmt_macro(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let Some(name) = node.path.segments.last().map(|segment| segment.ident.to_string()) else {
            return;
        };
        let declares = name.contains("lint")
            && !DECLARING.contains(&name.as_str())
            && !LINT_MACROS.contains(&name.as_str());
        if declares || HIDING.contains(&name.as_str()) {
            self.flag(format!("the macro `{name}!`"));
        }
        let included = identifiers(node.tokens.clone())
            .into_iter()
            .find(|ident| ident == "include" || ident.to_string().starts_with("include_"));
        if let Some(ident) = included
            && !DECLARING.contains(&name.as_str())
        {
            self.flag(format!("`{ident}` inside the macro `{name}!`"));
        }
        visit::visit_macro(self, node);
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        self.flag(format!("the renamed import `{}`", node.rename));
        visit::visit_use_rename(self, node);
    }

    fn visit_use_name(&mut self, node: &'ast syn::UseName) {
        self.binding(&node.ident, "an import");
        visit::visit_use_name(self, node);
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

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == "register_lints" {
            self.calls += 1;
            match registered_names(&node.args) {
                Some(names) if self.root => self.registered.extend(names),
                _ => self.flag(
                    "a `register_lints` call other than `register_lints(&[NAME, ..])` in `lib.rs`"
                        .to_owned(),
                ),
            }
        }
        visit::visit_expr_method_call(self, node);
    }
}

/// The lower-case names of `register_lints(&[NAME, ..])`'s one argument, if it has that form.
fn registered_names(
    arguments: &syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>,
) -> Option<Vec<String>> {
    let [syn::Expr::Reference(reference)] = arguments.iter().collect::<Vec<_>>().as_slice() else {
        return None;
    };
    let syn::Expr::Array(array) = &*reference.expr else {
        return None;
    };
    let names: Option<Vec<String>> = array
        .elems
        .iter()
        .map(|element| {
            let syn::Expr::Path(path) = element else {
                return None;
            };
            path.path.get_ident().map(ToString::to_string)
        })
        .collect();
    names.map(|names| names.iter().map(|name| name.to_lowercase()).collect())
}
