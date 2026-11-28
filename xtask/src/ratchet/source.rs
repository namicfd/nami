//! What the base reads from the head's source as data, with `syn`, without running any head
//! code (ADR-0022): the rule tables, the `check_*` calls of the rule modules, and the lint
//! declarations and registrations. Each file is first held to the dialect of [`super::dialect`],
//! in which these readings are what the compiler builds.
//!
//! Mutation testing makes no mutant for a changed table row. These checks cover the registry's
//! tables as data: every base row must be in the head unchanged or narrowed in what it permits,
//! and every base `check_*` call site must still be there. That is a statement about the
//! tables and the call sites, not about what tidy enforces. How the rule modules apply a row is
//! code: an additive diff leaves that code as it is ([`super::class`]), and a diff that edits it
//! needs the reviewer app's approval, or a code owner's. Only the fields these checks compare,
//! and those that only document, are covered: a change to any other field of a row makes the
//! diff an edit ([`super::dialect::registry::unproven_rows`]). A table in a form this module
//! does not read is no verdict, never a pass.

use super::dialect;
use super::{Check, Finding, Reason, Step, Tools, Verdict};
use crate::tidy::lints::DylintLint;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
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

/// The variant of `expected` that a field names, written as `Enum::Variant` in full.
fn variant(expr: &Expr, expected: &str) -> Result<String, String> {
    let named = if let Expr::Path(path) = expr
        && path.qself.is_none()
    {
        dialect::registry::variant_of(&path.path)
    } else {
        None
    };
    match named {
        Some((name, variant)) if name == expected => Ok(variant),
        _ => Err(format!("a field in {REGISTRY} is not written as `{expected}::<variant>`")),
    }
}

fn rule_row(row: &Expr) -> Result<(String, String), String> {
    let fields = fields(row)?;
    Ok((string(field(&fields, "id")?)?, variant(field(&fields, "kind")?, "RuleKind")?))
}

