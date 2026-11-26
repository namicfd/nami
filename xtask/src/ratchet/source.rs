//! What the base reads from the head's source as data, with `syn`, without running any head
//! code (ADR-0022): the rule tables, the `check_*` calls of the rule modules, and the lint
//! declarations and registrations.
//!
//! Mutation testing makes no mutant for a changed constant or a deleted line, and the corpus
//! pins a ban at one or two paths. These checks close that gap for data: every base row must be
//! in the head unchanged or narrowed in what it permits, and every base `check_*` call site must
//! still be there. A table in a form this module does not read is no verdict, never a pass.

use super::{Check, Finding};
use crate::tidy::lints::{self, DylintLint};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use syn::visit::{self, Visit};
use syn::{Expr, ExprArray, Lit};

/// The rule registry, relative to the root.
pub const REGISTRY: &str = "xtask/src/tidy/registry.rs";

/// The lint library's root module, where lints are registered.
const LINT_LIBRARY: &str = "lints/src/lib.rs";

/// The rule-path modules whose `check_*` call sites must stay (the registry has none).
pub const RULE_MODULES: [&str; 7] = [
    "xtask/src/tidy/docs.rs",
    "xtask/src/tidy/lints.rs",
    "xtask/src/tidy/manifest.rs",
    "xtask/src/tidy/rust_source.rs",
    "xtask/src/tidy/style_guide.rs",
    "xtask/src/tidy/text.rs",
    "xtask/src/tidy/tokens.rs",
];

/// The scope of a ban that applies everywhere; any other scope is narrower.
const SCOPE_EVERYWHERE: &str = "Everywhere";

/// The scopes the judge knows. A ban in any other scope gives no verdict.
const SCOPES: [&str; 2] = [SCOPE_EVERYWHERE, "NamiCrates"];

/// One row of `PATH_BANS` or `MACRO_BANS`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ban {
    /// The rule id.
    pub id: String,
    /// What is banned: the path segment, or the macro names.
    pub banned: Vec<String>,
    /// Where it is allowed.
    pub allowed: Vec<String>,
    /// The variant of `BanScope`.
    pub scope: String,
    /// The table it is a row of: `PATH_BANS` or `MACRO_BANS`.
    pub table: String,
}

/// The tables of the registry, as written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    /// `(id, kind)` of every row of `RULES`.
    pub rules: Vec<(String, String)>,
    /// The rows of `PATH_BANS` and `MACRO_BANS`.
    pub bans: Vec<Ban>,
}

impl Registry {
    /// Every rule id, from every table.
    #[must_use]
    pub fn ids(&self) -> BTreeSet<String> {
        let rules = self.rules.iter().map(|(id, _)| id.clone());
        rules.chain(self.bans.iter().map(|ban| ban.id.clone())).collect()
    }
}

/// Reads the registry's tables from its source.
///
/// # Errors
///
/// Returns what is unreadable: a file that does not parse, a missing table, or a row that is
/// not written as a literal.
pub fn parse_registry(text: &str) -> Result<Registry, String> {
    let file =
        syn::parse_file(text).map_err(|error| format!("{REGISTRY} does not parse: {error}"))?;
    let mut registry = Registry::default();
    let mut tables = 0_u32;
    for item in &file.items {
        let syn::Item::Const(constant) = item else {
            continue;
        };
        let name = constant.ident.to_string();
        let parsed = match name.as_str() {
            "RULES" => rows(&constant.expr)?
                .into_iter()
                .map(rule_row)
                .collect::<Result<Vec<_>, _>>()
                .map(|rules| registry.rules = rules),
            "PATH_BANS" | "MACRO_BANS" => {
                let bans = rows(&constant.expr)?
                    .into_iter()
                    .map(|row| ban_row(row, &name))
                    .collect::<Result<Vec<_>, _>>()?;
                registry.bans.extend(bans);
                Ok(())
            }
            _ => continue,
        };
        parsed?;
        tables += 1;
    }
    if tables != 3 {
        return Err(format!("{REGISTRY} must hold RULES, PATH_BANS and MACRO_BANS once each"));
    }
    Ok(registry)
}

