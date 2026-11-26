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

/// The doc section that says what a lint sees and misses, the dylint counterpart of a tidy
/// rule's `detail` in the registry.
const WHAT_IT_DOES: &str = "### What it does";

/// Every lint declared below `lints/src/`, sorted by name. A tree without that directory has
/// no lints.
pub fn declared(root: &Path) -> io::Result<Vec<DylintLint>> {
    let mut lints: Vec<DylintLint> =
        sources(root)?.iter().flat_map(|text| declarations_in(text)).collect();
    lints.sort();
    Ok(lints)
}

/// The `### What it does` section of each lint's doc comment, as one paragraph, by lower-case
/// lint name and sorted by it. A lint without that section has none here.
pub fn documented(root: &Path) -> io::Result<Vec<(String, String)>> {
    let mut found: Vec<(String, String)> =
        sources(root)?.iter().flat_map(|text| documentation_in(text)).collect();
    found.sort();
    Ok(found)
}

/// The text of every Rust file below `lints/src/`.
fn sources(root: &Path) -> io::Result<Vec<String>> {
    let mut texts = Vec::new();
    if !root.join(LINT_SOURCES).is_dir() {
        return Ok(texts);
    }
    for relative in super::files_below(root, Path::new(LINT_SOURCES))? {
        if relative.extension().is_some_and(|extension| extension == "rs") {
            texts.push(fs::read_to_string(root.join(&relative))?);
        }
    }
    Ok(texts)
}

/// The `### What it does` section of each declaration in one file: the doc comment lines
/// right before `pub NAME, Level, "description"`, from that heading to the next one.
fn documentation_in(text: &str) -> Vec<(String, String)> {
    let Ok(stream) = text.parse::<TokenStream>() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for sequence in tokens::sequences(stream) {
        for (start, window) in sequence.windows(6).enumerate() {
            let Some(lint) = declaration(window) else {
                continue;
            };
            let lines = doc_lines_before(sequence.get(..start).unwrap_or_default());
            if let Some(section) = what_it_does(&lines) {
                found.push((lint.name, section));
            }
        }
    }
    found
}

/// The doc comment lines that end right before `tokens` end: `///` comments lex as
/// `# [doc = "..."]`, so the walk goes back over those pairs.
fn doc_lines_before(tokens: &[TokenTree]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = tokens;
    for _ in 0..tokens.len() {
        let [before @ .., hash, TokenTree::Group(attribute)] = rest else {
            break;
        };
        let Some(line) = doc_text(attribute).filter(|_| tokens::is_punct(hash, '#')) else {
            break;
        };
        lines.push(line);
        rest = before;
    }
    lines.reverse();
    assert!(lines.len() <= tokens.len() / 2, "each doc line takes two tokens");
    lines
}

/// The text of a `[doc = "..."]` attribute group.
fn doc_text(attribute: &proc_macro2::Group) -> Option<String> {
    let inner: Vec<TokenTree> = attribute.stream().into_iter().collect();
    let [TokenTree::Ident(name), equals, TokenTree::Literal(literal)] = inner.as_slice() else {
        return None;
    };
    let syn::Lit::Str(text) = syn::Lit::new(literal.clone()) else {
        return None;
    };
    (name == "doc" && tokens::is_punct(equals, '=')).then(|| text.value())
}

/// The lines after `### What it does` and before the next `###` heading, joined into one
/// paragraph. `None` without that heading or without text under it.
fn what_it_does(lines: &[String]) -> Option<String> {
    let start = lines.iter().position(|line| line.trim() == WHAT_IT_DOES)?;
    let body = lines.iter().skip(start + 1).take_while(|line| !line.trim().starts_with("###"));
    let words: Vec<&str> = body.map(|line| line.trim()).filter(|line| !line.is_empty()).collect();
    (!words.is_empty()).then(|| words.join(" "))
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
    use super::{DylintLint, declarations_in, declared, documentation_in, documented};
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

    // Goal: the `### What it does` section of a declaration's doc comment is read as one
    // paragraph, up to the next heading; other doc text and undocumented lints give nothing.
    #[test]
    fn what_it_does_is_read() {
        let expected = ("nami_recursion".to_owned(), "Reports recursion.".to_owned());
        assert_eq!(documentation_in(DECLARATION), [expected]);
        let two = "m! {\n/// Intro.\n/// ### What it does\n///\n/// Reports `a`,\n/// and `b`.\n\
                   ///\n/// More.\n/// ### Why is this bad?\n/// Reasons.\npub A, Deny, \"x\" }";
        let section = ("a".to_owned(), "Reports `a`, and `b`. More.".to_owned());
        assert_eq!(documentation_in(two), [section]);
        assert_eq!(documentation_in("m! { /// Plain docs.\n pub A, Deny, \"x\" }"), []);
        assert_eq!(documentation_in("m! { /// ### What it does\n pub A, Deny, \"x\" }"), []);
        assert_eq!(documentation_in("m! { pub A, Deny, \"x\" }"), [], "no doc comment");
        let other_attribute = "m! { /// ### What it does\n/// X.\n#[allow(y)] pub A, Deny, \"x\" }";
        assert_eq!(documentation_in(other_attribute), [], "the docs must end at the declaration");
        assert_eq!(documentation_in("fn broken( {"), [], "a file that does not lex");
        let real = documented(&crate::workspace_root()).unwrap();
        assert!(real.iter().any(|(name, text)| name == "nami_recursion" && text.starts_with("R")));
        assert_eq!(documented(&crate::workspace_root().join("no-such-dir")).unwrap(), []);
    }
}
