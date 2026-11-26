//! Mutant coverage per changed hunk of a rule-path Rust file (review of #57, round 3).
//!
//! A count of mutants per workspace proves nothing about a hunk that has none: a changed
//! literal, a deleted line or a moved statement makes no mutant, and another function's mutant
//! does not test it. So every changed hunk must contain a mutant, one whose span starts on one
//! of its lines, or be one that cannot change what a rule reports: a hunk of comments and blank
//! lines only, a hunk inside the test module, or a hunk of dispatch calls, which the new
//! rule's case shows to run ([`super::class::dispatch`]). The registry's tables are read as
//! data, so their hunks need no mutant. Any other hunk, a pure deletion among them, is unproven.
//!
//! A mutant on a line says only that the head's tests pin the head's behaviour there: a test
//! edited together with the code it tests pins the edit. So for an edit to existing rule code
//! ([`super::class`]), the reviewer app's approval stands in for a hunk without a mutant, and
//! the per-hunk rule is what an additive diff's new code has to meet.

use super::dialect;
use std::path::Path;
use syn::spanned::Spanned;

/// One hunk of a zero-context diff: the head lines it adds and the base lines it removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The first head line it adds, or the head line after which it deletes.
    pub head_first: u32,
    /// The added lines.
    pub added: Vec<String>,
    /// The first base line it removes.
    pub base_first: u32,
    /// The removed lines.
    pub removed: Vec<String>,
}

impl Hunk {
    /// The head lines it adds, as an inclusive range; `None` for a pure deletion.
    fn head_lines(&self) -> Option<(u32, u32)> {
        let count = u32::try_from(self.added.len()).ok().filter(|count| *count > 0)?;
        Some((self.head_first, self.head_first + count - 1))
    }

    /// The base lines it removes, as an inclusive range; `None` for a pure addition.
    fn base_lines(&self) -> Option<(u32, u32)> {
        let count = u32::try_from(self.removed.len()).ok().filter(|count| *count > 0)?;
        Some((self.base_first, self.base_first + count - 1))
    }
}

/// Reads the hunks of a zero-context unified diff.
#[must_use]
pub fn parse_hunks(diff: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    for line in diff.lines() {
        if let Some(header) = line.strip_prefix("@@ ") {
            let mut ranges = header.split_whitespace();
            let start = |range: Option<&str>| -> u32 {
                range
                    .and_then(|range| range.get(1..))
                    .and_then(|range| range.split(',').next())
                    .and_then(|first| first.parse().ok())
                    .unwrap_or(0)
            };
            let base_first = start(ranges.next());
            let head_first = start(ranges.next());
            hunks.push(Hunk { head_first, added: Vec::new(), base_first, removed: Vec::new() });
        } else if let Some(hunk) = hunks.last_mut() {
            if let Some(added) = line.strip_prefix('+') {
                hunk.added.push(added.to_owned());
            } else if let Some(removed) = line.strip_prefix('-') {
                hunk.removed.push(removed.to_owned());
            }
        }
    }
    hunks
}

