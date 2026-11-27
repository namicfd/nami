//! The report lists that a dispatch call may extend: where an existing function of the base
//! collects the reports it returns or records, and does nothing else with them (review of #57,
//! rounds 6 and 7).
//!
//! `place.extend(call)` adds to whatever `place` is. On an allow-list or an exemption set it
//! loosens the rule that reads it: `names.extend(vendored(&root))` in `workspace_crates` would
//! let a nami crate depend on any crate it names. And on a list that the base later sorts and
//! dedups, or filters, the new reports can remove existing ones. So the place must be a list
//! of reports, read from the base's source with `syn`, to which the base only adds:
//!
//! - `Violation` is the crate's own type: the file imports it without a rename, or declares it,
//!   and no import renames anything to it;
//! - the place is a parameter of type `&mut Vec<Violation>`; a field `self.name` declared
//!   `Vec<Violation>` in the method's own type; a local declared `let name: Vec<Violation> =
//!   ..`, or `let name = ..` in a function that returns `Vec<Violation>` and ends in `name`; or
//!   a field `local.name` of that type, where `local` is a struct literal of a type of the file;
//! - every other use of the place in the base function is the receiver of `push` or `extend`,
//!   or the value returned, as the final expression or a field of a struct literal that is the
//!   final expression (`Report { violations: visitor.violations }`); a local must be returned;
//! - for a field, every access of a field of that name anywhere in the file outside its tests
//!   is such a use too, so no method of the type, nor any function handed the value, does more;
//! - no macro in the base function, or for a field in the file, mentions the place.
//!
//! Any `retain`, `dedup`, `sort`, `clear`, `truncate`, `drain`, `filter`, indexing or `&mut`
//! borrow of it makes it no report list, and the dispatch call an edit; so does a place whose
//! type cannot be read so.

use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use syn::visit::Visit;

/// The type of a report: a list of these is a report list.
const REPORT: &str = "Violation";

/// The methods that only add to a list.
const APPENDS: [&str; 2] = ["push", "extend"];

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
    if !is_own_report_type(file) {
        return lists;
    }
    for input in &signature.inputs {
        if let syn::FnArg::Typed(typed) = input
            && let syn::Pat::Ident(binding) = &*typed.pat
            && is_mut_report_list(&typed.ty)
            && only_appended(block, &binding.ident.to_string(), false)
        {
            let _new = lists.insert(binding.ident.to_string());
        }
    }
    if let Some(owner) = owner
        && signature.receiver().is_some()
    {
        for field in report_fields(owner, file) {
            let place = format!("self.{field}");
            if only_appended(block, &place, false) && field_only_appended(file, &field) {
                let _new = lists.insert(place);
            }
        }
    }
    let returns_list =
        matches!(&signature.output, syn::ReturnType::Type(_, ty) if is_report_list(ty));
    let tail = tail_of(block).and_then(place_name);
    for statement in &block.stmts {
        let syn::Stmt::Local(local) = statement else {
            continue;
        };
        for (place, field) in local_lists(local, file, returns_list, tail.as_deref()) {
            let fields_hold = field.is_none_or(|field| field_only_appended(file, &field));
            if only_appended(block, &place, true) && fields_hold {
                let _new = lists.insert(place);
            }
        }
    }
    lists
}

/// True if `Violation` is the crate's own type in `file`: imported without a rename or
/// declared there, and nothing renamed to it.
fn is_own_report_type(file: &syn::File) -> bool {
    let mut named = false;
    for item in &file.items {
        if let syn::Item::Struct(declared) = item {
            named |= declared.ident == REPORT;
        }
        let syn::Item::Use(import) = item else {
            continue;
        };
        let mut pending = vec![&import.tree];
        for _ in 0..super::TOKENS_MAX {
            let Some(tree) = pending.pop() else {
                break;
            };
            match tree {
                syn::UseTree::Name(name) => named |= name.ident == REPORT,
                syn::UseTree::Rename(rename) if rename.rename == REPORT => return false,
                syn::UseTree::Path(path) => pending.push(&path.tree),
                syn::UseTree::Group(group) => pending.extend(group.items.iter()),
                syn::UseTree::Rename(_) | syn::UseTree::Glob(_) => {}
            }
        }
    }
    named
}