/// The elements of `&[..]`.
fn rows(expr: &Expr) -> Result<Vec<&Expr>, String> {
    let Expr::Reference(reference) = expr else {
        return Err(format!("a table in {REGISTRY} is not written as `&[..]`"));
    };
    let Expr::Array(ExprArray { elems, .. }) = &*reference.expr else {
        return Err(format!("a table in {REGISTRY} is not written as `&[..]`"));
    };
    Ok(elems.iter().collect())
}

/// The fields of a struct literal, by name.
fn fields(row: &Expr) -> Result<BTreeMap<String, &Expr>, String> {
    let Expr::Struct(literal) = row else {
        return Err(format!("a row in {REGISTRY} is not a struct literal"));
    };
    if literal.rest.is_some() {
        return Err(format!("a row in {REGISTRY} uses `..`"));
    }
    let mut found = BTreeMap::new();
    for field in &literal.fields {
        let syn::Member::Named(name) = &field.member else {
            return Err(format!("a row in {REGISTRY} has an unnamed field"));
        };
        let _replaced = found.insert(name.to_string(), &field.expr);
    }
    Ok(found)
}

fn field<'a>(fields: &BTreeMap<String, &'a Expr>, name: &str) -> Result<&'a Expr, String> {
    fields.get(name).copied().ok_or_else(|| format!("a row in {REGISTRY} has no `{name}`"))
}

fn string(expr: &Expr) -> Result<String, String> {
    if let Expr::Lit(literal) = expr
        && let Lit::Str(text) = &literal.lit
    {
        return Ok(text.value());
    }
    Err(format!("a field in {REGISTRY} is not a string literal"))
}

fn strings(expr: &Expr) -> Result<Vec<String>, String> {
    rows(expr)?.into_iter().map(string).collect()
}

/// The last segment of a path such as `RuleKind::Source`.
fn variant(expr: &Expr) -> Result<String, String> {
    let Expr::Path(path) = expr else {
        return Err(format!("a field in {REGISTRY} is not an enum variant"));
    };
    let last = path.path.segments.last().ok_or("an empty path")?;
    Ok(last.ident.to_string())
}

fn rule_row(row: &Expr) -> Result<(String, String), String> {
    let fields = fields(row)?;
    Ok((string(field(&fields, "id")?)?, variant(field(&fields, "kind")?)?))
}

fn ban_row(row: &Expr, table: &str) -> Result<Ban, String> {
    let fields = fields(row)?;
    let id = string(field(&fields, "id")?)?;
    let allowed = strings(field(&fields, "allowed")?)?;
    let scope = variant(field(&fields, "scope")?)?;
    if !SCOPES.contains(&scope.as_str()) {
        return Err(format!("ban `{id}` has the scope `{scope}`, which the ratchet does not know"));
    }
    let banned = if table == "PATH_BANS" {
        vec![string(field(&fields, "segment")?)?]
    } else {
        strings(field(&fields, "names")?)?
    };
    Ok(Ban { id, banned, allowed, scope, table: table.to_owned() })
}

/// One finding for each base row that the head loosens: a rule that is gone or changed kind, a
/// ban that is gone, bans less, applies in a narrower scope, or is allowed in more places.
#[must_use]
pub fn loosened_rows(base: &Registry, head: &Registry) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut report =
        |message: String| findings.push(Finding::new(REGISTRY, Check::RuleSet, message));
    for (id, kind) in &base.rules {
        match head.rules.iter().find(|(head_id, _)| head_id == id) {
            None => report(format!(
                "tidy rule `{id}` is registered in the base and not in the head; a rule may be \
                 tightened or replaced under a new id, never removed (ADR-0022)"
            )),
            Some((_, head_kind)) if head_kind != kind => report(format!(
                "tidy rule `{id}` changes kind from {kind} to {head_kind}; that needs the code \
                 owner (ADR-0022)"
            )),
            Some(_) => {}
        }
    }
    for ban in &base.bans {
        let Some(head_ban) = head.bans.iter().find(|candidate| candidate.id == ban.id) else {
            report(format!(
                "ban `{}` is in the base and not in the head; a ban is never removed without the \
                 code owner (ADR-0022)",
                ban.id
            ));
            continue;
        };
        if let Some(problem) = ban_problem(ban, head_ban) {
            report(format!(
                "ban `{}` {problem}; a ban may only ban more, in a wider scope, with fewer \
                 exceptions (ADR-0022)",
                ban.id
            ));
        }
    }
    findings
}

