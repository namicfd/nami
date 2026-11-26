//! Rules decided on tokens, where the syntax tree does not look.
//!
//! `syn` keeps the arguments of a macro invocation and the inside of an attribute as raw tokens,
//! so a syntax rule sees neither `assert!(libm::sqrt(x) > 0.0)` nor
//! `#[cfg_attr(all(), expect(..))]`. The rules here read those tokens directly.
//!
//! Token trees nest. Every walk here uses an explicit work list with a bound and no recursion,
//! because the no-recursion lint also checks `xtask` (ADR-0006).

use proc_macro2::{Ident, Literal, TokenStream, TokenTree};

/// Upper bound on the token groups in one file, so the walk is bounded.
const TOKEN_GROUPS_MAX: u32 = 4_000_000;

/// Every token sequence in `stream`: the stream itself and the content of every group inside
/// it, at any depth. Tokens that are neighbours in the source are neighbours in one sequence.
#[must_use]
pub fn sequences(stream: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut pending = vec![stream];
    let mut found = Vec::new();
    for _ in 0..TOKEN_GROUPS_MAX {
        let Some(next) = pending.pop() else {
            assert!(!found.is_empty(), "the stream itself is a sequence");
            return found;
        };
        let sequence: Vec<TokenTree> = next.into_iter().collect();
        for tree in &sequence {
            if let TokenTree::Group(group) = tree {
                pending.push(group.stream());
            }
        }
        found.push(sequence);
    }
    panic!("more than {TOKEN_GROUPS_MAX} token groups in one stream");
}

/// What the ban and literal rules look for in the tokens of a macro invocation.
pub struct Findings<'a> {
    /// Identifiers that start a path: `libm` in `libm::sqrt` or `::libm::sqrt`, but not in
    /// `kernels::libm::sqrt`.
    pub path_starts: Vec<&'a Ident>,
    /// Names of macros that are invoked: `debug_assert` in `debug_assert!(..)`.
    pub macro_calls: Vec<&'a Ident>,
    /// Every literal.
    pub literals: Vec<&'a Literal>,
}

/// Collects the path starts, macro invocations and literals in one token sequence. Call it on
/// each of [`sequences`] to cover a whole macro body.
#[must_use]
pub fn findings(sequence: &[TokenTree]) -> Findings<'_> {
    let mut found =
        Findings { path_starts: Vec::new(), macro_calls: Vec::new(), literals: Vec::new() };
    // Whether the previous tokens were a path segment followed by `::`, so that the next
    // identifier continues that path instead of starting one.
    let mut continues_path = false;
    let mut rest = sequence;
    for _ in 0..sequence.len() {
        let [tree, after @ ..] = rest else {
            break;
        };
        rest = after;
        if let TokenTree::Literal(literal) = tree {
            found.literals.push(literal);
        }
        if let TokenTree::Ident(ident) = tree {
            if !continues_path && starts_with_path_separator(after) {
                found.path_starts.push(ident);
            }
            if let [bang, TokenTree::Group(_), ..] = after
                && is_punct(bang, '!')
            {
                found.macro_calls.push(ident);
            }
        }
        continues_path = ends_path_segment(tree) && starts_with_path_separator(after)
            || continues_path && is_punct(tree, ':');
    }
    found
}

/// True if a path can continue after `tree`: an identifier, or the `>` that closes generic
/// arguments.
fn ends_path_segment(tree: &TokenTree) -> bool {
    matches!(tree, TokenTree::Ident(_)) || is_punct(tree, '>')
}

fn starts_with_path_separator(trees: &[TokenTree]) -> bool {
    matches!(trees, [first, second, ..] if is_punct(first, ':') && is_punct(second, ':'))
}

/// True if `tree` is the punctuation `character`.
pub fn is_punct(tree: &TokenTree, character: char) -> bool {
    matches!(tree, TokenTree::Punct(punct) if punct.as_char() == character)
}

#[cfg(test)]
mod tests {
    use super::{findings, sequences};
    use proc_macro2::TokenStream;

    fn names(idents: &[&proc_macro2::Ident]) -> Vec<String> {
        idents.iter().map(ToString::to_string).collect()
    }

    /// The path starts and the macro calls in `text`, at any depth, each list sorted.
    fn found(text: &str) -> (Vec<String>, Vec<String>) {
        let stream: TokenStream = text.parse().unwrap();
        let mut path_starts = Vec::new();
        let mut macro_calls = Vec::new();
        for sequence in sequences(stream) {
            let findings = findings(&sequence);
            path_starts.extend(names(&findings.path_starts));
            macro_calls.extend(names(&findings.macro_calls));
        }
        path_starts.sort();
        macro_calls.sort();
        (path_starts, macro_calls)
    }

    // Goal: every group at any depth is one sequence, and an empty stream is one empty sequence.
    #[test]
    fn sequences_cover_every_group() {
        let stream: TokenStream = "a (b [c] {d}) e".parse().unwrap();
        let mut lengths: Vec<usize> = sequences(stream).iter().map(Vec::len).collect();
        lengths.sort_unstable();
        assert_eq!(lengths, [1, 1, 3, 3], "the two inner groups, the outer group and the stream");
        assert_eq!(sequences(TokenStream::new()).len(), 1);
    }

    // Goal: an identifier starts a path when `::` follows it and no path segment precedes it.
    #[test]
    fn path_starts_are_found() {
        assert_eq!(found("libm::sqrt(x)").0, ["libm"]);
        assert_eq!(found("::libm::sqrt(x)").0, ["libm"], "a leading `::` still starts at libm");
        assert_eq!(found("1.0 + ::libm::sqrt(x)").0, ["libm"], "an operator is no path segment");
        assert_eq!(found("f(y, ::libm::sqrt(x))").0, ["libm"], "nor is a comma");
        assert_eq!(found("a + libm::sqrt(x)").0, ["libm"]);
        assert_eq!(found("f(1.0, (libm::sqrt(x)))").0, ["libm"], "inside nested groups");
        assert_eq!(found("kernels::libm::sqrt(x)").0, ["kernels"], "libm continues a path");
        assert_eq!(found("Vec::<f64>::new()").0, ["Vec"], "`>::` continues a path");
        assert_eq!(found("libm").0, Vec::<String>::new(), "a lone identifier is no path");
        assert_eq!(found("x: libm").0, Vec::<String>::new(), "a single colon is no path");
    }

    // Goal: a macro invocation is an identifier, `!` and a group; `!=` is not one.
    #[test]
    fn macro_calls_are_found() {
        assert_eq!(found("debug_assert!(x > 0.0, \"positive\")").1, ["debug_assert"]);
        assert_eq!(found("core::debug_assert!(x)").1, ["debug_assert"]);
        assert_eq!(found("m! { a![1]; b!(2) }").1, ["a", "b", "m"]);
        assert_eq!(found("debug_assert != x").1, Vec::<String>::new());
        assert_eq!(found("!debug_assert").1, Vec::<String>::new());
    }

    // Goal: literals of every kind are collected, at the depth of the sequence only.
    #[test]
    fn literals_are_found() {
        let stream: TokenStream = "x < 1.0e-12 && y == \"s\" && f(3)".parse().unwrap();
        let top: Vec<Vec<String>> = sequences(stream)
            .iter()
            .map(|sequence| findings(sequence).literals.iter().map(ToString::to_string).collect())
            .collect();
        assert_eq!(top, [vec!["1.0e-12".to_owned(), "\"s\"".to_owned()], vec!["3".to_owned()]]);
    }
}
