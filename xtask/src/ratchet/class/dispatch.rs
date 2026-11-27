//! The dispatch calls of an additive diff: the statements that connect a new rule to the
//! existing code that runs the rules.
//!
//! A dispatch call is a statement of its own, ending in `;`, in one of these forms:
//!
//! - `f(args);` or `module::f(args);`, a call of a new function, or `Type::f(args);` of a
//!   function of a new type, each resolved by its path to the file that defines it
//!   ([`module_file`]);
//! - `place.extend(call);`, where `call` calls a new function whose return type is exactly
//!   `Vec<Violation>`, and `place` is a report list of the function the call is in, such as
//!   `self.violations` ([`super::sinks`]): a new rule's reports are added to the existing ones,
//!   and nothing else is. On any other place, such as an allow-list, it is an edit (review of
//!   #57, round 6);
//! - `place.register_lints(&[NEW, ..]);` with new lints only, and the base's
//!   `place.register_lints(&[..]);` with new lints appended;
//! - `place.register_late_lint_pass(Box::new(|_| Box::new(NewPass)));`, and the same with
//!   `register_early_lint_pass`, for a new lint pass.
//!
//! Every argument of a call is a literal or a shared reference to a path or a field of one: no
//! `&mut`, no value that may be a `&mut` reborrowed, no method call, block or closure. So a
//! tidy rule's new code gets nothing through its arguments that it could change the existing
//! rules' state with; the rule modules hold no `Cell`, `RefCell` or atomics behind a shared
//! reference.
//!
//! A lint pass is different: the compiler hands every pass the same `LateContext`, through
//! which shared references reach the compiler's diagnostics and session. New lint code that
//! names them is an edit ([`super::COMPILER_STATE`]), and `lint::exits_agree` is the backstop:
//! a dylint run whose exit code disagrees with the errors it printed, as one whose error count
//! a pass reset would, gives no verdict.

use super::NewNames;
use super::sinks::place_name;
use std::collections::BTreeSet;
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

/// The new items that `statement` dispatches, if it is a dispatch call in the file
/// `caller` (see the module documentation). `lists` are the report lists of the function it
/// is in ([`super::sinks::report_lists`]), the only places `extend` may add to.
#[must_use]
pub fn names(
    statement: &Stmt,
    caller: &str,
    new: &NewNames,
    lists: &BTreeSet<String>,
) -> Option<Vec<String>> {
    let Stmt::Expr(expression, Some(_)) = statement else {
        return None;
    };
    if let Expr::Call(call) = expression {
        return new_call(call, caller, new).map(|(name, _)| vec![name]);
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
        let into_list = place_name(&call.receiver).is_some_and(|place| lists.contains(&place));
        let (name, function) = new_call(inner, caller, new)?;
        let reports = function.is_some_and(|key| new.reports.contains(&key));
        return (into_list && reports).then(|| vec![name]);
    }
    if method == LINT_REGISTRATION {
        let names = lint_names(argument)?;
        let all_new = !names.is_empty() && names.iter().all(|name| new.lints.contains(name));
        return all_new.then_some(names);
    }
    if PASS_REGISTRATIONS.contains(&method.as_str()) {
        return new_pass(argument, caller, new).map(|name| vec![name]);
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

/// The file of the module that the path `segments` names, seen from the file `caller`:
/// `crate` or `self` first, `super`, then child modules, below the crate's `src/`. A module is
/// `name.rs` and the crate root `lib.rs`. `None` for a path that leaves the crate.
#[must_use]
pub fn module_file(caller: &str, segments: &[String]) -> Option<String> {
    let start = caller.find("src/")? + "src/".len();
    let (root, relative) = (caller.get(..start)?, caller.get(start..)?);
    let mut module: Vec<&str> = relative.strip_suffix(".rs")?.split('/').collect();
    if module.last().is_some_and(|last| ["lib", "main", "mod"].contains(last)) {
        let _root = module.pop();
    }
    for (index, segment) in segments.iter().enumerate() {
        match segment.as_str() {
            "crate" if index == 0 => module.clear(),
            "self" if index == 0 => {}
            "crate" | "self" => return None,
            "super" => {
                let _parent = module.pop()?;
            }
            name => module.push(name),
        }
    }
    let file = if module.is_empty() { "lib".to_owned() } else { module.join("/") };
    Some(format!("{root}{file}.rs"))
}

/// The name a call `f(args)` from the file `caller` dispatches, if `f` resolves by its path to
/// a function new in the file that defines it, or to a function of a type new in its file, and
/// every argument is read only. A name alone would let a new `fn line_of` in one file make a
/// call of an existing `line_of` in another look new (review of #57, round 5).
fn new_call(
    call: &syn::ExprCall,
    caller: &str,
    new: &NewNames,
) -> Option<(String, Option<(String, String)>)> {
    let Expr::Path(function) = &*call.func else {
        return None;
    };
    let plain = function.path.leading_colon.is_none() && function.qself.is_none();
    if !call.attrs.is_empty() || !function.attrs.is_empty() || !plain {
        return None;
    }
    let segments: Vec<&syn::PathSegment> = function.path.segments.iter().collect();
    if segments.iter().any(|segment| !segment.arguments.is_none()) {
        return None;
    }
    let names: Vec<String> = segments.iter().map(|segment| segment.ident.to_string()).collect();
    let (last, owners) = names.split_last()?;
    let defined = module_file(caller, owners)
        .map(|file| (file, last.clone()))
        .filter(|key| new.functions.contains(key));
    let of_type = owners.split_last().is_some_and(|(owner, modules)| {
        module_file(caller, modules).is_some_and(|file| new.types.contains(&(file, owner.clone())))
    });
    let read_only = call.args.iter().all(is_read_only);
    ((defined.is_some() || of_type) && read_only).then(|| (names.join("::"), defined))
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
fn new_pass(argument: &Expr, caller: &str, new: &NewNames) -> Option<String> {
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
    (plain && pass.attrs.is_empty() && new.types.contains(&(caller.to_owned(), name.clone())))
        .then_some(name)
}
