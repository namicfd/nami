//! The dispatch calls of an additive diff: the statements that connect a new rule to the
//! existing code that runs the rules.
//!
//! A dispatch call is a statement of its own, ending in `;`, in one of these forms:
//!
//! - `f(args);` or `module::f(args);`, a call of a new function, or `Type::f(args);` of a
//!   function of a new type;
//! - `place.extend(call);`, where `call` is such a call and `place` a path or a field of one,
//!   such as `self.violations`: a new rule's reports are added to the existing ones;
//! - `place.register_lints(&[NEW, ..]);` with new lints only, and the base's
//!   `place.register_lints(&[..]);` with new lints appended;
//! - `place.register_late_lint_pass(Box::new(|_| Box::new(NewPass)));`, and the same with
//!   `register_early_lint_pass`, for a new lint pass.
//!
//! Every argument of a call is a literal or a shared reference to a path or a field of one, so
//! the new code gets no way to change the state that the existing rules read: no `&mut`, no
//! value that may be a `&mut` reborrowed, no method call, block or closure.

use super::NewNames;
use syn::spanned::Spanned;
use syn::{Expr, Stmt};

/// The methods that register a lint pass.
const PASS_REGISTRATIONS: [&str; 2] = ["register_late_lint_pass", "register_early_lint_pass"];

/// The method that registers lints.
const LINT_REGISTRATION: &str = "register_lints";

/// The method that adds a new rule's reports to the existing ones.
const EXTEND: &str = "extend";

/// Upper bound on the fields of one place expression.
const FIELDS_MAX: u32 = 64;

/// The texts of a file's two sides, which the spans point into.
#[derive(Clone, Copy)]
pub struct Sides<'a> {
    /// The base's text.
    pub base: &'a str,
    /// The head's text.
    pub head: &'a str,
}

/// The new items that `statement` dispatches, if it is a dispatch call of the module
/// `module` (see the module documentation).
#[must_use]
pub fn names(statement: &Stmt, module: &str, new: &NewNames) -> Option<Vec<String>> {
    let Stmt::Expr(expression, Some(_)) = statement else {
        return None;
    };
    if let Expr::Call(call) = expression {
        return new_call(call, module, new).map(|name| vec![name]);
    }
    let Expr::MethodCall(call) = expression else {
        return None;
    };
    if !call.attrs.is_empty() || call.turbofish.is_some() || !is_place(&call.receiver) {
        return None;
    }
    let [argument] = call.args.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let method = call.method.to_string();
    if method == EXTEND
        && let Expr::Call(inner) = argument
    {
        return new_call(inner, module, new).map(|name| vec![name]);
    }
    if method == LINT_REGISTRATION {
        let names = lint_names(argument)?;
        let all_new = !names.is_empty() && names.iter().all(|name| new.lints.contains(name));
        return all_new.then_some(names);
    }
    if PASS_REGISTRATIONS.contains(&method.as_str()) {
        return new_pass(argument, new).map(|name| vec![name]);
    }
    None
}

/// The new lints that `after` appends to the base's registration `before`: both are
/// `place.register_lints(&[..]);` on the same place, the base's lints come first in the same
/// order, and every lint after them is new.
#[must_use]
pub fn appended_lints(
    before: &Stmt,
    after: &Stmt,
    sides: Sides<'_>,
    new: &NewNames,
) -> Option<Vec<String>> {
    let registration = |statement: &Stmt, text: &str| -> Option<(String, Vec<String>)> {
        let Stmt::Expr(Expr::MethodCall(call), Some(_)) = statement else {
            return None;
        };
        let [argument] = call.args.iter().collect::<Vec<_>>()[..] else {
            return None;
        };
        if call.method != LINT_REGISTRATION || !call.attrs.is_empty() || call.turbofish.is_some() {
            return None;
        }
        let receiver = text.get(call.receiver.span().byte_range())?;
        let receiver: proc_macro2::TokenStream = receiver.parse().ok()?;
        Some((receiver.to_string(), lint_names(argument)?))
    };
    let (base_place, base_names) = registration(before, sides.base)?;
    let (head_place, head_names) = registration(after, sides.head)?;
    let appended = head_names.get(base_names.len()..)?;
    let prefix = head_names.get(..base_names.len())? == base_names.as_slice();
    let all_new = !appended.is_empty() && appended.iter().all(|name| new.lints.contains(name));
    (base_place == head_place && prefix && all_new).then(|| appended.to_vec())
}