/// What makes `head` looser than `base`, if anything.
fn ban_problem(base: &Ban, head: &Ban) -> Option<String> {
    if head.table != base.table {
        return Some(format!(
            "moved from {} to {}, which bans something else",
            base.table, head.table
        ));
    }
    let dropped: Vec<&String> =
        base.banned.iter().filter(|name| !head.banned.contains(name)).collect();
    if !dropped.is_empty() {
        return Some(format!("no longer bans {dropped:?}"));
    }
    if head.scope != base.scope && head.scope != SCOPE_EVERYWHERE {
        return Some(format!("narrows its scope from {} to {}", base.scope, head.scope));
    }
    let covered =
        |entry: &String| base.allowed.iter().any(|allowed| Path::new(entry).starts_with(allowed));
    let widened: Vec<&String> = head.allowed.iter().filter(|entry| !covered(entry)).collect();
    if !widened.is_empty() {
        return Some(format!("is allowed in {widened:?} as well"));
    }
    None
}

/// The `check_*` calls of one module: `(enclosing function, callee)` pairs, method calls
/// included.
///
/// # Errors
///
/// Returns what is wrong if the module does not parse.
pub fn check_calls(relative: &str, text: &str) -> Result<BTreeSet<(String, String)>, String> {
    let file =
        syn::parse_file(text).map_err(|error| format!("{relative} does not parse: {error}"))?;
    let mut finder = CallFinder { function: String::new(), found: BTreeSet::new() };
    finder.visit_file(&file);
    Ok(finder.found)
}

struct CallFinder {
    function: String,
    found: BTreeSet<(String, String)>,
}

impl CallFinder {
    fn record(&mut self, callee: &syn::Ident) {
        let name = callee.to_string();
        if name.starts_with("check_") {
            let _new = self.found.insert((self.function.clone(), name));
        }
    }

    fn within(&mut self, name: &syn::Ident, visit_body: impl FnOnce(&mut Self)) {
        let outer = std::mem::replace(&mut self.function, name.to_string());
        visit_body(self);
        self.function = outer;
    }
}

/// True for test code: `#[test]` or `#[cfg(test)]`. Calls there are compiled out of the rule.
fn is_test_item(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        let is_cfg_test = attribute.path().is_ident("cfg")
            && attribute.parse_args::<syn::Path>().is_ok_and(|path| path.is_ident("test"));
        attribute.path().is_ident("test") || is_cfg_test
    })
}

