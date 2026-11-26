//! Rules about documents: the ADRs (ADR-0000).

use super::Violation;
use std::fs;
use std::io;
use std::path::Path;

// Which files are ADRs decides which files the ADR rules check, so it is core (ADR-0022, pending).
pub use super::{ADR_DIRECTORY, is_adr_file_name};

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

/// The names of the ADR files in the tree, sorted: the context of the rule `adr-number`.
pub fn adr_file_names(root: &Path) -> io::Result<Vec<String>> {
    let names: Vec<String> = super::entries_of(root, ADR_DIRECTORY)?
        .iter()
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .filter(|name| is_adr_file_name(name))
        .collect();
    assert!(names.is_sorted(), "entries are sorted, so the names are too");
    Ok(names)
}

/// Checks that an ADR's number is its own (ADR-0000): no other ADR file starts with the same
/// four digits, and the title repeats the number.
///
/// Two branches that each add "the next" ADR merge without a conflict, because the file names
/// differ. Only this rule sees the clash, in the merge queue at the latest. The title check
/// catches an ADR that was renumbered by renaming its file alone.
pub fn check_adr_number(relative: &Path, text: &str, adr_names: &[String]) -> Vec<Violation> {
    let name = relative.file_name().map_or_default(|name| name.to_string_lossy());
    assert!(is_adr_file_name(&name), "check_adr_number is called on ADR files only");
    let number = name.get(..4).expect("an ADR file name starts with four digits");
    let mut violations = Vec::new();
    for other in adr_names.iter().filter(|other| other.starts_with(number) && **other != name) {
        let message = format!(
            "ADR number {number} is also used by `{other}`; give the newer ADR the next free \
             number: rename its file, title, index row and references (ADR-0000)"
        );
        violations.push(Violation::new(relative, 0, "adr-number", message));
    }
    let title = format!("# ADR-{number}: ");
    let heading = text.lines().find(|line| line.starts_with("# "));
    if !heading.is_some_and(|line| line.starts_with(&title)) {
        let message = format!(
            "ADR title must be `{title}<title>`, with the number of the file name; fix the \
             title, or the file name if the title is right (ADR-0000)"
        );
        violations.push(Violation::new(relative, 0, "adr-number", message));
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::{adr_file_names, check_adr, check_adr_number};
    use crate::tidy::entries_of;
    use std::path::Path;

    fn rules(violations: &[super::Violation]) -> Vec<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: two ADR files with the same number are both reported, each naming the other, because
    // git merges them without a conflict. A number that is used once passes.
    #[test]
    fn adr_numbers_are_unique() {
        let names = ["0001-first.md", "0002-clash.md", "0002-second.md"].map(str::to_owned);
        let first = Path::new("docs/design/0001-first.md");
        assert_eq!(check_adr_number(first, "# ADR-0001: A title\n", &names), Vec::new());
        let text = "# ADR-0002: A title\n";
        let clash = check_adr_number(Path::new("docs/design/0002-clash.md"), text, &names);
        assert_eq!(rules(&clash), ["adr-number"]);
        assert!(clash[0].to_string().contains("`0002-second.md`"), "the other file is named");
        let second = check_adr_number(Path::new("docs/design/0002-second.md"), text, &names);
        assert_eq!(rules(&second), ["adr-number"]);
        assert!(second[0].to_string().contains("`0002-clash.md`"), "both files are reported");
    }

    // Goal: the title repeats the number of the file name, so that an ADR cannot be renumbered
    // by renaming its file alone. A missing or malformed title is reported too.
    #[test]
    fn adr_title_repeats_the_file_number() {
        let names = ["0003-renamed.md".to_owned()];
        let path = Path::new("docs/design/0003-renamed.md");
        let fired = |text: &str| rules(&check_adr_number(path, text, &names));
        assert_eq!(fired("# ADR-0003: A title\n"), Vec::<&str>::new());
        assert_eq!(fired("<!-- a comment -->\n# ADR-0003: A title\n"), Vec::<&str>::new());
        assert_eq!(fired("# ADR-0002: The old number\n"), ["adr-number"]);
        assert_eq!(fired("# ADR-0003 without a colon\n"), ["adr-number"]);
        assert_eq!(fired("## ADR-0003: Not the title\n"), ["adr-number"]);
        assert_eq!(fired("- Status: Accepted\n"), ["adr-number"]);
    }

    // Goal: the ADR names come from the tree, sorted, without the index.
    #[test]
    fn adr_file_names_are_listed() {
        let names = adr_file_names(&crate::workspace_root()).unwrap();
        assert!(names.contains(&"0000-architecture-decision-records.md".to_owned()));
        assert!(!names.contains(&"README.md".to_owned()), "the index is not an ADR");
        assert!(names.is_sorted(), "the names are sorted");
        let own = check_adr_number(
            Path::new("docs/design/0000-architecture-decision-records.md"),
            "# ADR-0000: Record decisions as ADRs\n",
            &names,
        );
        assert_eq!(own, Vec::new(), "no ADR in this repository shares number 0000");
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
