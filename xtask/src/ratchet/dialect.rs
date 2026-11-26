//! The dialect of Rust that the `syn` checks read soundly (ADR-0022, review of #57).
//!
//! The rule-set check reads the head's registry, the `check_*` call sites of its rule modules
//! and the lint library's declarations as data, without running the head. That is sound only
//! if the code that the compiler builds is the code that was read. A reader cannot follow
//! everything Rust can express: attributes that remove code, macros that generate it, names
//! that resolve to something else. So the rule paths are held to a small dialect, defined by
//! what it allows. A file outside it gives no verdict, so the code owner decides; it never
//! passes.
//!
//! In every rule-path Rust file, no raw identifier (`r#name`) appears anywhere. Beyond that:
//!
//! - **The registry** ([`registry`]): the tables `RULES`, `PATH_BANS`, `MACRO_BANS` and
//!   `PLANNED`, written as slices of struct literals or `planned(..)` calls whose arguments are
//!   string literals, `&[..]` of string literals, or enum variants written with their full type
//!   path (`BanScope::Everywhere`); doc comments only on the tables and nothing inside them; an
//!   inline `#[cfg(test)] mod tests`; and every other item token for token as in the base.
//! - **A rule module** ([`rules`]): the attributes `doc`, `derive`, `must_use`, `inline` and
//!   `expect` on items, fields and variants only, and `#[cfg(test)]` only on an inline
//!   `mod tests`, which the readers skip; no other module; no item macro and no macro but the
//!   standard expression macros of [`rules::MACROS`]; no glob or renamed import; a table, or
//!   the `registry` module, named only as the registry's own; and every `check_*` name either
//!   the definition of a top-level function or of a method in an inherent `impl`, or a call
//!   `self.check_*(..)` or `check_*(..)` that resolves to such a definition in the file.
//! - **The lint library** ([`lints`]): exactly the files that `lib.rs` declares with `mod`;
//!   no `#[path]`, no `include!` and no `macro_rules!`; `#[cfg(test)]` only on an inline
//!   module; each lint declared at the top of a file with `declare_lint!` or
//!   `declare_tool_lint!` in the form `pub NAME, Level, "description"`, and no other binding
//!   of its name.

pub mod fixed;
pub mod lints;
pub mod registry;
pub mod rules;

use proc_macro2::{TokenStream, TokenTree};
use std::path::Path;

/// The names of the registry's tables that the rule-set check reads.
pub const TABLES: [&str; 3] = ["RULES", "PATH_BANS", "MACRO_BANS"];

/// Upper bound on the token trees of one file, so that every walk is bounded.
const TOKENS_MAX: u32 = 2_000_000;

/// The no-verdict message for a file outside the dialect.
fn outside(relative: &str, problem: &str) -> String {
    format!(
        "{relative} holds {problem}, which is outside the dialect that the ratchet reads soundly \
         (ADR-0022); the rule-set check gives no verdict, so the code owner decides"
    )
}