impl<'ast> Visit<'ast> for CallFinder {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_test_item(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_test_item(&node.attrs) {
            self.within(&node.sig.ident, |finder| visit::visit_item_fn(finder, node));
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_test_item(&node.attrs) {
            self.within(&node.sig.ident, |finder| visit::visit_impl_item_fn(finder, node));
        }
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Expr::Path(path) = &*node.func
            && let Some(last) = path.path.segments.last()
        {
            self.record(&last.ident);
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.record(&node.method);
        visit::visit_expr_method_call(self, node);
    }
}

/// The lint names in every `register_lints(&[..])` call of the lint library's root module.
///
/// # Errors
///
/// Returns what is wrong if the module does not parse.
pub fn registered_lints(text: &str) -> Result<BTreeSet<String>, String> {
    let file =
        syn::parse_file(text).map_err(|error| format!("{LINT_LIBRARY} does not parse: {error}"))?;
    let mut finder = Registrations { found: BTreeSet::new() };
    finder.visit_file(&file);
    Ok(finder.found)
}

struct Registrations {
    found: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Registrations {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == "register_lints"
            && let Some(Expr::Reference(reference)) = node.args.first()
            && let Expr::Array(array) = &*reference.expr
        {
            self.found.extend(array.elems.iter().filter_map(lint_name));
        }
        visit::visit_expr_method_call(self, node);
    }
}

/// The lint named by an element of `register_lints(&[..])`, in lower case.
fn lint_name(element: &Expr) -> Option<String> {
    let Expr::Path(path) = element else {
        return None;
    };
    path.path.segments.last().map(|last| last.ident.to_string().to_lowercase())
}

/// Compares what the base reads from both trees: the registry, the `check_*` calls of the rule
/// modules, and the lints declared and registered.
///
/// # Errors
///
/// Returns what is unreadable: no verdict.
pub fn check(base: &Path, head: &Path) -> Result<Vec<Finding>, String> {
    super::form::check_head(head)?;
    let read = |root: &Path, relative: &str| -> Result<String, String> {
        read_optional(root, relative)?.ok_or_else(|| format!("{relative} is missing"))
    };
    let mut findings = loosened_rows(
        &parse_registry(&read(base, REGISTRY)?)?,
        &parse_registry(&read(head, REGISTRY)?)?,
    );
    for module in RULE_MODULES {
        let Some(base_text) = read_optional(base, module)? else {
            continue;
        };
        let before = check_calls(module, &base_text)?;
        let after = match read_optional(head, module)? {
            Some(text) => check_calls(module, &text)?,
            None => BTreeSet::new(),
        };
        for (function, callee) in before.difference(&after) {
            let message = format!(
                "`{function}` no longer calls `{callee}`; a rule's check is never removed without \
                 the code owner (ADR-0022)"
            );
            findings.push(Finding::new(module, Check::RuleSet, message));
        }
    }
    findings.extend(lint_findings(base, head)?);
    Ok(findings)
}

/// Lints: every lint the base enforces is declared exactly once in the head, still enforced,
/// and still registered.
fn lint_findings(base: &Path, head: &Path) -> Result<Vec<Finding>, String> {
    let read = |root: &Path, side: &str| {
        lints::declared(root)
            .map_err(|error| format!("cannot read the {side}'s lint declarations: {error}"))
    };
    let mut findings = weakened_lints(&read(base, "base")?, &read(head, "head")?);
    if let Some(base_text) = read_optional(base, LINT_LIBRARY)? {
        let before = registered_lints(&base_text)?;
        let after = match read_optional(head, LINT_LIBRARY)? {
            Some(text) => registered_lints(&text)?,
            None => BTreeSet::new(),
        };
        for lint in before.difference(&after) {
            let message = format!(
                "dylint lint `{lint}` is registered in the base and not in the head; a lint that \
                 is not registered never runs (ADR-0022)"
            );
            findings.push(Finding::new(LINT_LIBRARY, Check::RuleSet, message));
        }
    }
    Ok(findings)
}

/// One finding for each lint that the base enforces and the head does not declare exactly
/// once, or declares at a level that does not fail `just dylint`.
#[must_use]
pub fn weakened_lints(base: &[DylintLint], head: &[DylintLint]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for lint in base.iter().filter(|lint| lint.is_enforced()) {
        let declarations: Vec<&DylintLint> =
            head.iter().filter(|candidate| candidate.name == lint.name).collect();
        let problem = match declarations.as_slice() {
            [] => "is not declared in the head".to_owned(),
            [found] if !found.is_enforced() => {
                "is no longer `Deny` or `Forbid` in the head".to_owned()
            }
            [_] => continue,
            many => format!("is declared {} times in the head; declare it once", many.len()),
        };
        let message = format!(
            "dylint lint `{}` is enforced in the base and {problem}; a lint may be tightened, \
             never removed or lowered to a warning, which `just dylint` lets pass (ADR-0022)",
            lint.name
        );
        findings.push(Finding::new(LINT_LIBRARY, Check::RuleSet, message));
    }
    findings
}

/// The file at `relative` below `root`, or `None` if there is none. It must be a regular file
/// reached through no symbolic link, so that the judge reads what the compiler compiles (review
/// of #57, thread 3).
///
/// # Errors
///
/// No verdict: a link anywhere on the path, something that is not a regular file, or a read
/// error.
pub fn read_optional(root: &Path, relative: &str) -> Result<Option<String>, String> {
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read {relative}: {error}")),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "{relative} goes through a symbolic link, which the judge never follows"
                ));
            }
            Ok(_) => {}
        }
    }
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(format!("{relative} is not a regular file"));
    }
    fs::read_to_string(&path).map(Some).map_err(|error| format!("cannot read {relative}: {error}"))
}

