//! Rules about documents: the ADRs (ADR-0000).

use super::Violation;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Directory of the ADRs and their index (ADR-0000).
pub const ADR_DIRECTORY: &str = "docs/design";

/// Sections every ADR must have (ADR-0000).
const ADR_SECTIONS: [&str; 6] = [
    "## Context",
    "## Decision",
    "## Enforcement",
    "## Consequences",
    "## Alternatives considered",
    "## Evidence",
];

/// Valid beginnings of an ADR status line (ADR-0000).
const ADR_STATUSES: [&str; 4] = [
    "- Status: Proposed",
    "- Status: Accepted",
    "- Status: Superseded by ADR-",
    "- Status: Deprecated",
];

/// The entries directly inside `directory`, as paths relative to `root`, sorted.
pub fn entries_of(root: &Path, directory: &str) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(root.join(directory))?
        .map(|entry| entry.map(|entry| Path::new(directory).join(entry.file_name())))
        .collect::<io::Result<_>>()?;
    paths.sort();
    assert!(
        paths.iter().all(|path| path.starts_with(directory)),
        "entries are below the directory"
    );
    Ok(paths)
}

/// The ADR index, which every ADR must be listed in.
pub fn adr_index(root: &Path) -> io::Result<String> {
    fs::read_to_string(root.join(ADR_DIRECTORY).join("README.md"))
}

/// Checks one ADR: it is indexed, has a valid status and has every section (ADR-0000).
pub fn check_adr(relative: &Path, text: &str, index: &str) -> Vec<Violation> {
    let name = relative.file_name().map_or_default(|name| name.to_string_lossy());
    assert!(is_adr_file_name(&name), "check_adr is called on ADR files only");
    let mut violations = Vec::new();
    if !index.contains(&format!("({name})")) {
        let message = "ADR is missing from docs/design/README.md; add it to the index (ADR-0000)";
        violations.push(Violation::new(relative, 0, "adr-index", message));
    }
    let has_status =
        text.lines().any(|line| ADR_STATUSES.iter().any(|status| line.starts_with(status)));
    if !has_status {
        let message = "ADR has no valid status line; use `- Status: Proposed`, `Accepted`, \
                       `Superseded by ADR-NNNN` or `Deprecated` (ADR-0000)";
        violations.push(Violation::new(relative, 0, "adr-status", message));
    }
    for section in ADR_SECTIONS.iter().filter(|section| !text.lines().any(|line| line == **section))
    {
        let message = format!("ADR is missing the `{section}` section (ADR-0000)");
        violations.push(Violation::new(relative, 0, "adr-section", message));
    }
    violations
}

/// True for `NNNN-<title>.md`, the name of an ADR file.
pub fn is_adr_file_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let has_number = bytes.iter().take(4).filter(|byte| byte.is_ascii_digit()).count() == 4;
    let is_markdown = Path::new(name).extension().is_some_and(|extension| extension == "md");
    has_number && bytes.get(4) == Some(&b'-') && is_markdown
}

#[cfg(test)]
mod tests {
    use super::{check_adr, entries_of, is_adr_file_name};
    use std::path::Path;

    fn rules(violations: &[super::Violation]) -> Vec<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: an ADR must be indexed, have a status and have every section.
    #[test]
    fn adr_rules_fire() {
        let sections = "## Context\n## Decision\n## Enforcement\n## Consequences\n\
                        ## Alternatives considered\n## Evidence\n";
        let good = format!("# ADR-0001: x\n\n- Status: Accepted\n\n{sections}");
        let index = "| [0001](0001-x.md) |";
        let path = Path::new("docs/design/0001-x.md");
        assert_eq!(rules(&check_adr(path, &good, index)), Vec::<&str>::new());
        let bad = "# ADR-0002: y\n\n- Status: Maybe\n## Context\n";
        let violations = check_adr(Path::new("docs/design/0002-y.md"), bad, index);
        assert_eq!(rules(&violations)[..2], ["adr-index", "adr-status"]);
        assert_eq!(violations.len(), 7, "index, status and five missing sections");
    }

    // Goal: ADR files are recognised by their four-digit prefix.
    #[test]
    fn adr_file_names_are_recognised() {
        assert!(is_adr_file_name("0013-lint-table-refinements.md"));
        assert!(!is_adr_file_name("README.md"));
        assert!(!is_adr_file_name("13-x.md"));
        assert!(!is_adr_file_name("0013_x.md"), "the number must be followed by a dash");
        assert!(!is_adr_file_name("abcd-x.md"), "the prefix must be digits");
        assert!(!is_adr_file_name("0013-x.txt"), "ADRs are Markdown");
    }

    // Goal: a directory lists as sorted relative paths, and a missing directory is an error.
    #[test]
    fn directories_list_as_relative_paths() {
        let root = crate::workspace_root();
        let adrs = entries_of(&root, "docs/design").unwrap();
        assert!(adrs.is_sorted(), "paths are sorted");
        assert!(adrs.contains(&Path::new("docs/design/README.md").to_path_buf()));
        assert!(entries_of(&root, "docs/no-such-directory").is_err(), "missing directory");
    }
}
