//! The report lists that a dispatch call may extend: where an existing function of the base
//! collects the reports it returns or records (review of #57, round 6).
//!
//! `place.extend(call)` adds to whatever `place` is. On an allow-list or an exemption set it
//! loosens the rule that reads it: `names.extend(vendored(&root))` in `workspace_crates` would
//! let a nami crate depend on any crate it names. So the place must be a list of reports, read
//! from the base's source with `syn`, and the function's own:
//!
//! - a parameter of type `&mut Vec<Violation>` or `Vec<Violation>`, into which it reports;
//! - a field `self.name` of type `Vec<Violation>` in the declaration of the method's own type,
//!   into which its methods report;
//! - a local binding declared at the top of the body as `let name: Vec<Violation> = ..`, or
//!   `let name = ..` in a function that returns `Vec<Violation>` and ends in `name`; either way
//!   the function returns it;
//! - a field `local.name` of type `Vec<Violation>`, where `local` is declared at the top of the
//!   body as a struct literal of a type declared in the file, and the function's final
//!   expression returns `local.name`, as `rust_source::check` returns
//!   `Report { violations: visitor.violations }`.
//!
//! A place whose type cannot be read so is no report list, and the dispatch call is an edit.

use std::collections::BTreeSet;
use syn::visit::Visit;

/// The type of a report: a list of these is a report list.
const REPORT: &str = "Violation";

/// The report lists of the base function `signature` and `block`, a method of the type named
/// `owner` or a free function, in the base file `file`, as dotted places: `violations`,
/// `self.violations`, `visitor.violations`.
#[must_use]
pub fn report_lists(
    signature: &syn::Signature,
    block: &syn::Block,
    owner: Option<&str>,
    file: &syn::File,
) -> BTreeSet<String> {
    let mut lists = BTreeSet::new();
    for input in &signature.inputs {
        if let syn::FnArg::Typed(typed) = input
            && let syn::Pat::Ident(binding) = &*typed.pat
            && (is_report_list(&typed.ty) || is_mut_report_list(&typed.ty))
        {
            let _new = lists.insert(binding.ident.to_string());
        }
    }
    if let Some(owner) = owner
        && signature.receiver().is_some()
    {
        lists.extend(report_fields(owner, file).into_iter().map(|field| format!("self.{field}")));
    }
    let returned = returned_places(block);
    let returns_list =
        matches!(&signature.output, syn::ReturnType::Type(_, ty) if is_report_list(ty));
    for statement in &block.stmts {
        let syn::Stmt::Local(local) = statement else {
            continue;
        };
        for place in local_lists(local, file, returns_list, returned.tail.as_deref()) {
            if returned.places.contains(&place) {
                let _new = lists.insert(place);
            }
        }
    }
    lists
}

/// The places that `local` declares and that may be report lists: a typed or, in a function
/// returning a report list whose final expression is the binding, untyped binding; and the
/// report fields of a struct literal of a type of `file`.
fn local_lists(
    local: &syn::Local,
    file: &syn::File,
    returns_list: bool,
    tail: Option<&str>,
) -> Vec<String> {
    let Some((ident, declared)) = binding(&local.pat) else {
        return Vec::new();
    };
    let name = ident.to_string();
    if declared.is_some_and(is_report_list)
        || (declared.is_none() && returns_list && tail == Some(name.as_str()))
    {
        return vec![name];
    }
    let Some(init) = &local.init else {
        return Vec::new();
    };
    let syn::Expr::Struct(literal) = &*init.expr else {
        return Vec::new();
    };
    let Some(type_name) = literal.path.get_ident() else {
        return Vec::new();
    };
    report_fields(&type_name.to_string(), file)
        .into_iter()
        .map(|field| format!("{name}.{field}"))
        .collect()
}

/// The name a `let` pattern binds, and its declared type if it has one: `name` or
/// `name: Type`, with or without `mut`. `None` for any other pattern.
fn binding(pattern: &syn::Pat) -> Option<(&syn::Ident, Option<&syn::Type>)> {
    if let syn::Pat::Ident(binding) = pattern {
        return Some((&binding.ident, None));
    }
    let syn::Pat::Type(typed) = pattern else {
        return None;
    };
    let syn::Pat::Ident(binding) = &*typed.pat else {
        return None;
    };
    Some((&binding.ident, Some(&*typed.ty)))
}

/// The fields of type `Vec<Violation>` of the struct `owner` declared in `file`.
fn report_fields(owner: &str, file: &syn::File) -> Vec<String> {
    let declared = file.items.iter().find_map(|item| {
        let syn::Item::Struct(declared) = item else {
            return None;
        };
        (declared.ident == owner).then_some(declared)
    });
    let Some(declared) = declared else {
        return Vec::new();
    };
    declared
        .fields
        .iter()
        .filter(|field| is_report_list(&field.ty))
        .filter_map(|field| field.ident.as_ref().map(ToString::to_string))
        .collect()
}

/// True for exactly `Vec<Violation>`.
#[must_use]
pub fn is_report_list(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    let [segment] = path.path.segments.iter().collect::<Vec<_>>()[..] else {
        return false;
    };
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    let [syn::GenericArgument::Type(syn::Type::Path(item))] =
        arguments.args.iter().collect::<Vec<_>>()[..]
    else {
        return false;
    };
    let plain = path.qself.is_none() && item.qself.is_none() && path.path.leading_colon.is_none();
    plain && segment.ident == "Vec" && item.path.is_ident(REPORT)
}

/// True for exactly `&mut Vec<Violation>`.
fn is_mut_report_list(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Reference(reference)
        if reference.mutability.is_some() && is_report_list(&reference.elem))
}

/// What the function's final expression returns: every place it names, and the place it is,
/// if it is one.
struct Returned {
    places: BTreeSet<String>,
    tail: Option<String>,
}

/// The places named in `block`'s final expression, which the function returns.
fn returned_places(block: &syn::Block) -> Returned {
    let Some(syn::Stmt::Expr(tail, None)) = block.stmts.last() else {
        return Returned { places: BTreeSet::new(), tail: None };
    };
    let mut finder = Places { found: BTreeSet::new() };
    finder.visit_expr(tail);
    Returned { places: finder.found, tail: place_name(tail) }
}

/// Collects every place an expression names.
struct Places {
    found: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Places {
    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        if let Some(place) = place_name(node) {
            let _new = self.found.insert(place);
        }
        syn::visit::visit_expr(self, node);
    }
}

/// A path of one name, or a chain of named fields on one, as dotted text: `violations`,
/// `self.violations`, `visitor.violations`. `None` for any other expression.
#[must_use]
pub fn place_name(expression: &syn::Expr) -> Option<String> {
    let mut fields = Vec::new();
    let mut current = expression;
    for _ in 0..super::TOKENS_MAX {
        if let syn::Expr::Path(path) = current {
            if !path.attrs.is_empty() || path.qself.is_some() {
                return None;
            }
            fields.push(path.path.get_ident()?.to_string());
            fields.reverse();
            return Some(fields.join("."));
        }
        let syn::Expr::Field(field) = current else {
            return None;
        };
        let syn::Member::Named(name) = &field.member else {
            return None;
        };
        if !field.attrs.is_empty() {
            return None;
        }
        fields.push(name.to_string());
        current = &field.base;
    }
    None
}
