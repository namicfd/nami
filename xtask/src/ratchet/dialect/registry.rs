//! The dialect of the rule registry.
//!
//! The rule-set check reads `RULES`, `PATH_BANS` and `MACRO_BANS` row by row. So each table is
//! a slice of struct literals of its row type, `PLANNED` a slice of `planned(..)` calls, and
//! every argument a string literal, `&[..]` of string literals, or an enum variant with its
//! full type path. No attribute sits inside a table, and the tables carry doc comments only.
//! Everything else in the registry, the row types and enums and their code included, must be
//! token for token what the base has: a change there needs the code owner.
//!
//! A change to the rows needs no mutant only where the rule-set check proves it: every field
//! that changes must be one that check compares ([`COMPARED`]) or one that only documents
//! ([`DOCUMENTATION`]); any other is unproven ([`unproven_rows`]).

use super::{is_doc, is_test_module, outside, parse};
use proc_macro2::TokenStream;
use std::collections::{BTreeMap, BTreeSet};
use syn::spanned::Spanned;
use syn::{Expr, Lit};

/// The registry, relative to the root.
const REGISTRY: &str = super::super::source::REGISTRY;

/// Each table and the type of its rows; `PLANNED` holds calls of the function `planned`.
const ROW_TYPES: [(&str, &str); 4] = [
    ("RULES", "Rule"),
    ("PATH_BANS", "PathBan"),
    ("MACRO_BANS", "MacroBan"),
    ("PLANNED", "planned"),
];

/// The fields of each table's rows that the rule-set check compares
/// ([`super::super::source::loosened_rows`]).
pub const COMPARED: [(&str, &[&str]); 4] = [
    ("RULES", &["id", "kind"]),
    ("PATH_BANS", &["id", "segment", "allowed", "scope"]),
    ("MACRO_BANS", &["id", "names", "allowed", "scope"]),
    ("PLANNED", &[]),
];

/// The fields that only tidy's violation messages and `docs/RULES.md` print.
pub const DOCUMENTATION: [&str; 4] = ["message", "summary", "reference", "description"];

/// The parameters of `planned(..)`, in order.
const PLANNED_FIELDS: [&str; 4] = ["id", "mechanism", "description", "reference"];

/// The enums that a row may name, and the variants the ratchet knows.
pub const ENUMS: [(&str, &[&str]); 2] =
    [("RuleKind", &["Source", "Workspace"]), ("BanScope", &["Everywhere", "NamiCrates"])];

/// Checks the head's registry against the dialect, with `base_text` as the base's registry.
///
/// # Errors
///
/// No verdict: either file does not parse, or the head is outside the dialect.
pub fn check(base_text: &str, head_text: &str) -> Result<(), String> {
    let head = parse(REGISTRY, head_text)?;
    let base = syn::parse_file(base_text)
        .map_err(|error| format!("the base's {REGISTRY} does not parse: {error}"))?;
    if !head.attrs.iter().all(is_doc) {
        return Err(outside(REGISTRY, "a file attribute other than a doc comment"));
    }
    let mut seen: Vec<&str> = Vec::new();
    for item in &head.items {
        if let syn::Item::Const(constant) = item
            && let Some((name, row_type)) =
                ROW_TYPES.iter().find(|(name, _)| constant.ident == name)
        {
            table(constant, row_type)?;
            seen.push(name);
        }
    }
    seen.sort_unstable();
    let mut expected: Vec<&str> = ROW_TYPES.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    if seen != expected {
        return Err(outside(
            REGISTRY,
            "tables other than RULES, PATH_BANS, MACRO_BANS and PLANNED, once each",
        ));
    }
    let (before, after) = (others(base_text, &base)?, others(head_text, &head)?);
    if before != after {
        let changed =
            after.iter().zip(&before).position(|(head_item, base_item)| head_item != base_item);
        let at = changed.unwrap_or(before.len().min(after.len()));
        return Err(outside(
            REGISTRY,
            &format!(
                "a change outside the tables (item {} of {} differs from the base's)",
                at + 1,
                after.len()
            ),
        ));
    }
    Ok(())
}

/// Every item but the tables and the test module, as normalised token text.
fn others(text: &str, file: &syn::File) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for item in &file.items {
        let is_table = matches!(item, syn::Item::Const(constant)
            if ROW_TYPES.iter().any(|(name, _)| constant.ident == name));
        let is_tests = matches!(item, syn::Item::Mod(module) if is_test_module(module));
        if !is_table && !is_tests {
            let range = item.span().byte_range();
            let source = text.get(range).ok_or("an item's span lies outside its file")?;
            let tokens: TokenStream =
                source.parse().map_err(|error| format!("an item does not lex: {error}"))?;
            found.push(tokens.to_string());
        }
    }
    Ok(found)
}