/// The base's rules whose code sits in a rule path (their id is a string literal there) and
/// that have no firing fixture in the base. Each needs a case before the ratchet can vouch for
/// a change to it (ADR-0022).
///
/// # Errors
///
/// Returns what is unreadable.
pub fn rules_without_cases(base: &Path, tree: &Path) -> Result<Vec<String>, String> {
    let registry_text = read_optional(base, REGISTRY)?.ok_or("the base has no rule registry")?;
    let registry = parse_registry(&registry_text)?;
    let mut uncovered = Vec::new();
    for module in RULE_MODULES {
        let Some(text) = read_optional(tree, module)? else {
            continue;
        };
        for id in registry.ids() {
            let fixture =
                base.join(super::corpus::FIXTURE_DIRECTORY).join(format!("{id}.fires.fixture"));
            if text.contains(&format!("\"{id}\"")) && !fixture.is_file() {
                uncovered.push(format!("{id} ({module})"));
            }
        }
    }
    Ok(uncovered)
}

#[cfg(test)]
mod tests {
    use super::{
        Ban, check_calls, loosened_rows, parse_registry, read_optional, registered_lints,
        rules_without_cases, weakened_lints,
    };
    use crate::tidy::lints::DylintLint;
    use std::collections::BTreeSet;

    // Goal: the judge reads a regular file, `None` for a missing one, and no verdict for a path
    // through a symbolic link, to a file or a directory, or for something that is not a file.
    #[test]
    fn the_judge_never_follows_a_link() {
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-read-optional");
        scratch.write("lints/src/lib.rs", "a");
        let root = scratch.root();
        assert_eq!(read_optional(root, "lints/src/lib.rs"), Ok(Some("a".to_owned())));
        assert_eq!(read_optional(root, "lints/src/gone.rs"), Ok(None));
        assert_eq!(read_optional(root, "gone/src/lib.rs"), Ok(None));
        std::os::unix::fs::symlink(root.join("lints/src/lib.rs"), root.join("lints/src/a.rs"))
            .unwrap();
        std::os::unix::fs::symlink(root.join("lints"), root.join("linked")).unwrap();
        for linked in ["lints/src/a.rs", "linked/src/lib.rs"] {
            let error = read_optional(root, linked).unwrap_err();
            assert!(error.contains("symbolic link"), "{error}");
        }
        assert!(read_optional(root, "lints/src").unwrap_err().contains("not a regular file"));
        let below_a_file = read_optional(root, "lints/src/lib.rs/a.rs").unwrap_err();
        assert!(below_a_file.contains("cannot read"), "only a missing file is none");
    }

    fn real_registry() -> String {
        std::fs::read_to_string(crate::workspace_root().join(super::REGISTRY)).unwrap()
    }

    fn messages(base: &str, head: &str) -> Vec<String> {
        let findings =
            loosened_rows(&parse_registry(base).unwrap(), &parse_registry(head).unwrap());
        findings.into_iter().map(|finding| finding.message).collect()
    }

    // Goal: the real registry is read in full: every registered id, and both bans with their
    // scope and allowed paths.
    #[test]
    fn the_real_registry_is_read() {
        let registry = parse_registry(&real_registry()).unwrap();
        let expected: BTreeSet<String> =
            crate::tidy::registry::rule_ids().map(str::to_owned).collect();
        assert_eq!(registry.ids(), expected);
        let libm = registry.bans.iter().find(|ban| ban.id == "direct-libm").unwrap();
        let ban = Ban {
            id: "direct-libm".to_owned(),
            banned: vec!["libm".to_owned()],
            allowed: vec!["crates/kernels/src/math.rs".to_owned()],
            scope: "Everywhere".to_owned(),
            table: "PATH_BANS".to_owned(),
        };
        assert_eq!(*libm, ban);
        let debug = registry.bans.iter().find(|row| row.id == "debug-assert-placement").unwrap();
        assert_eq!((debug.table.as_str(), debug.scope.as_str()), ("MACRO_BANS", "Everywhere"));
        assert!(
            registry
                .rules
                .iter()
                .any(|(id, kind)| id == "assertion-density" && kind == "Workspace")
        );
    }

