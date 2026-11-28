//! `docs/design/README.md`: the index of the ADRs, generated from their files (ADR-20261128-1856).
//!
//! Each row comes from one ADR: its ID and file name, the title after `# ADR-<id>: `, its
//! `- Status:` line, its `- Summary:` line, and, from every other ADR's `- Amends` line, the
//! ADRs that amend it. Two pull requests that add an ADR each then change no common line, and
//! an amendment needs no edit of the ADR it amends. The rule `adr-index-doc` fails when the
//! committed file differs from what [`render`] generates, so it cannot drift.

use super::id::{AdrId, Reference};
use crate::tidy::{ADR_DIRECTORY, Violation};
use std::fs;
use std::io;
use std::path::Path;

/// The index, relative to the root.
pub const INDEX: &str = "docs/design/README.md";

/// The command that regenerates the index.
pub const WRITE_COMMAND: &str = "cargo xtask adr index --write";

/// What the index shows of one ADR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The ID that the file name gives.
    pub id: AdrId,
    /// The file name, inside [`ADR_DIRECTORY`].
    pub file: String,
    /// The title after `# ADR-<id>: `, or the whole first heading if it has another form.
    pub title: String,
    /// The text of the `- Status:` line, or empty.
    pub status: String,
    /// The text of the `- Summary:` line, or `None`.
    pub summary: Option<String>,
    /// The ADRs that the `- Amends` lines name.
    pub amends: Vec<AdrId>,
}

/// The ADRs in the tree at `root`, sorted by ID: every file in [`ADR_DIRECTORY`] whose name
/// gives an ID.
///
/// # Errors
///
/// When the directory or a file in it cannot be read.
pub fn entries(root: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for path in crate::tidy::entries_of(root, ADR_DIRECTORY)? {
        let name = path.file_name().map_or_default(|name| name.to_string_lossy().into_owned());
        if let Some(id) = AdrId::of_file_name(&name) {
            entries.push(entry_of(id, &name, &fs::read_to_string(root.join(&path))?));
        }
    }
    entries.sort_by(|a, b| (a.id, &a.file).cmp(&(b.id, &b.file)));
    assert!(entries.is_sorted_by(|a, b| a.id <= b.id), "the entries are sorted by ID");
    Ok(entries)
}

/// The entry of the ADR `id` in the file `file` with the text `text`.
#[must_use]
pub fn entry_of(id: AdrId, file: &str, text: &str) -> Entry {
    let heading = text.lines().find_map(|line| line.strip_prefix("# ")).unwrap_or_default();
    let prefix = format!("ADR-{id}: ");
    let title = heading.strip_prefix(&prefix).unwrap_or(heading).trim().to_owned();
    let items = header_items(text);
    let value_of =
        |key: &str| items.iter().find_map(|candidate| candidate.strip_prefix(key)).map(str::trim);
    let amends: Vec<AdrId> = items
        .iter()
        .filter(|item| item.starts_with("Amends"))
        .flat_map(|item| references(&outside_parentheses(item)))
        .filter_map(|reference| match reference {
            Reference::Id(amended) => Some(amended),
            Reference::Malformed => None,
        })
        .collect();
    let entry = Entry {
        id,
        file: file.to_owned(),
        title,
        status: value_of("Status:").unwrap_or_default().to_owned(),
        summary: value_of("Summary:").map(str::to_owned),
        amends,
    };
    assert!(entry.summary.as_ref().is_none_or(|summary| !summary.contains('\n')), "one line");
    entry
}

/// The items of an ADR's header: each top-level `- ` line between the title and the first
/// `## ` heading, without the dash, joined with its indented continuation lines.
#[must_use]
pub fn header_items(text: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut is_open = false;
    let body = text.lines().skip_while(|line| !line.starts_with("# ")).skip(1);
    for line in body.take_while(|line| !line.starts_with("## ")) {
        if let Some(item) = line.strip_prefix("- ") {
            items.push(item.trim().to_owned());
            is_open = true;
        } else if is_open && line.starts_with(char::is_whitespace) && !line.trim().is_empty() {
            let last = items.last_mut().expect("an open item exists");
            last.push(' ');
            last.push_str(line.trim());
        } else {
            is_open = false;
        }
    }
    assert!(items.iter().all(|item| !item.contains('\n')), "an item is one line");
    items
}

/// `text` without what stands in parentheses, at any depth. In an `Amends` line, a
/// parenthesis says what is amended, and an ADR cited there is context, not amended.
#[must_use]
pub fn outside_parentheses(text: &str) -> String {
    let mut depth: u32 = 0;
    let mut kept = String::new();
    for character in text.chars() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => kept.push(character),
            _ => {}
        }
    }
    assert!(kept.len() <= text.len(), "only removes");
    kept
}

