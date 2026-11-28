//! The workspace rules about ADR IDs (ADR-20261128-1856): every file in `docs/design/` is
//! named by a valid ID (`adr-id`), and every `ADR-` reference in the repository names an ADR
//! that exists (`adr-reference`).

use super::id::{AdrId, Reference, is_slug};
use crate::tidy::{ADR_DIRECTORY, Violation};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The index, the one file in [`ADR_DIRECTORY`] that is no ADR.
const INDEX_NAME: &str = "README.md";

/// Agent configuration at the root: checked for references, except the worktrees in it, which
/// are other checkouts.
const AGENT_DIRECTORY: &str = ".claude";

/// Claude Code's worktrees, inside [`AGENT_DIRECTORY`].
const WORKTREES: &str = "worktrees";

/// The extensions of the files whose references are checked: sources, documents, configuration
/// and the expected output of the lint UI tests. A scratch file such as the `pr.diff` that CI
/// writes for cargo-mutants quotes old text, and is not the repository's.
const REFERENCE_EXTENSIONS: [&str; 9] =
    ["json", "md", "rs", "sh", "stderr", "toml", "txt", "yaml", "yml"];

/// Files without one of those extensions whose references are checked.
const REFERENCE_NAMES: [&str; 2] = ["CODEOWNERS", "justfile"];

/// The rule `adr-id`: every file in [`ADR_DIRECTORY`] but the index is named
/// `YYYYMMDD-HHMM-<slug>.md`, after a valid UTC minute, or carries one of the legacy numbers.
///
/// # Errors
///
/// When the directory cannot be read.
pub fn check_file_names(root: &Path) -> io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for path in crate::tidy::entries_of(root, ADR_DIRECTORY)? {
        let name = path.file_name().map_or_default(|name| name.to_string_lossy().into_owned());
        if name == INDEX_NAME || !root.join(&path).is_file() {
            continue;
        }
        let id = AdrId::of_file_name(&name);
        let problem = match id {
            None => "it is not `YYYYMMDD-HHMM-<slug>.md`, named after a UTC minute",
            Some(id) if !id.is_assignable() => {
                "its number is no ADR's: numbers ended at 0043, and new ADRs are named after \
                 the UTC minute they are written in"
            }
            Some(_) if !AdrId::slug_of_file_name(&name).is_some_and(is_slug) => {
                "its slug is not lower-case words joined by dashes"
            }
            Some(_) => continue,
        };
        let message = format!(
            "`{name}` is no ADR file name: {problem}; create an ADR with `cargo xtask adr new \
             <slug>`, which names it after the current UTC minute (ADR-20261128-1856)"
        );
        violations.push(Violation::new(&path, 0, "adr-id", message));
    }
    assert!(
        violations
            .iter()
            .all(|violation| violation.path.parent() == Some(Path::new(ADR_DIRECTORY))),
        "only files directly in the ADR directory are judged"
    );
    Ok(violations)
}

/// The rule `adr-reference`: every `ADR-` followed by a digit, in every text file that tidy
/// walks, in `docs/` and in the agent configuration, names an ADR in `ids`.
///
/// # Errors
///
/// When a directory or file cannot be read.
pub fn check_references(root: &Path, ids: &BTreeSet<AdrId>) -> io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for relative in reference_files(root)? {
        let Ok(text) = String::from_utf8(fs::read(root.join(&relative))?) else {
            continue;
        };
        violations.extend(references_in(&relative, &text, ids));
    }
    assert!(
        violations.iter().all(|violation| !violation.path.starts_with("xtask/fixtures")),
        "tidy's fixtures cite missing ADRs on purpose and are never judged"
    );
    Ok(violations)
}

/// The violations of `adr-reference` in one file.
#[must_use]
pub fn references_in(relative: &Path, text: &str, ids: &BTreeSet<AdrId>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line, number) in text.lines().zip(1_u32..) {
        for (offset, _) in line.match_indices("ADR-") {
            let after = line.get(offset + "ADR-".len()..).unwrap_or_default();
            let problem = match super::id::reference_after(after) {
                None => continue,
                Some(Reference::Id(id)) if ids.contains(&id) => continue,
                Some(Reference::Id(id)) => format!(
                    "`ADR-{id}` names no ADR in docs/design/; cite one that exists. For an ADR \
                     of another repository or a closed pull request, drop the `ADR-` prefix"
                ),
                Some(Reference::Malformed) => {
                    let digits: String = after
                        .chars()
                        .take_while(|character| character.is_ascii_digit() || *character == '-')
                        .collect();
                    format!(
                        "`ADR-{digits}` is no ADR ID; an ID is `YYYYMMDD-HHMM`, or `NNNN` for \
                         ADRs 0000 to 0043"
                    )
                }
            };
            let message = format!("{problem} (ADR-20261128-1856)");
            violations.push(Violation::new(relative, number, "adr-reference", message));
        }
    }
    let lines = u32::try_from(text.lines().count()).expect("a file has < 2^32 lines");
    assert!(
        violations.iter().all(|violation| (1..=lines).contains(&violation.line)),
        "every violation is on a line of the file"
    );
    violations
}