/// One table: `pub const NAME: &[Type] = &[row, ..];` with doc comments only.
fn table(constant: &syn::ItemConst, row_type: &str) -> Result<(), String> {
    let name = constant.ident.to_string();
    if !constant.attrs.iter().all(is_doc) {
        return Err(outside(REGISTRY, &format!("an attribute on `{name}`")));
    }
    for row in elements(&constant.expr, &name)? {
        if row_type == "planned" {
            planned_row(row, &name)?;
        } else {
            struct_row(row, row_type, &name)?;
        }
    }
    Ok(())
}

/// The elements of `&[..]`, which carry no attribute.
fn elements<'a>(expr: &'a Expr, table: &str) -> Result<Vec<&'a Expr>, String> {
    let not_a_slice = || outside(REGISTRY, &format!("`{table}` not written as `&[..]`"));
    let Expr::Reference(reference) = expr else {
        return Err(not_a_slice());
    };
    let Expr::Array(array) = &*reference.expr else {
        return Err(not_a_slice());
    };
    if reference.mutability.is_some() || !reference.attrs.is_empty() || !array.attrs.is_empty() {
        return Err(not_a_slice());
    }
    Ok(array.elems.iter().collect())
}

/// A row `Type { field: value, .. }`, without `..` and attributes.
fn struct_row(row: &Expr, row_type: &str, table: &str) -> Result<(), String> {
    let Expr::Struct(literal) = row else {
        return Err(outside(REGISTRY, &format!("a row of `{table}` that is not a struct literal")));
    };
    let plain = literal.qself.is_none() && literal.path.is_ident(row_type);
    if !plain || literal.rest.is_some() || literal.dot2_token.is_some() || !literal.attrs.is_empty()
    {
        return Err(outside(
            REGISTRY,
            &format!("a row of `{table}` that is not a plain `{row_type} {{ .. }}`"),
        ));
    }
    for field in &literal.fields {
        if !field.attrs.is_empty() || !matches!(field.member, syn::Member::Named(_)) {
            return Err(outside(
                REGISTRY,
                &format!("a field of `{table}` with an attribute or no name"),
            ));
        }
        value(&field.expr, table)?;
    }
    Ok(())
}

/// A row `planned(value, ..)`.
fn planned_row(row: &Expr, table: &str) -> Result<(), String> {
    let Expr::Call(call) = row else {
        return Err(outside(REGISTRY, &format!("a row of `{table}` that is not `planned(..)`")));
    };
    let is_planned = matches!(&*call.func, Expr::Path(path)
        if path.qself.is_none() && path.attrs.is_empty() && path.path.is_ident("planned"));
    if !is_planned || !call.attrs.is_empty() {
        return Err(outside(REGISTRY, &format!("a row of `{table}` that is not `planned(..)`")));
    }
    call.args.iter().try_for_each(|argument| value(argument, table))
}

/// A string literal, `&[..]` of string literals, or `Enum::Variant` of a known enum.
fn value(expr: &Expr, table: &str) -> Result<(), String> {
    let bad = |what: &str| Err(outside(REGISTRY, &format!("{what} in `{table}`")));
    if is_string(expr) {
        return Ok(());
    }
    if let Expr::Reference(_) = expr {
        if elements(expr, table)?.into_iter().all(is_string) {
            return Ok(());
        }
        return bad("a list element that is not a string literal");
    }
    if let Expr::Path(path) = expr
        && path.attrs.is_empty()
        && path.qself.is_none()
    {
        if variant_of(&path.path).is_some() {
            return Ok(());
        }
        return bad("a name that is not `Enum::Variant` of a known enum");
    }
    bad("a value that is not a plain string literal, a list of them or an enum variant")
}

/// True for a plain string literal, without a suffix or an attribute.
fn is_string(expr: &Expr) -> bool {
    let Expr::Lit(literal) = expr else {
        return false;
    };
    let Lit::Str(text) = &literal.lit else {
        return false;
    };
    literal.attrs.is_empty() && text.suffix().is_empty()
}

/// `(enum, variant)` for a path of exactly two plain segments that names a known variant.
#[must_use]
pub fn variant_of(path: &syn::Path) -> Option<(&'static str, String)> {
    if path.leading_colon.is_some() || path.segments.len() != 2 {
        return None;
    }
    let plain = path.segments.iter().all(|segment| segment.arguments.is_none());
    let first = path.segments.first()?;
    let last = path.segments.last()?;
    let (name, variants) = ENUMS.iter().find(|(name, _)| first.ident == name)?;
    let known = variants.iter().any(|variant| last.ident == variant);
    (plain && known).then(|| (*name, last.ident.to_string()))
}