    // Goal: the review's exploits are findings: a narrowed scope, a wider allowed list, a ban
    // that bans less, a removed rule or ban, a changed kind. Unchanged rows, a narrower allowed
    // entry and a new row are not.
    #[test]
    fn loosened_rows_are_found() {
        let base = real_registry();
        assert_eq!(messages(&base, &base), Vec::<String>::new());
        let path_ban = "PathBan {\n    id: \"direct-libm\",\n    segment: \"libm\",";
        let as_macro = "MacroBan {\n    id: \"direct-libm\",\n    names: &[\"libm\"],";
        let moved = base.replacen(
            "pub const PATH_BANS: &[PathBan] = &[",
            "pub const PATH_BANS: &[PathBan] = &[];\nconst UNUSED: &[PathBan] = &[",
            1,
        );
        let row = format!(
            "{as_macro}\n    allowed: &[\"crates/kernels/src/math.rs\"],\n    \
             scope: BanScope::Everywhere,\n    message: \"m\",\n}}, "
        );
        let table = "pub const MACRO_BANS: &[MacroBan] = &[";
        let moved = moved.replace(table, &format!("{table}{row}"));
        assert!(base.contains(path_ban), "the fixture text of the probe");
        let found = messages(&base, &moved);
        assert!(
            found.iter().any(|message| message.contains("moved from PATH_BANS to MACRO_BANS")),
            "{found:?}"
        );
        let unknown = base.replacen("scope: BanScope::Everywhere", "scope: BanScope::Somewhere", 1);
        assert!(parse_registry(&unknown).unwrap_err().contains("does not know"));
        let wider = base.replacen("scope: BanScope::Everywhere", "scope: BanScope::NamiCrates", 1);
        assert_eq!(
            messages(&wider, &base),
            Vec::<String>::new(),
            "NamiCrates to Everywhere widens"
        );
        let narrowed = base.replace("scope: BanScope::Everywhere", "scope: BanScope::NamiCrates");
        assert!(
            messages(&base, &narrowed)[0]
                .contains("narrows its scope from Everywhere to NamiCrates")
        );
        let allowed = base.replace(
            "allowed: &[\"crates/kernels\"]",
            "allowed: &[\"crates/kernels\", \"crates/solver\"]",
        );
        assert!(messages(&base, &allowed)[0].contains("is allowed in [\"crates/solver\"] as well"));
        let fewer = base.replace("names: &[\"debug_assert\", ", "names: &[");
        assert!(messages(&base, &fewer)[0].contains("no longer bans [\"debug_assert\"]"));
        let removed = base.replace("id: \"line-length\"", "id: \"line-length-renamed\"");
        assert!(
            messages(&base, &removed)[0]
                .contains("tidy rule `line-length` is registered in the base and not")
        );
        let kind = base.replacen("kind: RuleKind::Source", "kind: RuleKind::Workspace", 1);
        assert!(messages(&base, &kind)[0].contains("changes kind from Source to Workspace"));
        let tighter = base.replace(
            "allowed: &[\"crates/kernels\"]",
            "allowed: &[\"crates/kernels/src/reduce.rs\"]",
        );
        assert_eq!(messages(&base, &tighter), Vec::<String>::new(), "an exception narrowed");
        let gone = base.replace("allowed: &[\"crates/kernels/src/math.rs\"]", "allowed: &[]");
        assert_eq!(messages(&gone, &base).len(), 1, "an exception added back is a loosening");
    }

    // Goal: a registry in another form is unreadable, never an empty table.
    #[test]
    fn unreadable_registries_are_errors() {
        assert!(parse_registry("fn a( {").is_err(), "does not parse");
        assert!(parse_registry("const RULES: &[Rule] = &[];").unwrap_err().contains("once each"));
        let call = "const RULES: &[Rule] = &[source(\"a\", \"b\", \"c\")];\nconst PATH_BANS: \
                    &[PathBan] = &[];\nconst MACRO_BANS: &[MacroBan] = &[];";
        assert!(parse_registry(call).unwrap_err().contains("not a struct literal"));
        let computed = "const RULES: &[Rule] = &[];\nconst PATH_BANS: &[PathBan] = &[PathBan { \
                        id: ID, segment: \"a\", allowed: &[], scope: BanScope::Everywhere, \
                                           message: \"m\" }];\nconst MACRO_BANS: &[MacroBan] = \
                                                      &[];";
        assert!(parse_registry(computed).unwrap_err().contains("not a string literal"));
        let vector = "const RULES: &[Rule] = &[];\nconst PATH_BANS: &[PathBan] = BANS;\nconst \
                      MACRO_BANS: &[MacroBan] = &[];";
        assert!(parse_registry(vector).unwrap_err().contains("not written as `&[..]`"));
    }