/// The files whose references are checked: what tidy's walk reads, everything below `docs/`,
/// and the agent configuration without its worktrees. Tidy's own fixtures stay out: they cite
/// ADRs that do not exist on purpose.
fn reference_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = crate::tidy::files_below(root, Path::new(""))?;
    files.extend(crate::tidy::files_below(root, Path::new("docs"))?);
    if root.join(AGENT_DIRECTORY).is_dir() {
        for path in crate::tidy::entries_of(root, AGENT_DIRECTORY)? {
            if path.ends_with(WORKTREES) {
                continue;
            }
            if root.join(&path).is_dir() {
                files.extend(crate::tidy::files_below(root, &path)?);
            } else {
                files.push(path);
            }
        }
    }
    files.retain(|file| is_reference_source(file));
    files.sort();
    files.dedup();
    assert!(files.iter().all(|file| file.is_relative()), "paths are relative to the root");
    Ok(files)
}

/// True for a file whose kind holds references: [`REFERENCE_EXTENSIONS`] or
/// [`REFERENCE_NAMES`].
fn is_reference_source(file: &Path) -> bool {
    let name = file.file_name().map_or_default(|name| name.to_string_lossy());
    let extension = file.extension().map_or_default(|extension| extension.to_string_lossy());
    REFERENCE_EXTENSIONS.contains(&extension.as_ref()) || REFERENCE_NAMES.contains(&name.as_ref())
}

#[cfg(test)]
mod tests {
    use super::{check_file_names, check_references, references_in};
    use crate::adr::id::AdrId;
    use crate::tidy::scratch::Scratch;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn rules(violations: &[crate::tidy::Violation]) -> Vec<(&'static str, u32)> {
        violations.iter().map(|violation| (violation.rule, violation.line)).collect()
    }

    // Goal: the index and well-formed names pass; a name without an ID, a number that no ADR had,
    // a bad slug and a stamp that is no minute are reported, one each.
    #[test]
    fn adr_file_names_are_checked() {
        let scratch = Scratch::new("adr-file-names");
        for name in ["README.md", "0013-lint-table.md", "20261128-1903-adr-ids.md"] {
            scratch.write(&format!("docs/design/{name}"), "");
        }
        assert_eq!(check_file_names(scratch.root()).unwrap(), Vec::new());
        let bad =
            ["notes.md", "0044-next.md", "0016-gap.md", "0013-Upper.md", "20261131-1200-x.md"];
        for name in bad {
            scratch.write(&format!("docs/design/{name}"), "");
        }
        let violations = check_file_names(scratch.root()).unwrap();
        let named: Vec<String> = violations.iter().map(|v| v.path.display().to_string()).collect();
        assert_eq!(
            named,
            ["0013-Upper.md", "0016-gap.md", "0044-next.md", "20261131-1200-x.md", "notes.md"]
                .map(|name| format!("docs/design/{name}"))
        );
        assert!(violations[0].message.contains("slug"), "{}", violations[0].message);
        assert!(violations[1].message.contains("ended at 0043"), "{}", violations[1].message);
        assert!(violations[4].message.contains("cargo xtask adr new"), "{}", violations[4]);
    }

    // Goal: a reference to an existing ADR passes, one to a missing ADR or with malformed digits
    // is reported at its line, and a placeholder is no reference.
    #[test]
    fn references_must_resolve() {
        let ids = BTreeSet::from([AdrId::Legacy(1)]);
        // Built at run time, so that this file cites no missing ADR itself.
        let (missing, malformed) = (format!("ADR-{}", "0044"), format!("ADR-{}", "123"));
        let text = format!("ADR-0001 and ADR-NNNN.\n{missing}, {malformed}.\n");
        let violations = references_in(Path::new("a.md"), &text, &ids);
        assert_eq!(rules(&violations), [("adr-reference", 2), ("adr-reference", 2)]);
        assert!(violations[0].message.contains("names no ADR"), "{}", violations[0]);
        let expected = format!("`{malformed}` is no ADR ID");
        assert!(violations[1].message.contains(&expected), "{}", violations[1]);
    }

    // Goal: references are checked in sources, documents, configuration and agent files, but not
    // in fixtures, build output, worktrees or scratch files such as CI's `pr.diff`, and a file
    // that is not UTF-8 is skipped.
    #[test]
    fn references_are_checked_across_the_tree() {
        let scratch = Scratch::new("adr-references");
        let missing = format!("See ADR-{}.\n", "0044");
        let checked = ["AGENTS.md", "docs/plan.md", "docs/design/x.md", ".claude/agents/r.md"];
        let more = [".claude/settings.json", "xtask/src/a.rs", "justfile", "lints/ui/a.stderr"];
        for file in checked.iter().chain(&more) {
            scratch.write(file, &missing);
        }
        let skipped = ["xtask/fixtures/tidy/a.fixture", "target/a.md", ".claude/worktrees/w/a.md"];
        for file in skipped.iter().chain(&["pr.diff", "notes"]) {
            scratch.write(file, &missing);
        }
        std::fs::write(scratch.root().join("docs/binary.md"), [0xff, 0xfe]).unwrap();
        let violations = check_references(scratch.root(), &BTreeSet::new()).unwrap();
        let files: BTreeSet<String> =
            violations.iter().map(|v| v.path.display().to_string()).collect();
        let expected = checked.iter().chain(&more).map(|file| (*file).to_owned());
        assert_eq!(files, expected.collect::<BTreeSet<_>>());
    }
}