/// Every `ADR-` reference in `text`, in order.
#[must_use]
pub fn references(text: &str) -> Vec<Reference> {
    let found: Vec<Reference> =
        text.split("ADR-").skip(1).filter_map(super::id::reference_after).collect();
    assert!(found.len() <= text.matches("ADR-").count(), "at most one reference a prefix");
    found
}

/// Renders the index of `entries`.
#[must_use]
pub fn render(entries: &[Entry]) -> String {
    let mut lines = header();
    lines.push(row(&["ADR", "Title", "Status", "Amended by", "Summary"]));
    lines.push(row(&["---"; 5]));
    for entry in entries {
        let amenders: Vec<String> = entries
            .iter()
            .filter(|other| other.id != entry.id && other.amends.contains(&entry.id))
            .map(|other| {
                let link = format!("[{}]({})", other.id, other.file);
                match other.status.split_whitespace().next() {
                    Some(word) if word != "Accepted" => format!("{link} ({})", word.to_lowercase()),
                    _ => link,
                }
            })
            .collect();
        let link = format!("[{}]({})", entry.id, entry.file);
        let summary = entry.summary.as_deref().unwrap_or_default();
        lines.push(row(&[&link, &entry.title, &entry.status, &amenders.join(", "), summary]));
    }
    lines.push(String::new());
    lines.push(
        "Research behind these decisions: [the report]\
         (../research/ai-agent-swarm-repo-readiness.md) and [its source notes]\
         (../research/ai-agent-swarm-repo-readiness-notes/)."
            .to_owned(),
    );
    assert!(lines.iter().all(|line| !line.contains('\n')), "every element is one line");
    lines.join("\n") + "\n"
}

fn header() -> Vec<String> {
    vec![
        "# Architecture decision records".to_owned(),
        String::new(),
        format!(
            "<!-- Generated by `{WRITE_COMMAND}` from the ADR files in this directory. Do not \
             edit: the tidy rule `adr-index-doc` fails when this file is stale. -->"
        ),
        String::new(),
        "The rules for how nami is written, tested, merged and paid for, and why. The process is \
         defined in ADR-0000, amended by ADR-20261128-1856. Accepted ADRs are not edited; a new \
         ADR supersedes them. The exceptions are the status line, links, and a mechanical change \
         that is applied to every ADR and recorded in the ADR that makes it (ADR-0015, \
         ADR-20261128-1856). Only a human can accept an ADR. Agents may propose them."
            .to_owned(),
        String::new(),
        "**A new ADR** starts with `cargo xtask adr new <slug>`. Its ID is the UTC minute it was \
         started, `ADR-YYYYMMDD-HHMM`, and its file is `YYYYMMDD-HHMM-<slug>.md`. ADRs 0000 \
         to 0043 keep their numbers. Each row below comes from one ADR: its title, its \
         `- Status:` line and its `- Summary:` line. An ADR that changes another says so in a \
         `- Amends: ADR-…` line, which fills the other's Amended by column. After changing any \
         of these, run `cargo xtask adr index --write`."
            .to_owned(),
        String::new(),
    ]
}

fn row(cells: &[&str]) -> String {
    let cells: Vec<String> = cells.iter().map(|cell| cell.replace('|', "\\|")).collect();
    format!("| {} |", cells.join(" | "))
}

/// The rule `adr-index-doc`: the committed index is what [`render`] generates from the ADRs.
///
/// # Errors
///
/// When the ADRs cannot be read.
pub fn check(root: &Path) -> io::Result<Vec<Violation>> {
    let expected = render(&entries(root)?);
    let path = Path::new(INDEX);
    let Ok(actual) = fs::read_to_string(root.join(path)) else {
        let message = format!(
            "{INDEX} is missing; run `{WRITE_COMMAND}` and commit the result (ADR-20261128-1856)"
        );
        return Ok(vec![Violation::new(path, 0, "adr-index-doc", message)]);
    };
    if actual == expected {
        return Ok(Vec::new());
    }
    let same_lines = actual.lines().zip(expected.lines()).take_while(|(a, b)| a == b).count();
    let line = u32::try_from(same_lines + 1).expect("documents have < 2^32 lines");
    let message = format!(
        "{INDEX} is stale: it differs from the ADRs from this line on. Do not edit it by hand; \
         run `{WRITE_COMMAND}` and commit the result (ADR-20261128-1856)"
    );
    Ok(vec![Violation::new(path, line, "adr-index-doc", message)])
}

