//! The lints that the project's dylint library declares (`lints/`, ADR-0006).
//!
//! Tidy does not run them; `just dylint` does. Tidy reads their declarations, so that the style
//! guide can only name a lint that exists (`doc-tag`) and `docs/RULES.md` can list them.

use super::tokens;
use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::io;
use std::path::Path;

/// Where the dylint library's sources live, relative to the root.
pub const LINT_SOURCES: &str = "lints/src";

/// The lint levels that rustc knows, as they are written in a declaration.
const LEVELS: [&str; 4] = ["Allow", "Warn", "Deny", "Forbid"];

/// The levels at which a dylint lint fails the gate. `Warn` is not one of them: `just dylint`
/// does not deny warnings.
const ENFORCED_LEVELS: [&str; 2] = ["Deny", "Forbid"];

/// One lint declared with `declare_late_lint!` or one of its siblings.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DylintLint {
    /// The lint's name in lower case, as it is written in attributes and tags.
    pub name: String,
    /// Its default level: `Allow`, `Warn`, `Deny` or `Forbid`.
    pub level: String,
    /// The one-line description from the declaration.
    pub description: String,
}

impl DylintLint {
    /// True if a finding of this lint fails `just dylint`.
    pub fn is_enforced(&self) -> bool {
        ENFORCED_LEVELS.contains(&self.level.as_str())
    }
}

/// Every lint declared below `lints/src/`, sorted by name. A tree without that directory has
/// no lints.
pub fn declared(root: &Path) -> io::Result<Vec<DylintLint>> {
    let mut lints = Vec::new();
    if !root.join(LINT_SOURCES).is_dir() {
        return Ok(lints);
    }
    for relative in super::files_below(root, Path::new(LINT_SOURCES))? {
        if relative.extension().is_some_and(|extension| extension == "rs") {
            lints.extend(declarations_in(&fs::read_to_string(root.join(&relative))?));
        }
    }
    lints.sort();
    Ok(lints)
}

/// The lint declarations in one source file. A declaration is the token run
/// `pub NAME, Level, "description"`, which is what `declare_late_lint!`, `declare_early_lint!`,
/// `impl_late_lint!` and rustc's `declare_lint!` all take.
fn declarations_in(text: &str) -> Vec<DylintLint> {
    let Ok(stream) = text.parse::<TokenStream>() else {
        return Vec::new();
    };
    let mut lints = Vec::new();
    for sequence in tokens::sequences(stream) {
        lints.extend(sequence.windows(6).filter_map(declaration));
    }
    lints
}

/// Reads six tokens as `pub NAME, Level, "description"`, if that is what they are.
fn declaration(window: &[TokenTree]) -> Option<DylintLint> {
    let [
        TokenTree::Ident(public),
        TokenTree::Ident(name),
        first_comma,
        TokenTree::Ident(level),
        second_comma,
        TokenTree::Literal(literal),
    ] = window
    else {
        return None;
    };
    let has_commas = tokens::is_punct(first_comma, ',') && tokens::is_punct(second_comma, ',');
    let has_level = LEVELS.iter().any(|known| level == known);
    let syn::Lit::Str(description) = syn::Lit::new(literal.clone()) else {
        return None;
    };
    (public == "pub" && has_commas && has_level).then(|| DylintLint {
        name: name.to_string().to_lowercase(),
        level: level.to_string(),
        description: description.value(),
    })
}

#[cfg(test)]
mod tests {
    use super::{DylintLint, declarations_in, declared};
    use crate::tidy::scratch::Scratch;

    const DECLARATION: &str = "dylint_linting::declare_late_lint! {\n    /// ### What it does\n    \
                               ///\n    /// Reports recursion.\n    pub NAMI_RECURSION,\n    \
                               Deny,\n    \"function that can call itself (ADR-0006)\"\n}\n";

    fn lint(name: &str, level: &str, description: &str) -> DylintLint {
        DylintLint {
            name: name.to_owned(),
            level: level.to_owned(),
            description: description.to_owned(),
        }
    }

    // Goal: a declaration is read with its name in lower case, its level and its description,
    // whatever the macro and the layout.
    #[test]
    fn declarations_are_read() {
        let expected = lint("nami_recursion", "Deny", "function that can call itself (ADR-0006)");
        assert_eq!(declarations_in(DECLARATION), [expected]);
        let one_line = "declare_lint! { pub NAMI_FLOAT_REDUCTION, Warn, \"naive float sum\" }";
        let warned = lint("nami_float_reduction", "Warn", "naive float sum");
        assert_eq!(declarations_in(one_line), [warned]);
        let with_state = "impl_late_lint! { pub A_LINT, Forbid, \"x\", State::new() }";
        assert_eq!(declarations_in(with_state), [lint("a_lint", "Forbid", "x")]);
    }

    // Goal: tokens that merely resemble a declaration are not lints.
    #[test]
    fn other_tokens_are_not_declarations() {
        assert_eq!(declarations_in("pub fn check(a: A, b: B) {}"), []);
        assert_eq!(declarations_in("m! { pub NAME, Loud, \"unknown level\" }"), []);
        assert_eq!(declarations_in("m! { pub NAME, Deny, 3 }"), [], "no description");
        assert_eq!(declarations_in("m! { NAME, Deny, \"not public\" }"), []);
        assert_eq!(declarations_in("// pub NAME, Deny, \"in a comment\""), []);
        assert_eq!(declarations_in("m! { pub NAME Deny \"no commas\" x y }"), []);
        assert_eq!(declarations_in("fn broken( {"), [], "a file that does not lex");
    }

    // Goal: only Deny and Forbid fail the gate; a Warn lint would pass `just dylint`.
    #[test]
    fn only_denied_lints_are_enforced() {
        assert!(lint("a", "Deny", "x").is_enforced(), "deny fails the gate");
        assert!(lint("a", "Forbid", "x").is_enforced(), "forbid fails the gate");
        assert!(!lint("a", "Warn", "x").is_enforced(), "warnings are not denied by dylint");
        assert!(!lint("a", "Allow", "x").is_enforced(), "an allowed lint reports nothing");
    }

    // Goal: lints are collected from every Rust file below `lints/src/`, sorted by name; the
    // UI test inputs next to it are not declarations, and a tree without lints has none.
    #[test]
    fn lints_are_collected_from_the_tree() {
        let scratch = Scratch::new("lints");
        assert_eq!(declared(scratch.root()).unwrap(), []);
        scratch.write("lints/src/lib.rs", DECLARATION);
        scratch.write("lints/src/more/float.rs", "declare_lint! { pub A_FIRST, Warn, \"a\" }");
        scratch.write("lints/src/notes.md", "pub NOT_RUST, Deny, \"ignored\"");
        scratch.write("lints/ui/main.rs", "declare_lint! { pub IN_UI, Deny, \"ignored\" }");
        let names: Vec<String> =
            declared(scratch.root()).unwrap().into_iter().map(|lint| lint.name).collect();
        assert_eq!(names, ["a_first", "nami_recursion"]);
        let real = declared(&crate::workspace_root()).unwrap();
        assert!(real.iter().any(|lint| lint.name == "nami_recursion" && lint.is_enforced()));
    }
}