/// One row of a table: its line, and each field's value as normalised token text.
struct Row {
    line: usize,
    fields: BTreeMap<String, String>,
}

/// Every changed field of a registry row that the rule-set check neither compares
/// ([`COMPARED`]) nor is documentation only ([`DOCUMENTATION`]), as an unproven item: no
/// data check proves it, and a table makes no mutant. Rows are matched by table and id; a new
/// or removed row changes every field it has.
///
/// # Errors
///
/// No verdict: either registry does not parse, or a row has no string id, or shares it.
pub fn unproven_rows(base_text: &str, head_text: &str) -> Result<Vec<String>, String> {
    let (base, head) = (rows_of(base_text)?, rows_of(head_text)?);
    let keys: BTreeSet<&(String, String)> = base.keys().chain(head.keys()).collect();
    assert!(keys.len() >= base.len().max(head.len()), "every row of either side is a key");
    let mut unproven = Vec::new();
    for key in keys {
        let (before, after) = (base.get(key), head.get(key));
        let Some(shown) = after.or(before) else {
            continue;
        };
        let side = if after.is_some() { "" } else { " of the base" };
        let (table, id) = key;
        let compared =
            COMPARED.iter().find(|(name, _)| name == table).map_or(&[][..], |entry| entry.1);
        let names: BTreeSet<&String> =
            before.into_iter().chain(after).flat_map(|side_row| side_row.fields.keys()).collect();
        for field in names {
            let known =
                compared.contains(&field.as_str()) || DOCUMENTATION.contains(&field.as_str());
            let value =
                |side_row: Option<&Row>| side_row.and_then(|r| r.fields.get(field)).cloned();
            if !known && value(before) != value(after) {
                unproven.push(format!(
                    "the change at {REGISTRY}:{}{side} to `{field}` of `{id}` in `{table}` is \
                     neither compared by the rule-set check nor documentation, so no check \
                     proves it",
                    shown.line
                ));
            }
        }
    }
    Ok(unproven)
}

/// The rows of every table of `text`, by table and id.
fn rows_of(text: &str) -> Result<BTreeMap<(String, String), Row>, String> {
    let file =
        syn::parse_file(text).map_err(|error| format!("{REGISTRY} does not parse: {error}"))?;
    let mut rows = BTreeMap::new();
    for item in &file.items {
        let syn::Item::Const(constant) = item else {
            continue;
        };
        let Some((table, _)) = ROW_TYPES.iter().find(|(name, _)| constant.ident == name) else {
            continue;
        };
        for row in elements(&constant.expr, table)? {
            let fields = row_fields(text, row, table)?;
            let id = fields.get("id").and_then(|id| syn::parse_str::<syn::LitStr>(id).ok());
            let id = id.ok_or_else(|| format!("a row of `{table}` in {REGISTRY} has no id"))?;
            let line = row.span().start().line;
            let key = ((*table).to_owned(), id.value());
            if rows.insert(key, Row { line, fields }).is_some() {
                return Err(format!("two rows of `{table}` in {REGISTRY} share an id"));
            }
        }
    }
    Ok(rows)
}

/// The fields of one row by name, each as normalised token text; the arguments of
/// `planned(..)` by [`PLANNED_FIELDS`].
fn row_fields(text: &str, row: &Expr, table: &str) -> Result<BTreeMap<String, String>, String> {
    let value = |expr: &Expr| -> Result<String, String> {
        let source = text.get(expr.span().byte_range()).ok_or("a field lies outside its file")?;
        let tokens: TokenStream =
            source.parse().map_err(|error| format!("a field does not lex: {error}"))?;
        Ok(tokens.to_string())
    };
    let mut fields = BTreeMap::new();
    if let Expr::Struct(literal) = row {
        for field in &literal.fields {
            let syn::Member::Named(name) = &field.member else {
                return Err(outside(REGISTRY, &format!("a field of `{table}` with no name")));
            };
            let _replaced = fields.insert(name.to_string(), value(&field.expr)?);
        }
    } else if let Expr::Call(call) = row {
        for (index, argument) in call.args.iter().enumerate() {
            let name = PLANNED_FIELDS
                .get(index)
                .map_or_else(|| format!("argument {}", index + 1), |name| (*name).to_owned());
            let _replaced = fields.insert(name, value(argument)?);
        }
    } else {
        return Err(outside(REGISTRY, &format!("a row of `{table}` of another form")));
    }
    Ok(fields)
}