/// The name a call `f(args)` dispatches, if `f` is a new function of `module` or a path to one,
/// or a function of a new type, and every argument is read only.
fn new_call(call: &syn::ExprCall, module: &str, new: &NewNames) -> Option<String> {
    let Expr::Path(function) = &*call.func else {
        return None;
    };
    if !call.attrs.is_empty() || !function.attrs.is_empty() || function.qself.is_some() {
        return None;
    }
    let segments: Vec<&syn::PathSegment> = function.path.segments.iter().collect();
    if segments.iter().any(|segment| !segment.arguments.is_none()) {
        return None;
    }
    let last = segments.last()?.ident.to_string();
    let named = match segments.len().checked_sub(2).and_then(|index| segments.get(index)) {
        None => new.functions.contains(&(module.to_owned(), last.clone())),
        Some(owner) => {
            let owner = owner.ident.to_string();
            new.functions.contains(&(owner.clone(), last.clone())) || new.types.contains(&owner)
        }
    };
    let read_only = call.args.iter().all(is_read_only);
    let name = segments.iter().map(|segment| segment.ident.to_string()).collect::<Vec<_>>();
    (named && read_only).then(|| name.join("::"))
}

/// True for an argument that gives the callee no way to change existing state: a literal, or a
/// shared reference to a place.
fn is_read_only(argument: &Expr) -> bool {
    if let Expr::Lit(literal) = argument {
        return literal.attrs.is_empty();
    }
    let Expr::Reference(reference) = argument else {
        return false;
    };
    reference.attrs.is_empty() && reference.mutability.is_none() && is_place(&reference.expr)
}

/// True for a path, or a field of a place: `self`, `node.attrs`, `self.violations`.
fn is_place(expression: &Expr) -> bool {
    let mut current = expression;
    for _ in 0..FIELDS_MAX {
        if let Expr::Path(path) = current {
            return path.attrs.is_empty() && path.qself.is_none();
        }
        let Expr::Field(field) = current else {
            return false;
        };
        if !field.attrs.is_empty() {
            return false;
        }
        current = &field.base;
    }
    false
}

/// The names of `&[NAME, ..]`, if every element is a plain name.
fn lint_names(argument: &Expr) -> Option<Vec<String>> {
    let Expr::Reference(reference) = argument else {
        return None;
    };
    let Expr::Array(array) = &*reference.expr else {
        return None;
    };
    if reference.mutability.is_some() || !reference.attrs.is_empty() || !array.attrs.is_empty() {
        return None;
    }
    array
        .elems
        .iter()
        .map(|element| {
            let Expr::Path(path) = element else {
                return None;
            };
            let plain = path.attrs.is_empty() && path.qself.is_none();
            path.path.get_ident().filter(|_| plain).map(ToString::to_string)
        })
        .collect()
}

/// What `Box::new(inner)` boxes.
fn boxed(expression: &Expr) -> Option<&Expr> {
    let Expr::Call(call) = expression else {
        return None;
    };
    let Expr::Path(function) = &*call.func else {
        return None;
    };
    let segments: Vec<String> =
        function.path.segments.iter().map(|segment| segment.ident.to_string()).collect();
    let [inner] = call.args.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let plain = call.attrs.is_empty() && function.attrs.is_empty() && function.qself.is_none();
    (plain && segments == ["Box", "new"]).then_some(inner)
}

/// The new pass that `Box::new(|_| Box::new(Pass))` registers.
fn new_pass(argument: &Expr, new: &NewNames) -> Option<String> {
    let Expr::Closure(closure) = boxed(argument)? else {
        return None;
    };
    let [syn::Pat::Wild(_)] = closure.inputs.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let plain = closure.attrs.is_empty()
        && closure.capture.is_none()
        && closure.asyncness.is_none()
        && closure.constness.is_none()
        && closure.lifetimes.is_none()
        && matches!(closure.output, syn::ReturnType::Default);
    let Expr::Path(pass) = boxed(&closure.body)? else {
        return None;
    };
    let name = pass.path.get_ident()?.to_string();
    (plain && pass.attrs.is_empty() && new.types.contains(&name)).then_some(name)
}