/// The hunks between the base's and the head's copy of `relative`, from `git diff --no-index`
/// with no context, run in `work`, outside both trees, with no external diff or text
/// conversion. A file missing on one side is diffed against `/dev/null`.
///
/// # Errors
///
/// Returns what failed if git cannot run or fails.
pub fn hunks(base: &Path, head: &Path, relative: &str, work: &Path) -> Result<Vec<Hunk>, String> {
    let side = |root: &Path| {
        let path = root.join(relative);
        if path.is_file() { path } else { Path::new("/dev/null").to_path_buf() }
    };
    std::fs::create_dir_all(work)
        .map_err(|error| format!("cannot create {}: {error}", work.display()))?;
    let mut command = super::command("git", work);
    let _command = command
        .args(["diff", "--no-index", "--no-color", "--no-ext-diff", "--no-textconv", "-U0", "--"])
        .arg(side(base))
        .arg(side(head));
    let output = super::output(&mut command, "`git diff --no-index`")?;
    match output.status.code() {
        Some(0 | 1) => Ok(parse_hunks(&String::from_utf8_lossy(&output.stdout))),
        status => Err(format!(
            "`git diff --no-index` of {relative} exited with {status:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// The line ranges of `text` where a change needs no mutant: the test module.
///
/// # Errors
///
/// No verdict: the file does not parse.
pub fn exempt_ranges(relative: &str, text: &str) -> Result<Vec<(u32, u32)>, String> {
    let file =
        syn::parse_file(text).map_err(|error| format!("{relative} does not parse: {error}"))?;
    let line = |position: usize| u32::try_from(position).unwrap_or(u32::MAX);
    let ranges: Vec<(u32, u32)> = file
        .items
        .iter()
        .filter(|item| matches!(item, syn::Item::Mod(module) if dialect::is_test_module(module)))
        .map(|item| (line(item.span().start().line), line(item.span().end().line)))
        .collect();
    assert!(ranges.len() <= file.items.len(), "one range per test module");
    Ok(ranges)
}

/// True for a line that a comment or nothing makes up.
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with("//")
}

/// True if `range` lies inside one of `ranges`; true for no range at all.
fn inside(range: Option<(u32, u32)>, ranges: &[(u32, u32)]) -> bool {
    range.is_none_or(|(first, last)| ranges.iter().any(|(from, to)| *from <= first && last <= *to))
}

/// True if a hunk needs no mutant: comments only, or inside the exempt ranges of both sides.
#[must_use]
pub fn is_exempt(hunk: &Hunk, head_ranges: &[(u32, u32)], base_ranges: &[(u32, u32)]) -> bool {
    let comments = hunk.added.iter().chain(&hunk.removed).all(|line| is_comment(line));
    comments || (inside(hunk.head_lines(), head_ranges) && inside(hunk.base_lines(), base_ranges))
}

/// True if a mutant starts on one of the hunk's added lines; a pure deletion contains none.
#[must_use]
pub fn contains_mutant(hunk: &Hunk, starts: &[u32]) -> bool {
    hunk.head_lines()
        .is_some_and(|(first, last)| starts.iter().any(|line| first <= *line && *line <= last))
}

#[cfg(test)]
mod tests {
    use super::{Hunk, contains_mutant, exempt_ranges, is_exempt, parse_hunks};

    fn hunk(head_first: u32, added: &[&str], base_first: u32, removed: &[&str]) -> Hunk {
        let lines = |lines: &[&str]| lines.iter().map(|line| (*line).to_owned()).collect();
        Hunk { head_first, added: lines(added), base_first, removed: lines(removed) }
    }

    // Goal: a zero-context diff is read into hunks with their first lines and their added and
    // removed lines, pure deletions and additions included.
    #[test]
    fn hunks_are_read() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -3 +3 @@ fn a() {\n-old\n+new\n\
                    @@ -7,2 +6,0 @@\n-gone\n-gone too\n@@ -9,0 +9,2 @@\n+one\n+two\n";
        assert_eq!(
            parse_hunks(diff),
            [
                hunk(3, &["new"], 3, &["old"]),
                hunk(6, &[], 7, &["gone", "gone too"]),
                hunk(9, &["one", "two"], 9, &[])
            ]
        );
    }

    // Goal: a hunk contains a mutant only if one starts on an added line; a pure deletion
    // contains none; comments, and changes inside the test module on both sides, need none. A
    // constant is no exemption: a changed constant is an edit.
    #[test]
    fn coverage_is_per_hunk() {
        let changed = hunk(5, &["a", "b"], 5, &["c"]);
        assert!(contains_mutant(&changed, &[6]) && !contains_mutant(&changed, &[4, 7]));
        assert!(!contains_mutant(&hunk(5, &[], 5, &["c"]), &[5]), "a pure deletion");
        assert!(is_exempt(&hunk(5, &["// a", ""], 5, &["  // b"]), &[], &[]));
        assert!(is_exempt(&changed, &[(1, 10)], &[(4, 6)]));
        assert!(!is_exempt(&changed, &[(1, 10)], &[(6, 9)]), "the base side is outside");
        assert!(!is_exempt(&changed, &[(6, 10)], &[(1, 9)]), "the head side is outside");
        let deletion = hunk(5, &[], 5, &["a", "b"]);
        assert!(is_exempt(&deletion, &[], &[(4, 6)]) && !is_exempt(&deletion, &[], &[(4, 5)]));
        let text = "const A: u8 = 1;\nfn f() {\n    const B: u8 = 2;\n}\n\
                    #[cfg(test)]\nmod tests {\n    fn t() {}\n}\n\
                    static C: u8 = 3;\nstruct S;\nimpl S {\n    const D: u8 = 4;\n}\n";
        let expected = vec![(5, 8)];
        assert_eq!(exempt_ranges("m.rs", text), Ok(expected));
    }
}