    // Goal: `check_*` calls are found with their enclosing function, as calls and as method
    // calls, so that a deleted call is seen.
    #[test]
    fn check_calls_are_found() {
        let text = "fn a() { check_x(); helper(); }\nimpl V { fn b(&mut self) { self.check_y(1); \
                    super::check_z(); } }\nmod inner { fn c() { check_w(); } }";
        let found = check_calls("m.rs", text).unwrap();
        let pairs: Vec<(&str, &str)> =
            found.iter().map(|(f, c)| (f.as_str(), c.as_str())).collect();
        let expected = [("a", "check_x"), ("b", "check_y"), ("b", "check_z"), ("c", "check_w")];
        assert_eq!(pairs, expected, "a module that is not test code counts");
        let real =
            std::fs::read_to_string(crate::workspace_root().join("xtask/src/tidy/rust_source.rs"))
                .unwrap();
        let calls = check_calls("rust_source.rs", &real).unwrap();
        assert!(calls.contains(&("check_body".to_owned(), "check_float_equality".to_owned())));
        let in_tests = format!(
            "{real}\n#[cfg(test)]\nmod decoy {{ fn check_body() {{ check_float_equality(); }} }}"
        );
        let decoy =
            real.replace("self.check_float_equality(function);", "") + &in_tests[real.len()..];
        let pair = ("check_body".to_owned(), "check_float_equality".to_owned());
        assert!(
            !check_calls("rust_source.rs", &decoy).unwrap().contains(&pair),
            "test code does not count"
        );
        let deleted = real.replace("self.check_float_equality(function);", "");
        assert!(
            !check_calls("rust_source.rs", &deleted)
                .unwrap()
                .contains(&("check_body".to_owned(), "check_float_equality".to_owned()))
        );
    }

    // Goal: registered lints are read from `register_lints(&[..])`, and the real library
    // registers both of its lints.
    #[test]
    fn registered_lints_are_read() {
        let real =
            std::fs::read_to_string(crate::workspace_root().join("lints/src/lib.rs")).unwrap();
        let found = registered_lints(&real).unwrap();
        assert_eq!(
            found,
            BTreeSet::from(["nami_float_reduction".to_owned(), "nami_recursion".to_owned()])
        );
        assert!(registered_lints("fn f( {").is_err());
    }

    fn lint(name: &str, level: &str) -> DylintLint {
        DylintLint { name: name.to_owned(), level: level.to_owned(), description: String::new() }
    }

    // Goal: an enforced base lint must stay declared once and enforced in the head; a decoy
    // second declaration is a finding; a warn-only base lint or a new head lint is free.
    #[test]
    fn weakened_lints_are_found() {
        let base = [lint("a", "Deny"), lint("b", "Deny"), lint("c", "Forbid"), lint("d", "Warn")];
        let head = [lint("a", "Deny"), lint("b", "Warn"), lint("e", "Deny")];
        let messages: Vec<String> =
            weakened_lints(&base, &head).into_iter().map(|finding| finding.message).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("`b` is enforced in the base and is no longer `Deny`"));
        assert!(messages[1].contains("`c` is enforced in the base and is not declared"));
        let decoy = [lint("a", "Deny"), lint("a", "Warn")];
        assert!(
            weakened_lints(&[lint("a", "Deny")], &decoy)[0].message.contains("declared 2 times")
        );
        let kept = [lint("a", "Forbid"), lint("d", "Warn")];
        assert!(weakened_lints(&kept, &head).is_empty(), "a kept, d never enforced, e is new");
    }

    // Goal: in this tree, every rule whose id appears in a rule-path module has a firing
    // fixture: the invariant that lets the ratchet vouch for a change to it (ADR-0022).
    #[test]
    fn every_rule_in_a_rule_path_has_a_case() {
        let root = crate::workspace_root();
        assert_eq!(rules_without_cases(&root, &root).unwrap(), Vec::<String>::new());
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-uncovered");
        scratch.write(super::REGISTRY, &real_registry());
        scratch.write("xtask/src/tidy/text.rs", "const RULE: &str = \"assertion-density\";\n");
        let uncovered = rules_without_cases(scratch.root(), scratch.root()).unwrap();
        assert_eq!(uncovered, ["assertion-density (xtask/src/tidy/text.rs)"]);
    }
}