/// The rule `adr-summary`: every ADR has a `- Summary:` line, which is its row's last column.
#[must_use]
pub fn check_summaries(entries: &[Entry]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for entry in entries.iter().filter(|entry| entry.summary.as_ref().is_none_or(String::is_empty))
    {
        let path = Path::new(ADR_DIRECTORY).join(&entry.file);
        let message = "ADR has no `- Summary:` line; add one at the end of its header, with the \
                       one line the index shows, then run `cargo xtask adr index --write` \
                       (ADR-20261128-1856)";
        violations.push(Violation::new(&path, 0, "adr-summary", message));
    }
    assert!(violations.len() <= entries.len(), "at most one violation an ADR");
    violations
}

#[cfg(test)]
mod tests {
    use super::{Entry, entry_of, header_items, references, render};
    use crate::adr::id::{AdrId, Reference};

    const TEXT: &str = "<!-- a comment -->\n# ADR-0007: A title\n\n- Status: Accepted\n\
                        - Date: 2026-11-24\n- Amends, once accepted: ADR-0003 (point 2 of \
                        ADR-0002 (twice)) and\n  ADR-0005\n- Summary: One line\n  that \
                        wraps.\n\nA paragraph.\n\n\
                        ## Context\n\n- Amends: ADR-0001\n";

    // Goal: the header items are the top-level dashes between the title and the first section,
    // with their continuation lines; a list in a section is no header.
    #[test]
    fn header_items_join_their_continuation_lines() {
        assert_eq!(
            header_items(TEXT),
            [
                "Status: Accepted",
                "Date: 2026-11-24",
                "Amends, once accepted: ADR-0003 (point 2 of ADR-0002 (twice)) and ADR-0005",
                "Summary: One line that wraps.",
            ]
        );
        assert_eq!(header_items("- Status: Accepted\n"), Vec::<String>::new(), "no title");
    }

    // Goal: an entry has the title without its ID, the status, the summary and every ADR that
    // its `Amends` lines name outside parentheses, and nothing from the sections.
    #[test]
    fn entries_come_from_the_header() {
        let entry = entry_of(AdrId::Legacy(7), "0007-a.md", TEXT);
        assert_eq!(entry.title, "A title");
        assert_eq!(entry.status, "Accepted");
        assert_eq!(entry.summary.as_deref(), Some("One line that wraps."));
        assert_eq!(entry.amends, [AdrId::Legacy(3), AdrId::Legacy(5)]);
        let bare = entry_of(AdrId::Legacy(8), "0008-b.md", "# Another title\n");
        assert_eq!((bare.title.as_str(), bare.status.as_str()), ("Another title", ""));
        assert_eq!(bare.summary, None);
    }

    // Goal: references are read after each prefix, and placeholders are skipped.
    #[test]
    fn references_are_found_in_text() {
        // Built at run time, so that this file cites no malformed ID itself.
        let found = references(&format!("ADR-0001, ADR-NNNN and ADR-{} (ADR-0002)", "123"));
        let expected = [AdrId::Legacy(1)].map(Reference::Id).into_iter();
        let expected: Vec<Reference> =
            expected.chain([Reference::Malformed, Reference::Id(AdrId::Legacy(2))]).collect();
        assert_eq!(found, expected);
        assert_eq!(references("no reference"), Vec::new());
    }

    fn entry(number: u32, status: &str, amends: &[u32]) -> Entry {
        Entry {
            id: AdrId::Legacy(number),
            file: format!("{number:04}-x.md"),
            title: format!("Title {number}"),
            status: status.to_owned(),
            summary: Some(format!("Rule {number} | piped")),
            amends: amends.iter().map(|&amended| AdrId::Legacy(amended)).collect(),
        }
    }

    // Goal: a row links the ADR and shows its title, status and summary, with pipes escaped, and
    // names the ADRs that amend it, marked with their status unless they are accepted.
    #[test]
    fn rows_name_the_amending_adrs() {
        let entries = [
            entry(1, "Accepted", &[]),
            entry(2, "Accepted (2026-11-28)", &[1]),
            entry(3, "Proposed (point 6 Accepted)", &[1, 3]),
        ];
        let text = render(&entries);
        assert!(text.starts_with("# Architecture decision records\n"), "{text}");
        assert!(text.contains("| ADR | Title | Status | Amended by | Summary |\n"), "{text}");
        assert!(
            text.contains(
                "| [0001](0001-x.md) | Title 1 | Accepted | [0002](0002-x.md), [0003](0003-x.md) \
                 (proposed) | Rule 1 \\| piped |\n"
            ),
            "{text}"
        );
        assert!(text.contains("| [0003](0003-x.md) | Title 3 | Proposed (point 6 Accepted) |  |"));
        assert!(text.ends_with("readiness-notes/).\n"), "{text}");
    }
}