/// Every identifier in `stream`, macro bodies included, in order.
fn identifiers(stream: TokenStream) -> Vec<proc_macro2::Ident> {
    let mut pending: Vec<TokenTree> = stream.into_iter().collect();
    pending.reverse();
    let mut found = Vec::new();
    for _ in 0..TOKENS_MAX {
        let Some(tree) = pending.pop() else {
            return found;
        };
        match tree {
            TokenTree::Ident(ident) => found.push(ident),
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                pending.extend(inner.into_iter().rev());
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
    panic!("a file has more than {TOKENS_MAX} token trees");
}

/// Every string literal in `stream`, macro bodies included, by value, and the value of every
/// `concat!` of string literals, so that an id split across a `concat!` is still seen.
fn string_literals(stream: TokenStream) -> Vec<String> {
    let mut pending: Vec<Vec<TokenTree>> = vec![stream.into_iter().collect()];
    let mut found = Vec::new();
    for _ in 0..TOKENS_MAX {
        let Some(sequence) = pending.pop() else {
            return found;
        };
        for (index, tree) in sequence.iter().enumerate() {
            if let TokenTree::Literal(literal) = tree {
                found.extend(string_value(literal));
            }
            let TokenTree::Group(group) = tree else {
                continue;
            };
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            found.extend(follows_concat(&sequence, index).then(|| concatenated(&inner)));
            pending.push(inner);
        }
    }
    panic!("a file has more than {TOKENS_MAX} token sequences");
}

/// True if the group at `index` of `sequence` is the body of `concat!`.
fn follows_concat(sequence: &[TokenTree], index: usize) -> bool {
    let Some(start) = index.checked_sub(2) else {
        return false;
    };
    let name = matches!(sequence.get(start), Some(TokenTree::Ident(name)) if name == "concat");
    let bang =
        matches!(sequence.get(start + 1), Some(TokenTree::Punct(bang)) if bang.as_char() == '!');
    name && bang
}

/// The string literals among `parts`, joined: what `concat!` makes of them.
fn concatenated(parts: &[TokenTree]) -> String {
    let values: Vec<String> = parts
        .iter()
        .filter_map(|part| {
            let TokenTree::Literal(literal) = part else {
                return None;
            };
            string_value(literal)
        })
        .collect();
    values.concat()
}

/// The value of a string literal token, if it is one.
fn string_value(literal: &proc_macro2::Literal) -> Option<String> {
    syn::parse_str::<syn::LitStr>(&literal.to_string()).ok().map(|text| text.value())
}

/// Parses `text`, and refuses a raw identifier anywhere in it.
///
/// # Errors
///
/// No verdict: the file does not parse, or holds `r#name`.
fn parse(relative: &str, text: &str) -> Result<syn::File, String> {
    let stream: TokenStream =
        text.parse().map_err(|error| format!("{relative} does not lex: {error}"))?;
    if let Some(raw) = identifiers(stream).iter().find(|ident| ident.to_string().starts_with("r#"))
    {
        return Err(outside(relative, &format!("the raw identifier `{raw}`")));
    }
    syn::parse_file(text).map_err(|error| format!("{relative} does not parse: {error}"))
}

/// True for a doc comment, which is `#[doc = "..."]`.
fn is_doc(attribute: &syn::Attribute) -> bool {
    attribute.path().is_ident("doc") && matches!(attribute.meta, syn::Meta::NameValue(_))
}

/// True for exactly `#[cfg(test)]`.
fn is_cfg_test(attribute: &syn::Attribute) -> bool {
    attribute.path().is_ident("cfg")
        && attribute.parse_args::<syn::Ident>().is_ok_and(|ident| ident == "test")
}

/// True for an inline `mod tests { .. }` whose only attributes are docs and `#[cfg(test)]`.
pub(crate) fn is_test_module(module: &syn::ItemMod) -> bool {
    let cfg_test = module.attrs.iter().filter(|attribute| is_cfg_test(attribute)).count();
    let others = module.attrs.iter().all(|attribute| is_doc(attribute) || is_cfg_test(attribute));
    module.ident == "tests" && module.content.is_some() && cfg_test == 1 && others
}

/// Checks every rule-path Rust file of the head against the dialect. `base` provides what the
/// registry's other items must equal.
///
/// # Errors
///
/// No verdict: a file that cannot be read, or that is outside the dialect.
pub fn check_head(base: &Path, head: &Path) -> Result<(), String> {
    let registry_path = super::source::REGISTRY;
    if let Some(text) = super::source::read_optional(head, registry_path)? {
        let base_text = super::source::read_optional(base, registry_path)?
            .ok_or("the base has no rule registry")?;
        registry::check(&base_text, &text)?;
    }
    let library = lints::library(head)?;
    let lint_files = library.files.iter().map(String::as_str);
    for relative in super::source::RULE_MODULES.into_iter().chain(lint_files) {
        let Some(text) = super::source::read_optional(head, relative)? else {
            continue;
        };
        if super::source::RULE_MODULES.contains(&relative) {
            let _sites = rules::call_sites(relative, &text)?;
        }
        let base_text = super::source::read_optional(base, relative)?;
        fixed::check(relative, base_text.as_deref(), &text)?;
    }
    Ok(())
}

/// A path as written, such as `syn::visit::Visit`.
fn path_text(path: &syn::Path) -> String {
    let names: Vec<String> =
        path.segments.iter().map(|segment| segment.ident.to_string()).collect();
    let text = names.join("::");
    if path.leading_colon.is_some() { format!("::{text}") } else { text }
}

/// The tokens of `text` without its test module: the top-level run `#[cfg(test)] mod tests
/// { .. }`, which the compiler leaves out of the tidy binary.
///
/// # Errors
///
/// No verdict: the file does not lex.
fn without_tests(relative: &str, text: &str) -> Result<TokenStream, String> {
    let stream: TokenStream =
        text.parse().map_err(|error| format!("{relative} does not lex: {error}"))?;
    let all: Vec<TokenTree> = stream.into_iter().collect();
    let is_test_run = |run: &[TokenTree]| match run {
        [
            TokenTree::Punct(hash),
            TokenTree::Group(attribute),
            TokenTree::Ident(keyword),
            TokenTree::Ident(name),
            TokenTree::Group(body),
        ] => {
            let inside: String =
                attribute.stream().to_string().chars().filter(|c| !c.is_whitespace()).collect();
            hash.as_char() == '#'
                && inside == "cfg(test)"
                && keyword == "mod"
                && name == "tests"
                && body.delimiter() == proc_macro2::Delimiter::Brace
        }
        _ => false,
    };
    let mut kept = Vec::with_capacity(all.len());
    let mut index = 0;
    for _ in 0..all.len() {
        let Some(tree) = all.get(index) else {
            break;
        };
        if all.get(index..index + 5).is_some_and(is_test_run) {
            index += 5;
        } else {
            kept.push(tree.clone());
            index += 1;
        }
    }
    assert!(kept.len() <= all.len(), "only tokens are dropped");
    Ok(kept.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::{check_head, lints, registry, rules};

    fn real(relative: &str) -> String {
        std::fs::read_to_string(crate::workspace_root().join(relative)).unwrap()
    }

    /// Replaces the one occurrence of `from` in `text`.
    fn replaced(text: &str, from: &str, to: &str) -> String {
        assert_eq!(text.matches(from).count(), 1, "the probe's anchor `{from}`");
        text.replacen(from, to, 1)
    }

    // Goal: one probe per rule of a rule module's dialect, each the only form outside it in
    // its module, gives no verdict with its own reason; and the forms the dialect allows pass.
    #[test]
    fn each_rule_module_rule_holds_alone() {
        let refused = [
            ("fn a() { let PATH_BANS = 1; }", "a binding named `PATH_BANS`"),
            ("const RULES: u8 = 0;", "a constant named `RULES`"),
            ("static MACRO_BANS: u8 = 0;", "a static named `MACRO_BANS`"),
            ("struct A;\nimpl A { const RULES: u8 = 0; }", "an associated constant named"),
            ("trait T { #[cfg(unix)] fn f(); }", "on a trait method"),
            ("struct S { #[cfg(unix)] a: u8 }", "on a field"),
            ("enum E { #[cfg(unix)] A }", "on a variant"),
            ("fn a() -> u8 { crate::other::PATH_BANS }", "the path `crate::other::PATH_BANS`"),
            ("fn a() -> u8 { other::RULES }", "the path `other::RULES`"),
            ("use crate::other::PATH_BANS;", "the import `crate::other::PATH_BANS`"),
            ("use crate::other::registry;", "the import `crate::other::registry`"),
            ("use other::registry::{self};", "the import `other::registry::self`"),
            ("fn a() { let _ = concat!(\"a\", 1); }", "a `concat!` of something other"),
        ];
        for (text, reason) in refused {
            let error = rules::call_sites("m.rs", text).unwrap_err();
            assert!(error.contains(reason), "{text}: {error}");
        }
        let allowed = [
            "use super::registry::{self, PATH_BANS};",
            "use crate::tidy::registry::RULES;\nfn a() -> u8 { registry::MACRO_BANS }",
            "use std::fmt::{self};\nfn a() -> u8 { super::registry::RULES }",
            "fn a() { let _ = concat!(\"a\", \"b\"); }",
        ];
        for text in allowed {
            assert!(rules::call_sites("m.rs", text).is_ok(), "{text}");
        }
    }

    // Goal: the literals of a module outside its test module are read, with each `concat!` of
    // literals joined, and only the exact `#[cfg(test)] mod tests { .. }` is left out.
    #[test]
    fn literals_are_read_outside_the_tests() {
        let text = "fn a() { let _ = concat!(\"li\", \"bm\"); let _ = vec![\"x\", \"y\"]; }\n\
                    #[cfg(test)]\nmod tests { const T: &str = \"test\"; }\n\
                    #[doc = \"d\"]\nmod tests { const K: &str = \"kept\"; }\n\
                    #[must_use]\nimpl tests { const L: &str = \"also\"; }\n";
        let found = rules::literals("m.rs", text).unwrap();
        let expected: std::collections::BTreeSet<String> =
            ["li", "bm", "libm", "x", "y", "d", "kept", "also"].map(str::to_owned).into();
        assert_eq!(found, expected);
    }

    // Goal: a changed or new constant or static in a rule module or the lint library, and new
    // code that reads the environment, a process or a file, gives no verdict; the base's own
    // pass, and a function that changes without them is in the dialect.
    #[test]
    fn constants_and_the_environment_stay_the_bases() {
        let module = "xtask/src/tidy/rust_source.rs";
        let base = real(module);
        assert_eq!(super::fixed::check(module, Some(&base), &base), Ok(()));
        let probes = [
            replaced(
                &base,
                "const MAGIC_TOLERANCE_MAX: f64 = \
                             1.0e-6;",
                "const MAGIC_TOLERANCE_MAX: f64 = 1.0e-9;",
            ),
            format!(
                "{base}\nconst PATH_BAN_ROWS: &[(&str, &[&str])] = &[(\"direct-libm\", \
                     &[\"crates/solver\"])];\n"
            ),
            format!(
                "{base}\nfn quiet() -> bool {{ std::env::var_os(\"GITHUB_JOB\").is_some() \
                     }}\n"
            ),
            format!("{base}\nfn run() {{ let _ = std::process::Command::new(\"true\"); }}\n"),
            format!("{base}\nfn read() {{ let _ = std::fs::read_to_string(\"x\"); }}\n"),
            format!("{base}\nfn job() -> Option<&'static str> {{ option_env!(\"CI\") }}\n"),
        ];
        for (index, probe) in probes.iter().enumerate() {
            assert!(super::fixed::check(module, Some(&base), probe).is_err(), "probe {index}");
        }
        let changed = super::fixed::check(module, Some(&base), &probes[0]).unwrap_err();
        assert!(changed.contains("`MAGIC_TOLERANCE_MAX`"), "{changed}");
        let reads = super::fixed::check(module, Some(&base), &probes[2]).unwrap_err();
        assert!(reads.contains("in `quiet`"), "the message names the function: {reads}");
        let line = base.lines().count() + 2;
        let named = [
            ("use std::env::var;", format!("in `an import at line {line}`")),
            ("type Env = std::env::Args;", format!("in `the item at line {line}`")),
        ];
        for (addition, expected) in named {
            let error = super::fixed::check(module, Some(&base), &format!("{base}\n{addition}\n"));
            assert!(error.as_ref().unwrap_err().contains(&expected), "{error:?}");
        }
        let more = [
            "static LIMIT: u8 = 1;",
            "struct Extra;\nimpl Extra { const LIMIT: u8 = 1; }",
            "struct Extra;\nimpl Extra { fn f() { let _ = std::env::var(\"X\"); } }",
        ];
        for addition in more {
            let probe = format!("{base}\n{addition}\n");
            assert!(super::fixed::check(module, Some(&base), &probe).is_err(), "{addition}");
        }
        assert!(super::fixed::check(module, None, "const A: u8 = 1;").is_err(), "a new module");
        let unrelated = format!("{base}\nfn extra() -> u8 {{ 1 }}\n");
        assert_eq!(super::fixed::check(module, Some(&base), &unrelated), Ok(()));
        let lib = real("lints/src/lib.rs");
        let device = replaced(
            &lib,
            "    dylint_linting::init_config(session);",
            "    if std::env::var_os(\"GITHUB_JOB\").is_some() { return; }\n    \
             dylint_linting::init_config(session);",
        );
        assert!(super::fixed::check("lints/src/lib.rs", Some(&lib), &device).is_err());
    }

    // Goal: the registration counts only as an unconditional statement of `register_lints`.
    #[test]
    fn registration_is_unconditional() {
        let registration = "    lint_store.register_lints(&[NAMI_RECURSION, \
                            NAMI_FLOAT_REDUCTION]);";
        let conditional = "    if session.is_nightly_build() {\n        \
                           lint_store.register_lints(&[NAMI_RECURSION, \
                           NAMI_FLOAT_REDUCTION]);\n    }";
        let guarded =
            library_with("ratchet-lints-if", |text| replaced(text, registration, conditional), &[]);
        assert!(guarded.unwrap_err().contains("unconditional"), "an `if` around it");
        let closure = "    let register = || lint_store.register_lints(&[NAMI_RECURSION, \
                       NAMI_FLOAT_REDUCTION]);\n    register();";
        let probe = library_with(
            "ratchet-lints-closure",
            |text| replaced(text, registration, closure),
            &[],
        );
        assert!(probe.is_err(), "a closure around it");
        let exits = [
            "    if session.is_nightly_build() { return; }\n",
            "    session.opts().map(|_| ()).ok()?;\n",
            "    quiet!();\n",
        ];
        for (index, exit) in exits.iter().enumerate() {
            let name = format!("ratchet-lints-exit-{index}");
            let early = library_with(
                &name,
                |text| replaced(text, registration, &format!("{exit}{registration}")),
                &[],
            );
            assert!(early.unwrap_err().contains("unconditional"), "{exit}");
        }
        let after = "    lint_store.register_lints(&[NAMI_RECURSION, NAMI_FLOAT_REDUCTION]);\n    \
                     if session.is_nightly_build() { return; }";
        let late =
            library_with("ratchet-lints-late", |text| replaced(text, registration, after), &[]);
        assert!(late.is_ok(), "a way out after the registration: {late:?}");
    }

    // Goal: the real rule-path files are in the dialect, so the check never stops honest PRs.
    #[test]
    fn the_rule_paths_are_in_the_dialect() {
        let root = crate::workspace_root();
        assert_eq!(check_head(&root, &root), Ok(()));
    }

    // Goal: every form that the review of #57 used to hide a loosening in the registry, and its
    // neighbours, gives no verdict: an attribute inside a table, a variant renamed by an import
    // or a constant, a variant path that is not `Enum::Variant`, a computed value, and any
    // change outside the tables.
    #[test]
    fn registry_forms_outside_the_dialect_give_no_verdict() {
        let base = real(super::super::source::REGISTRY);
        assert_eq!(registry::check(&base, &base), Ok(()));
        let table = "pub const PATH_BANS: &[PathBan] = &[PathBan {";
        let libm_scope = "    scope: BanScope::Everywhere,\n    message: \"direct use of `libm`";
        let probes = [
            replaced(&base, table, "pub const PATH_BANS: &[PathBan] = &[#[cfg(test)] PathBan {"),
            replaced(
                &base,
                "use super::Scope;",
                "use super::Scope;\nuse BanScope::NamiCrates as Everywhere;",
            ),
            replaced(
                &base,
                libm_scope,
                "    scope: Everywhere,\n    message: \"direct use of `libm`",
            ),
            replaced(
                &base,
                "use super::Scope;",
                "use super::Scope;\nconst Everywhere: BanScope = BanScope::NamiCrates;",
            ),
            replaced(
                &base,
                "use super::Scope;",
                "use super::Scope;\nuse RuleKind::Workspace as Source;",
            ),
            replaced(
                &base,
                libm_scope,
                "    scope: self::BanScope::Everywhere,\n    message: \"direct use of `libm`",
            ),
            replaced(
                &base,
                libm_scope,
                "    scope: BanScope::Somewhere,\n    message: \"direct use of `libm`",
            ),
            replaced(&base, "    segment: \"libm\",", "    #[cfg(test)]\n    segment: \"libm\","),
            replaced(&base, "    segment: \"libm\",", "    segment: concat!(\"li\", \"bm\"),"),
            replaced(&base, "self == Self::Everywhere || scope.is_nami()", "scope.is_nami()"),
            replaced(&base, "pub const RULES: &[Rule]", "#[cfg(any())]\npub const RULES: &[Rule]"),
            format!("{base}\nmacro_rules! live {{ () => {{}} }}\nlive!();\n"),
            format!("{base}\nconst r#PATH_BANS: &[PathBan] = &[];\n"),
            replaced(
                &base,
                "    planned(\n        \"alloc-after-init\",",
                "    unplanned(\n        \"alloc-after-init\",",
            ),
            replaced(&base, "    segment: \"libm\",", "    segment: \"libm\"suffix,"),
            replaced(
                &base,
                libm_scope,
                "    scope: ::BanScope::Everywhere,\n    message: \
                                         \"direct use of `libm`",
            ),
        ];
        for (index, probe) in probes.iter().enumerate() {
            let verdict = registry::check(&base, probe);
            assert!(verdict.is_err(), "probe {index} must give no verdict");
        }
    }

    // Goal: every form that the review of #57 used to hide a loosening in a rule module gives no
    // verdict: `#[cfg(test)]` on a statement, a raw identifier, `#[path]` with a renamed
    // registry, and calls of a `check_*` name in another form; and so do their neighbours.
    #[test]
    fn rule_module_forms_outside_the_dialect_give_no_verdict() {
        let module = "xtask/src/tidy/rust_source.rs";
        let base = real(module);
        assert!(rules::call_sites(module, &base).is_ok());
        let derives = "        self.check_derives(&node.attrs);\n        visit::visit_item_struct";
        let bans = "        for ban in PATH_BANS {\n            let applies";
        let import = "use super::registry::{self, MACRO_BANS, PATH_BANS};";
        let statements = [
            "#[cfg(test)]\nself.check_derives(&node.attrs);",
            "#[expect(unused)]\nself.check_derives(&node.attrs);",
            "Self::check_derives(self, &node.attrs);",
            "let f = Self::check_derives;",
            "fn check_derives(_: &[Attribute]) {}",
            "let _ = format!(\"{}\", stringify!(check_derives));",
            "let _ = include_str!(\"x\");",
        ];
        let mut probes: Vec<String> = statements
            .iter()
            .map(|statement| {
                replaced(&base, derives, &format!("{statement}\n        visit::visit_item_struct"))
            })
            .collect();
        probes.extend([
            replaced(
                &base,
                bans,
                "const r#PATH_BANS: &[registry::PathBan] = &[];\nfor ban in \
                                   PATH_BANS {\nlet applies",
            ),
            replaced(
                &base,
                import,
                "use super::registry as rules;\n#[path = \"tables.rs\"]\nmod \
                                     registry;\nuse registry::{MACRO_BANS, PATH_BANS};",
            ),
            replaced(&base, import, "use crate::other::{self as registry, MACRO_BANS, PATH_BANS};"),
            replaced(&base, import, "use super::registry::*;"),
            format!("{base}\ntrait Look {{ fn check_derives(&mut self, _: &[Attribute]); }}\n"),
            format!("{base}\n#[cfg(unix)]\nfn extra() {{}}\n"),
            format!("{base}\n#[cfg_attr(test, derive(Debug))]\nstruct Extra;\n"),
            format!("{base}\nmod extra {{}}\n"),
            format!("{base}\nfn extra() {{ impl Visitor<'_> {{ fn check_more(&self) {{}} }} }}\n"),
        ]);
        for (index, probe) in probes.iter().enumerate() {
            assert!(
                rules::call_sites(module, probe).is_err(),
                "probe {index} must give no verdict"
            );
        }
    }

    // Goal: a call redirected from `self.check_derives` to a free function of the same name is
    // in the dialect, but it is another call site, so the rule-set check reports it.
    #[test]
    fn a_look_alike_call_is_another_call_site() {
        let module = "xtask/src/tidy/rust_source.rs";
        let base = real(module);
        let head = replaced(
            &base,
            "        self.check_derives(&node.attrs);\n        visit::visit_item_struct",
            "        check_derives(&node.attrs);\n        visit::visit_item_struct",
        );
        let head = format!("{head}\nfn check_derives(_: &[Attribute]) {{}}\n");
        let before = rules::call_sites(module, &base).unwrap();
        let after = rules::call_sites(module, &head).unwrap();
        let site = ("Visitor::visit_item_struct".to_owned(), "self.check_derives".to_owned());
        assert!(before.contains(&site) && !after.contains(&site), "{after:?}");
    }

    /// A tree whose lint library is the real one, with `edit` applied to `lib.rs` and `extra`
    /// files written.
    fn library_with(
        name: &str,
        edit: impl Fn(&str) -> String,
        extra: &[(&str, &str)],
    ) -> Result<lints::Library, String> {
        let scratch = crate::tidy::scratch::Scratch::new(name);
        for file in ["lints/src/float_reduction.rs", "lints/src/recursion.rs"] {
            scratch.write(file, &real(file));
        }
        scratch.write("lints/src/lib.rs", &edit(&real("lints/src/lib.rs")));
        for (relative, text) in extra {
            scratch.write(relative, text);
        }
        lints::library(scratch.root())
    }

    // Goal: the lint library is read with its levels and registrations, and every form that the
    // review of #57 used to hide a lowered lint gives no verdict: a description that is not a
    // literal, another visibility, an uncompiled file, `#[path]`, a shadowing static, a renamed
    // declaring macro, a declaration inside a function, and a macro that may declare a lint.
    #[test]
    fn lint_library_forms_outside_the_dialect_give_no_verdict() {
        let real_library = library_with("ratchet-lints-real", str::to_owned, &[]).unwrap();
        let levels: Vec<(&str, &str)> = real_library
            .declarations
            .iter()
            .map(|lint| (lint.name.as_str(), lint.level.as_str()))
            .collect();
        assert_eq!(levels, [("nami_float_reduction", "Deny"), ("nami_recursion", "Deny")]);
        assert_eq!(real_library.registered.len(), 2);
        let declaration =
            "    pub NAMI_RECURSION,\n    Deny,\n    \"function that can call itself (ADR-0006)\"";
        let edits: [(&str, &str); 8] = [
            (
                declaration,
                "    pub NAMI_RECURSION,\n    Warn,\n    concat!(\"function that can call itself \
                 (ADR-0006)\")",
            ),
            (
                declaration,
                "    pub(crate) NAMI_RECURSION,\n    Warn,\n    \"function that can call itself \
                 (ADR-0006)\"",
            ),
            ("mod recursion;", "#[path = \"recursion.rs\"]\nmod recursion;"),
            (
                "    dylint_linting::init_config(session);",
                "    static NAMI_RECURSION: u8 = 0;\n    dylint_linting::init_config(session);",
            ),
            (
                "use rustc_session::Session;",
                "use rustc_session::Session;\nuse dylint_linting::declare_lint as quiet;",
            ),
            (
                "    dylint_linting::init_config(session);",
                "    dylint_linting::declare_lint! { pub NAMI_X, Allow, \"x\" }\n    \
                 dylint_linting::init_config(session);",
            ),
            (
                "dylint_linting::dylint_library!();",
                "dylint_linting::dylint_library!();\ndylint_linting::declare_late_lint! { pub \
                 NAMI_Y, Warn, \"y\" }",
            ),
            (
                "dylint_linting::dylint_library!();",
                "dylint_linting::dylint_library!();\ninclude!(\"more.rs\");",
            ),
        ];
        let description = "\"function that can call itself (ADR-0006)\"\n}";
        let more: [(&str, &str); 3] = [
            ("    Deny,\n    \"function that can", "    Denied,\n    \"function that can"),
            (description, "\"function that can call itself (ADR-0006)\"suffix\n}"),
            (description, "\"function that can call itself (ADR-0006)\", extra\n}"),
        ];
        for (index, (from, to)) in edits.iter().chain(&more).enumerate() {
            let probe = library_with("ratchet-lints-probe", |text| replaced(text, from, to), &[]);
            assert!(probe.is_err(), "probe {index} must give no verdict: {probe:?}");
        }
        let stray = [("lints/src/notes/levels.rs", "pub NAMI_RECURSION, Deny, \"x\"\n")];
        let uncompiled = library_with("ratchet-lints-stray", str::to_owned, &stray).unwrap_err();
        assert!(uncompiled.contains("does not declare"), "{uncompiled}");
    }

    // Goal: in a module file of the lint library, each form outside the dialect gives no
    // verdict with its own reason: a `#[cfg(test)]` that is not on `mod tests`, a declaration
    // below the top, a file module outside `lib.rs`, an `include` inside a macro, and a binding
    // of a declared lint's name.
    #[test]
    fn lint_module_forms_outside_the_dialect_give_no_verdict() {
        let in_module = [
            ("#[cfg(test)]\nmod other {}\n", "`#[cfg(test)]` on the module `other`"),
            ("#[cfg(test)]\nfn other() {}\n", "`#[cfg(test)]` on the function `other`"),
            ("mod inner { dylint_linting::declare_lint! { pub NAMI_Z, Deny, \"z\" } }\n", "below"),
            ("mod deeper;\n", "the file module `deeper` outside `lib.rs`"),
            ("fn f() { let _ = format!(\"{}\", include_str!(\"x\")); }\n", "`include_str` inside"),
            ("use crate::NAMI_RECURSION;\n", "an import named `NAMI_RECURSION`"),
            ("const NAMI_RECURSION: u8 = 0;\n", "a constant named `NAMI_RECURSION`"),
            ("fn f() { let NAMI_RECURSION = 0; }\n", "a binding named `NAMI_RECURSION`"),
        ];
        for (extra, reason) in in_module {
            let text = format!("{}\n{extra}", real("lints/src/recursion.rs"));
            let files = [("lints/src/recursion.rs", text.as_str())];
            let error = library_with("ratchet-lints-module", str::to_owned, &files).unwrap_err();
            assert!(error.contains(reason), "{extra}: {error}");
        }
    }
}