/// The places that `local` declares and that may be report lists, each with the field it is,
/// if it is a field: a typed or, in a function returning a report list whose final expression
/// is the binding, untyped binding; and the report fields of a struct literal of a type of
/// `file`.
fn local_lists(
    local: &syn::Local,
    file: &syn::File,
    returns_list: bool,
    tail: Option<&str>,
) -> Vec<(String, Option<String>)> {
    let Some((ident, declared)) = binding(&local.pat) else {
        return Vec::new();
    };
    let name = ident.to_string();
    if declared.is_some_and(is_report_list)
        || (declared.is_none() && returns_list && tail == Some(name.as_str()))
    {
        return vec![(name, None)];
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
        .map(|field| (format!("{name}.{field}"), Some(field)))
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

/// The final expression of `block`, which the function returns.
fn tail_of(block: &syn::Block) -> Option<&syn::Expr> {
    let Some(syn::Stmt::Expr(tail, None)) = block.stmts.last() else {
        return None;
    };
    Some(tail)
}

/// How many times the final expression of `block` returns a value that `matches`: as the
/// expression itself, or as a field value of a struct literal that is the expression.
fn returned(block: &syn::Block, matches: &dyn Fn(&syn::Expr) -> bool) -> usize {
    let Some(tail) = tail_of(block) else {
        return 0;
    };
    if matches(tail) {
        return 1;
    }
    let syn::Expr::Struct(literal) = tail else {
        return 0;
    };
    literal.fields.iter().filter(|field| matches(&field.expr)).count()
}

/// True if every use of `place` in `block` is the receiver of `push` or `extend`, or a value
/// returned; if `must_return`, the place is returned too; and no macro in `block` mentions it.
fn only_appended(block: &syn::Block, place: &str, must_return: bool) -> bool {
    let is_place = |expression: &syn::Expr| place_name(expression).as_deref() == Some(place);
    let mut uses = Uses { matches: &is_place, all: 0, appended: 0 };
    uses.visit_block(block);
    let segments: Vec<&str> = place.split('.').collect();
    let mentioned = macro_mentions_in(block, &segments);
    let returned = returned(block, &is_place);
    let accounted = uses.all == uses.appended + returned;
    accounted && !mentioned && (!must_return || returned > 0)
}

/// True if every access of a field named `field` outside the test module of `file` is the
/// receiver of `push` or `extend`, or a value its function returns, and no macro there
/// mentions `.field`: so no method of the type, nor any function that is handed a value of it,
/// does anything else with the list.
fn field_only_appended(file: &syn::File, field: &str) -> bool {
    let is_field = |expression: &syn::Expr| {
        matches!(expression, syn::Expr::Field(access)
            if matches!(&access.member, syn::Member::Named(name) if name == field))
    };
    let mut uses = Uses { matches: &is_field, all: 0, appended: 0 };
    let mut returned_count = 0;
    let mut mentioned = false;
    for item in &file.items {
        if matches!(item, syn::Item::Mod(module) if super::dialect::is_test_module(module)) {
            continue;
        }
        uses.visit_item(item);
        for block in function_blocks(item) {
            returned_count += returned(block, &is_field);
        }
        mentioned |= macro_mentions_item(item, &["", field]);
    }
    uses.all == uses.appended + returned_count && !mentioned
}

/// The bodies of the functions that `item` is or holds as `impl` members.
fn function_blocks(item: &syn::Item) -> Vec<&syn::Block> {
    if let syn::Item::Fn(function) = item {
        return vec![&function.block];
    }
    let syn::Item::Impl(block) = item else {
        return Vec::new();
    };
    block
        .items
        .iter()
        .filter_map(|member| {
            let syn::ImplItem::Fn(method) = member else {
                return None;
            };
            Some(&method.block)
        })
        .collect()
}

/// Counts the uses of a place, and how many of them receive `push` or `extend`.
struct Uses<'m> {
    matches: &'m dyn Fn(&syn::Expr) -> bool,
    all: usize,
    appended: usize,
}

impl<'ast> Visit<'ast> for Uses<'_> {
    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        if (self.matches)(node) {
            self.all += 1;
        }
        if let syn::Expr::MethodCall(call) = node
            && APPENDS.iter().any(|append| call.method == append)
            && (self.matches)(&call.receiver)
        {
            self.appended += 1;
        }
        syn::visit::visit_expr(self, node);
    }
}

/// True if a macro in `block` mentions the place `segments`, such as `violations` or
/// `self.violations`.
fn macro_mentions_in(block: &syn::Block, segments: &[&str]) -> bool {
    let mut finder = Mentions { segments, found: false };
    finder.visit_block(block);
    finder.found
}

/// True if a macro in `item` mentions the dotted `segments`; an empty first segment matches
/// any name, so `["", "violations"]` finds any `x.violations`.
fn macro_mentions_item(item: &syn::Item, segments: &[&str]) -> bool {
    let mut finder = Mentions { segments, found: false };
    finder.visit_item(item);
    finder.found
}

/// Finds a mention of a dotted place in the tokens of a macro.
struct Mentions<'s> {
    segments: &'s [&'s str],
    found: bool,
}

impl<'ast> Visit<'ast> for Mentions<'_> {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.found |= mentions(node.tokens.clone(), self.segments);
    }
}

/// True if `stream` holds the names of `segments` joined by `.`, at any depth.
fn mentions(stream: TokenStream, segments: &[&str]) -> bool {
    let mut pending: Vec<Vec<TokenTree>> = vec![stream.into_iter().collect()];
    let width = segments.len() * 2 - 1;
    for _ in 0..super::TOKENS_MAX {
        let Some(sequence) = pending.pop() else {
            return false;
        };
        let found = sequence.windows(width).any(|window| {
            window.iter().enumerate().all(|(index, tree)| match (index % 2, tree) {
                (0, TokenTree::Ident(name)) => {
                    let wanted = segments.get(index / 2).copied().unwrap_or_default();
                    wanted.is_empty() || name == wanted
                }
                (1, TokenTree::Punct(dot)) => dot.as_char() == '.',
                _ => false,
            })
        });
        if found {
            return true;
        }
        pending.extend(sequence.into_iter().filter_map(|tree| match tree {
            TokenTree::Group(group) => Some(group.stream().into_iter().collect()),
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => None,
        }));
    }
    panic!("a macro has more than {} token sequences", super::TOKENS_MAX);
}

/// A path of one name, or a chain of named fields on one, as dotted text: `violations`,
/// `self.violations`, `visitor.violations`. `None` for any other expression.
#[must_use]
pub fn place_name(expression: &syn::Expr) -> Option<String> {
    let mut fields = Vec::new();
    let mut current = expression;
    for _ in 0..super::TOKENS_MAX {
        if let syn::Expr::Path(path) = current {
            // A qualified path (`<A>::c`, `<A as B>::c`) has a leading `::` or two segments, so
            // `get_ident` refuses it.
            if !path.attrs.is_empty() {
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
