//! Rules about documents: the ADRs (ADR-0000) and the learnings.

use super::Violation;
use std::fs;
use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// Directory of the ADRs and their index (ADR-0000).
pub const ADR_DIRECTORY: &str = "docs/design";

/// Directory of the learnings: what went wrong in a session, and the guardrail it suggests.
pub const LEARNING_DIRECTORY: &str = "docs/learnings";

/// The one file in the learnings directory that is not a learning. It describes the format.
pub const LEARNING_README: &str = "README.md";

/// Valid beginnings of a learning's status line.
const LEARNING_STATUSES: [&str; 3] =
    ["- Status: Open", "- Status: Codified in ", "- Status: Rejected"];

/// Sections every learning must have.
const LEARNING_SECTIONS: [&str; 2] = ["## What happened", "## Proposed guardrail"];

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

/// The names of the ADR files in the tree, sorted: the context of the rule `adr-number`.
pub fn adr_file_names(root: &Path) -> io::Result<Vec<String>> {
    let names: Vec<String> = entries_of(root, ADR_DIRECTORY)?
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

/// True for `NNNN-<title>.md`, the name of an ADR file.
pub fn is_adr_file_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let has_number = bytes.iter().take(4).filter(|byte| byte.is_ascii_digit()).count() == 4;
    let is_markdown = Path::new(name).extension().is_some_and(|extension| extension == "md");
    has_number && bytes.get(4) == Some(&b'-') && is_markdown
}

/// Checks one learning: its name is dated, and it has a status and both sections.
pub fn check_learning(relative: &Path, text: &str) -> Vec<Violation> {
    assert!(relative.starts_with(LEARNING_DIRECTORY), "check_learning is called on learnings only");
    let name = relative.file_name().map_or_default(|name| name.to_string_lossy());
    let mut violations = Vec::new();
    let mut report = |message: String| {
        violations.push(Violation::new(relative, 0, "learning-format", message));
    };
    if !is_learning_file_name(&name) {
        report(format!(
            "learning `{name}` must be named `YYYY-MM-DD-<lowercase-slug>.md`, after the day it \
             happened; rename it ({LEARNING_DIRECTORY}/{LEARNING_README})"
        ));
    }
    let has_status =
        text.lines().any(|line| LEARNING_STATUSES.iter().any(|status| line.starts_with(status)));
    if !has_status {
        report(format!(
            "learning has no valid status line; use `- Status: Open`, `- Status: Codified in \
             <rule, lint or ADR>` or `- Status: Rejected` ({LEARNING_DIRECTORY}/{LEARNING_README})"
        ));
    }
    let is_missing = |section: &&&str| !text.lines().any(|line| line == **section);
    for section in LEARNING_SECTIONS.iter().filter(is_missing) {
        report(format!(
            "learning is missing the `{section}` section; add it \
             ({LEARNING_DIRECTORY}/{LEARNING_README})"
        ));
    }
    violations
}

/// True for `YYYY-MM-DD-<slug>.md`, where the slug is lowercase words and numbers joined by
/// single dashes.
fn is_learning_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let mut parts = stem.splitn(4, '-');
    let (Some(year), Some(month), Some(day), Some(slug)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let is_number = |part: &str, digits: usize, values: RangeInclusive<u32>| {
        let is_digits = part.len() == digits && part.bytes().all(|byte| byte.is_ascii_digit());
        is_digits && part.parse::<u32>().is_ok_and(|value| values.contains(&value))
    };
    let is_word = |word: &str| {
        !word.is_empty()
            && word.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    let is_dated =
        is_number(year, 4, 1..=9999) && is_number(month, 2, 1..=12) && is_number(day, 2, 1..=31);
    is_dated && slug.split('-').all(is_word)
}

#[cfg(test)]
mod tests {
    use super::{adr_file_names, check_adr, check_adr_number, check_learning, entries_of};
    use super::{is_adr_file_name, is_learning_file_name};
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

    // Goal: a learning needs a dated name, a status line and both sections; each missing part
    // is one violation.
    #[test]
    fn learning_rule_fires() {
        let good = "# A title\n\n- Status: Open\n\n## What happened\n\n## Proposed guardrail\n";
        let path = Path::new("docs/learnings/2026-11-25-tidy-missed-libm.md");
        assert_eq!(check_learning(path, good), Vec::new());
        let codified = good.replace("- Status: Open", "- Status: Codified in tidy `direct-libm`");
        assert_eq!(check_learning(path, &codified), Vec::new());
        let rejected = good.replace("- Status: Open", "- Status: Rejected");
        assert_eq!(check_learning(path, &rejected), Vec::new());
        let no_status = good.replace("- Status: Open", "- Status: Done");
        assert_eq!(rules(&check_learning(path, &no_status)), ["learning-format"]);
        let indented = good.replace("- Status: Open", "  - Status: Open");
        assert_eq!(check_learning(path, &indented).len(), 1, "the status starts its line");
        let no_guardrail = good.replace("## Proposed guardrail", "## Guardrail");
        assert_eq!(check_learning(path, &no_guardrail).len(), 1);
        let badly_named = Path::new("docs/learnings/tidy-missed-libm.md");
        assert_eq!(rules(&check_learning(badly_named, good)), ["learning-format"]);
        assert_eq!(check_learning(badly_named, "").len(), 4, "name, status and two sections");
    }

    // Goal: a learning is named by its date and a lowercase slug, and nothing else.
    #[test]
    fn learning_file_names_are_recognised() {
        assert!(is_learning_file_name("2026-11-25-tidy-missed-libm.md"));
        assert!(is_learning_file_name("2026-01-31-run2.md"));
        assert!(!is_learning_file_name("2026-11-25.md"), "the slug is required");
        assert!(!is_learning_file_name("2026-11-25-.md"), "the slug is not empty");
        assert!(!is_learning_file_name("2026-11-25-Tidy.md"), "the slug is lowercase");
        assert!(!is_learning_file_name("2026-11-25-tidy_missed.md"), "words join by dashes");
        assert!(!is_learning_file_name("2026-11-25-tidy--missed.md"), "single dashes");
        assert!(!is_learning_file_name("2026-13-25-tidy.md"), "there is no month 13");
        assert!(!is_learning_file_name("2026-00-25-tidy.md"), "there is no month 0");
        assert!(!is_learning_file_name("2026-11-32-tidy.md"), "there is no day 32");
        assert!(!is_learning_file_name("2026-11-00-tidy.md"), "there is no day 0");
        assert!(!is_learning_file_name("26-11-25-tidy.md"), "the year has four digits");
        assert!(!is_learning_file_name("2026-1-25-tidy.md"), "the month has two digits");
        assert!(!is_learning_file_name("2026-11-25-tidy.txt"), "learnings are Markdown");
        assert!(!is_learning_file_name("notes.md"), "the date is required");
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