fn ban_row(row: &Expr, table: &str) -> Result<Ban, String> {
    let fields = fields(row)?;
    let id = string(field(&fields, "id")?)?;
    let allowed = strings(field(&fields, "allowed")?)?;
    let scope = variant(field(&fields, "scope")?, "BanScope")
        .map_err(|problem| format!("ban `{id}`: {problem}, with a scope the ratchet knows"))?;
    assert!(SCOPES.contains(&scope.as_str()), "the dialect knows only these scopes");
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

/// The `check_*` call sites of one module, keyed as in [`dialect::rules`].
///
/// # Errors
///
/// No verdict: the module does not parse, or is outside the dialect.
pub fn check_calls(relative: &str, text: &str) -> Result<BTreeSet<(String, String)>, String> {
    dialect::rules::call_sites(relative, text)
}

/// Compares what the base reads from both trees, into `verdict`, as part of the step
/// [`Step::RuleSet`]: the registry, the `check_*` calls of each rule module, and the lints
/// declared and registered. Each reading holds its own files of the head to the dialect, as
/// [`dialect::check_head`] does for all of them, and fails alone: a file that cannot be read,
/// or that is outside the dialect, leaves out only the readings of that file, and the others'
/// findings are recorded (review of #57, round 13). A head file that cannot be read so is a
/// form the ratchet refuses ([`Reason::Refused`]); a base file that cannot be read is missing
/// evidence ([`Reason::Evidence`]).
pub fn check(base: &Path, head: &Path, tools: &Tools<'_>, verdict: &mut Verdict) {
    let readings = [registry_findings(base, head)]
        .into_iter()
        .chain(RULE_MODULES.iter().map(|module| call_findings(base, head, module)))
        .chain([lint_findings(base, head)]);
    for reading in readings {
        let injected = verdict.inject(Step::RuleSet, tools, Ok::<_, String>(()));
        match (injected, reading) {
            (Err(reason), _) => verdict.fail(Step::RuleSet, reason),
            (Ok(()), Ok(findings)) => {
                for finding in findings {
                    verdict.record(finding);
                }
            }
            (Ok(()), Err((why, reason))) => verdict.fail_for(Step::RuleSet, why, reason),
        }
    }
}

/// What one reading of [`check`] found, or why it could not read: the kind and the reason.
type Reading = Result<Vec<Finding>, (Reason, String)>;

/// A failure to read the head as the dialect allows.
fn refused(reason: String) -> (Reason, String) {
    (Reason::Refused, reason)
}

/// A failure to read the base.
fn unread(reason: String) -> (Reason, String) {
    (Reason::Evidence, reason)
}

/// The registry's rows that the head loosens.
///
/// # Errors
///
/// No verdict: either registry is missing or does not parse, or the head's is outside the
/// dialect.
fn registry_findings(base: &Path, head: &Path) -> Reading {
    let read = |root: &Path| -> Result<String, String> {
        read_optional(root, REGISTRY)?.ok_or_else(|| format!("{REGISTRY} is missing"))
    };
    let head_text = read(head).map_err(refused)?;
    dialect::registry::check(&head_text).map_err(refused)?;
    let after = parse_registry(&head_text).map_err(refused)?;
    let before = read(base).and_then(|text| parse_registry(&text)).map_err(unread)?;
    Ok(loosened_rows(&before, &after))
}

/// The `check_*` calls of the rule module `module` that the base makes and the head does not.
///
/// # Errors
///
/// No verdict: the module cannot be read, or either side is outside the dialect.
fn call_findings(base: &Path, head: &Path, module: &str) -> Reading {
    let after = match read_optional(head, module).map_err(refused)? {
        Some(text) => check_calls(module, &text).map_err(refused)?,
        None => BTreeSet::new(),
    };
    let Some(base_text) = read_optional(base, module).map_err(unread)? else {
        return Ok(Vec::new());
    };
    let before = check_calls(module, &base_text).map_err(unread)?;
    let findings: Vec<Finding> = before
        .difference(&after)
        .map(|(function, call)| {
            let message = format!(
                "`{function}` no longer calls `{call}(..)`; a rule's check is never removed \
                 without the code owner (ADR-0022)"
            );
            Finding::new(module, Check::RuleSet, message)
        })
        .collect();
    assert!(findings.len() <= before.len(), "at most one finding per base call");
    Ok(findings)
}

/// Lints: every lint the base enforces is declared exactly once in the head, still enforced,
/// and still registered.
fn lint_findings(base: &Path, head: &Path) -> Reading {
    let before =
        dialect::lints::library(base).map_err(|problem| unread(format!("the base: {problem}")))?;
    let after = dialect::lints::library(head).map_err(refused)?;
    let mut findings = weakened_lints(&before.declarations, &after.declarations);
    for lint in before.registered.difference(&after.registered) {
        let message = format!(
            "dylint lint `{lint}` is registered in the base and not in the head; a lint that is \
             not registered never runs (ADR-0022)"
        );
        findings.push(Finding::new(LINT_LIBRARY, Check::RuleSet, message));
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

/// The base's rules whose code sits in a rule path (their id is a string literal there, read
/// from the module's tokens outside its tests) and that have no firing fixture in the base.
/// Each needs a case before the ratchet can vouch for a change to it (ADR-0022).
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
        let literals = dialect::rules::literals(module, &text)?;
        for id in registry.ids() {
            let fixture =
                base.join(super::corpus::FIXTURE_DIRECTORY).join(format!("{id}.fires.fixture"));
            if literals.contains(&id) && !fixture.is_file() {
                uncovered.push(format!("{id} ({module})"));
            }
        }
    }
    Ok(uncovered)
}

/// The head's new rules, registered in the head and not in the base, whose code sits in a rule
/// path of the head and that have no firing fixture in the head. A new rule's case comes with
/// the head, which the canonical new rule shows; without one, nothing but the head's own
/// tests would say that it fires (review of #55, thread 2).
///
/// # Errors
///
/// Returns what is unreadable, a symbolic link among it.
pub fn new_rules_without_cases(base: &Path, head: &Path) -> Result<Vec<String>, String> {
    let base_text = read_optional(base, REGISTRY)?.ok_or("the base has no rule registry")?;
    let Some(head_text) = read_optional(head, REGISTRY)? else {
        return Ok(Vec::new());
    };
    let known = parse_registry(&base_text)?.ids();
    let added: Vec<String> =
        parse_registry(&head_text)?.ids().difference(&known).cloned().collect();
    let mut uncovered = Vec::new();
    for module in RULE_MODULES {
        let Some(text) = read_optional(head, module)? else {
            continue;
        };
        let literals = dialect::rules::literals(module, &text)?;
        for id in added.iter().filter(|id| literals.contains(*id)) {
            let fixture = format!("{}/{id}.fires.fixture", super::corpus::FIXTURE_DIRECTORY);
            if read_optional(head, &fixture)?.is_none() {
                uncovered.push(format!("{id} ({module})"));
            }
        }
    }
    Ok(uncovered)
}

#[cfg(test)]
mod tests {
    use super::{
        Ban, check_calls, loosened_rows, parse_registry, read_optional, rules_without_cases,
        weakened_lints,
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

    /// The frozen copy of the registry ([`crate::ratchet::frozen`]).
    fn real_registry() -> String {
        crate::ratchet::frozen(super::REGISTRY)
    }

    fn messages(base: &str, head: &str) -> Vec<String> {
        let findings =
            loosened_rows(&parse_registry(base).unwrap(), &parse_registry(head).unwrap());
        findings.into_iter().map(|finding| finding.message).collect()
    }

    // Goal: the real registry is read in full: every id its text spells out, and both bans
    // with their scope and allowed paths. The registry is the frozen copy, so the ids come from
    // its text, not from the compiled registry, which an additive diff grows.
    #[test]
    fn the_real_registry_is_read() {
        let text = real_registry();
        let registry = parse_registry(&text).unwrap();
        let spelled: BTreeSet<String> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("id: \""))
            .map(|rest| rest.trim_end_matches("\",").to_owned())
            .collect();
        assert!(spelled.len() > 30, "the frozen registry holds every rule of its day");
        assert_eq!(registry.ids(), spelled);
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
        assert!(parse_registry(&unknown).unwrap_err().contains("a scope the ratchet knows"));
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

    // Goal: a field names a variant of its own enum, and the rule-set check holds the head's
    // registry to the dialect: an attribute inside a table is no verdict. A change outside the
    // tables reads the same tables, so the rule-set check finds nothing, and the diff is an
    // edit, which the reviewer app or a code owner approves.
    #[test]
    fn the_registry_is_read_in_the_dialect() {
        let base = real_registry();
        let wrong_enum = base.replacen("kind: RuleKind::Source", "kind: BanScope::Everywhere", 1);
        assert!(parse_registry(&wrong_enum).unwrap_err().contains("`RuleKind::<variant>`"));
        let trees = crate::tidy::scratch::Scratch::new("ratchet-source-dialect");
        let (base_root, head_root) = (trees.root().join("base"), trees.root().join("head"));
        trees.write(&format!("base/{}", super::REGISTRY), &base);
        let hidden = base.replacen("    Rule {", "    #[cfg(any())]\n    Rule {", 1);
        trees.write(&format!("head/{}", super::REGISTRY), &hidden);
        let checked = || {
            let mut verdict = crate::ratchet::Verdict::default();
            super::check(&base_root, &head_root, &crate::ratchet::plain_tools(), &mut verdict);
            let settleable = verdict.is_settleable();
            (verdict.findings, verdict.failed, settleable)
        };
        let (found, failed, settleable) = checked();
        assert!(found.is_empty(), "{found:?}");
        assert!(failed.len() == 1 && failed[0].contains("outside the dialect"), "{failed:?}");
        assert!(settleable, "a head outside the dialect is a form the ratchet refuses");
        let narrowed =
            base.replace("self == Self::Everywhere || scope.is_nami()", "scope.is_nami()");
        trees.write(&format!("head/{}", super::REGISTRY), &narrowed);
        assert_eq!(checked(), (Vec::new(), Vec::new(), true));
        let paths = [super::REGISTRY.to_owned()];
        let class = crate::ratchet::class::classify(&base_root, &head_root, &paths).unwrap();
        assert!(class.edits[0].contains("`BanScope::includes` changed"), "{:?}", class.edits);
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

    // Goal: `check_*` calls are found keyed by their enclosing function and their form, so
    // that a deleted call is seen, and a call redirected to a look-alike is not the same call.
    #[test]
    fn check_calls_are_found() {
        let text = "fn check_x() {}\nfn a() { check_x(); helper(); }\nstruct V;\nimpl V { fn \
                    check_y(&self, _: u8) {}\n fn b(&mut self) { self.check_y(1); } }";
        let found = check_calls("m.rs", text).unwrap();
        let pairs: Vec<(&str, &str)> =
            found.iter().map(|(f, c)| (f.as_str(), c.as_str())).collect();
        assert_eq!(pairs, [("V::b", "self.check_y"), ("a", "check_x")]);
        let real = crate::ratchet::frozen("xtask/src/tidy/rust_source.rs");
        let calls = check_calls("rust_source.rs", &real).unwrap();
        let pair = ("Visitor::check_body".to_owned(), "self.check_float_equality".to_owned());
        assert!(calls.contains(&pair), "{calls:?}");
        let deleted = real.replace("self.check_float_equality(function);", "");
        assert!(!check_calls("rust_source.rs", &deleted).unwrap().contains(&pair));
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

    // Goal: every rule whose id appears in a rule-path module has a firing fixture, the
    // invariant that lets the ratchet vouch for a change to it (ADR-0022): the frozen `text.rs`
    // with the fixtures of its three rules has none uncovered, and a module that names a rule
    // with no fixture is reported. Frozen inputs, so that an additive change cannot break it.
    #[test]
    fn every_rule_in_a_rule_path_has_a_case() {
        let covered = crate::tidy::scratch::Scratch::new("ratchet-covered");
        covered.write(super::REGISTRY, &real_registry());
        let text = "xtask/src/tidy/text.rs";
        covered.write(text, &crate::ratchet::frozen(text));
        for id in ["line-length", "debt-marker", "test-goal"] {
            let fixture = format!("xtask/fixtures/tidy/{id}.fires.fixture");
            covered.write(&fixture, &crate::ratchet::frozen(&fixture));
        }
        let none = rules_without_cases(covered.root(), covered.root()).unwrap();
        assert_eq!(none, Vec::<String>::new());
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-uncovered");
        scratch.write(super::REGISTRY, &real_registry());
        scratch.write(text, "const RULE: &str = \"assertion-density\";\n");
        let uncovered = rules_without_cases(scratch.root(), scratch.root()).unwrap();
        assert_eq!(uncovered, ["assertion-density (xtask/src/tidy/text.rs)"]);
    }

    // Goal: review of #55, thread 2: a new rule's case must come with the head. The canonical
    // new rule with its fixtures has none missing; without its firing fixture it is named; and
    // a head with no registry, or a rule of the base, is not this check's.
    #[test]
    fn a_new_rule_needs_its_case_in_the_head() {
        let diff = crate::ratchet::canonical::TEXT_RULE;
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-new-rule-case");
        scratch.write(&format!("base/{}", super::REGISTRY), &real_registry());
        let fires = "xtask/fixtures/tidy/trailing-space.fires.fixture";
        for (path, _, head) in diff.changes().unwrap() {
            scratch.write(&format!("head/{path}"), &head);
        }
        let (base, head) = (scratch.root().join("base"), scratch.root().join("head"));
        assert_eq!(super::new_rules_without_cases(&base, &head).unwrap(), Vec::<String>::new());
        std::fs::remove_file(head.join(fires)).unwrap();
        let missing = super::new_rules_without_cases(&base, &head).unwrap();
        assert_eq!(missing, ["trailing-space (xtask/src/tidy/text.rs)"]);
        let unchanged = super::new_rules_without_cases(&base, &base).unwrap();
        assert_eq!(unchanged, Vec::<String>::new(), "no new rule");
        std::fs::remove_file(head.join(super::REGISTRY)).unwrap();
        assert_eq!(super::new_rules_without_cases(&base, &head).unwrap(), Vec::<String>::new());
    }
}
