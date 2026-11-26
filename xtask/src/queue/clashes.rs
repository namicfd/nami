//! ADR numbers that a pull request takes while `main` or another open pull request also uses
//! them (issue #51).
//!
//! Two ADR files with one number merge without a git conflict, and tidy's `adr-number` rule
//! then fails whichever PR reaches the merge queue second. This finds the clash while both PRs
//! are still open, so that the second one renumbers before it is queued (ADR-0000).
//!
//! A clash is a number that another file name also carries: the same file in two places, such
//! as a PR stacked on another, is the same ADR and no clash. Only the number of a file is ever
//! quoted, never the rest of its name, because another PR's file names are not ours to repeat.

use crate::tidy::docs::is_adr_file_name;
use std::collections::BTreeSet;

use super::forge::PullFile;

/// The directory that holds the ADRs, relative to the repository root.
pub const ADR_DIRECTORY: &str = "docs/design/";

/// The first line of the comment that this check posts, so that it finds the comment again.
pub const MARKER: &str = "<!-- nami:adr-clashes -->";

/// Where the other user of an ADR number is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Holder {
    /// The base branch already has it.
    Main,
    /// Another open pull request adds it.
    Pull(u32),
}

/// One ADR number that the pull request adds and someone else also uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clash {
    /// The ADR number, such as 22 for `0022-...md`.
    pub number: u32,
    /// Who else uses it.
    pub holder: Holder,
}

/// The ADR number of a file name, such as 22 for `0022-title.md`. `None` for other files.
#[must_use]
pub fn adr_number(name: &str) -> Option<u32> {
    if !is_adr_file_name(name) {
        return None;
    }
    let number = name.get(..4).and_then(|digits| digits.parse().ok());
    assert!(number.is_some(), "an ADR file name starts with four digits");
    number
}

/// The file name of `path` if it is an ADR in [`ADR_DIRECTORY`].
fn adr_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix(ADR_DIRECTORY).filter(|name| !name.contains('/'))?;
    adr_number(name).map(|_| name)
}

/// True if the file's paths, before and after, are relative to the repository root.
fn is_relative(file: &PullFile) -> bool {
    !file.path.starts_with('/')
        && file.previous.as_deref().is_none_or(|path| !path.starts_with('/'))
}

/// The ADR file names that a pull request's `files` bring in: added, copied, or renamed to.
#[must_use]
pub fn added_adrs(files: &[PullFile]) -> BTreeSet<String> {
    assert!(files.iter().all(is_relative), "GitHub gives a PR's paths from the repository root");
    let added: BTreeSet<String> = files
        .iter()
        .filter(|file| matches!(file.status.as_str(), "added" | "copied" | "renamed"))
        .filter_map(|file| adr_name(&file.path).map(str::to_owned))
        .collect();
    added
}

/// The ADR file names that a pull request's `files` take away: removed, or renamed from.
#[must_use]
pub fn removed_adrs(files: &[PullFile]) -> BTreeSet<String> {
    assert!(files.iter().all(is_relative), "GitHub gives a PR's paths from the repository root");
    let removed: BTreeSet<String> = files
        .iter()
        .filter_map(|file| match file.status.as_str() {
            "removed" => Some(file.path.as_str()),
            "renamed" => file.previous.as_deref(),
            _ => None,
        })
        .filter_map(|path| adr_name(path).map(str::to_owned))
        .collect();
    removed
}

/// The clashes of the ADRs that a pull request adds (`own`) with the ADRs on main that it keeps
/// (`main` without `removed`), and with the ADRs that other open pull requests add.
#[must_use]
pub fn clashes(
    own: &BTreeSet<String>,
    main: &[String],
    removed: &BTreeSet<String>,
    others: &[(u32, BTreeSet<String>)],
) -> BTreeSet<Clash> {
    let kept = main.iter().filter(|name| !removed.contains(*name));
    let holders = kept.map(|name| (Holder::Main, name)).chain(
        others.iter().flat_map(|(pull, names)| names.iter().map(|n| (Holder::Pull(*pull), n))),
    );
    let mut found = BTreeSet::new();
    for (holder, name) in holders {
        let Some(number) = adr_number(name) else {
            continue;
        };
        let clashes_here = own.iter().any(|mine| adr_number(mine) == Some(number) && mine != name);
        if clashes_here {
            let _ = found.insert(Clash { number, holder });
        }
    }
    assert!(
        found.iter().all(|clash| own.iter().any(|mine| adr_number(mine) == Some(clash.number))),
        "every clash is on a number that the pull request adds"
    );
    found
}

/// One above the highest ADR number in `names`: the number a renumbered ADR takes.
#[must_use]
pub fn next_free<'names>(names: impl IntoIterator<Item = &'names String>) -> u32 {
    let highest = names.into_iter().filter_map(|name| adr_number(name)).max();
    highest.map_or(0, |highest| highest + 1)
}

/// The comment for a pull request with `clashes`, which names the next free number.
#[must_use]
pub fn render(clashes: &BTreeSet<Clash>, next: u32) -> String {
    assert!(!clashes.is_empty(), "a comment without clashes is `resolved`");
    let lines: Vec<String> = clashes
        .iter()
        .map(|clash| match clash.holder {
            Holder::Main => format!(
                "- ADR-{:04} is already on `main`: this PR lands second, so it renumbers.",
                clash.number
            ),
            Holder::Pull(pull) => format!(
                "- ADR-{:04} is also added by #{pull}: whichever of the two lands second \
                 renumbers.",
                clash.number
            ),
        })
        .collect();
    let text = format!(
        "{MARKER}\n### ADR numbers in use elsewhere\n\n{}\n\nGit merges two ADRs with one \
         number without a conflict, and tidy's `adr-number` rule then fails the second PR in \
         the merge queue. To renumber, change the file name, the `# ADR-NNNN:` title, the row \
         in `docs/design/README.md` and every reference (ADR-0000). The next free number is \
         ADR-{next:04}.\n\nThis comment is advisory; `cargo xtask queue clashes --pr <n>` \
         checks again.\n",
        lines.join("\n")
    );
    assert!(text.starts_with(MARKER), "the marker finds the comment again");
    text
}

/// The comment once the clashes it reported are gone.
#[must_use]
pub fn resolved() -> String {
    format!("{MARKER}\n### ADR numbers in use elsewhere\n\nNo clashes any more.\n")
}

#[cfg(test)]
mod tests {
    use super::{Clash, Holder, MARKER, added_adrs, adr_number, clashes, next_free, removed_adrs};
    use super::{render, resolved};
    use crate::queue::forge::PullFile;
    use std::collections::BTreeSet;

    fn file(path: &str, status: &str, previous: Option<&str>) -> PullFile {
        PullFile {
            path: path.to_owned(),
            status: status.to_owned(),
            previous: previous.map(str::to_owned),
        }
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    // Goal: only `NNNN-title.md` files carry an ADR number.
    #[test]
    fn adr_numbers_come_from_file_names() {
        assert_eq!(adr_number("0022-ratchet.md"), Some(22));
        assert_eq!(adr_number("0000-records.md"), Some(0));
        assert_eq!(adr_number("README.md"), None);
        assert_eq!(adr_number("0022_ratchet.md"), None);
    }

    // Goal: a PR adds the ADRs that it adds, copies or renames to, in `docs/design/` itself, and
    // takes away those it removes or renames from; other files and other states do not count.
    #[test]
    fn added_and_removed_adrs_are_read_from_the_files() {
        let files = [
            file("docs/design/0022-a.md", "added", None),
            file("docs/design/0023-b.md", "renamed", Some("docs/design/0021-b.md")),
            file("docs/design/0024-c.md", "copied", Some("docs/design/0001-x.md")),
            file("docs/design/0005-d.md", "modified", None),
            file("docs/design/0006-e.md", "removed", None),
            file("docs/design/README.md", "added", None),
            file("docs/design/old/0030-f.md", "added", None),
            file("docs/0031-g.md", "added", None),
            file("docs/design/0032-h.md", "renamed", Some("docs/elsewhere.md")),
        ];
        assert_eq!(
            added_adrs(&files),
            names(&["0022-a.md", "0023-b.md", "0024-c.md", "0032-h.md"])
        );
        assert_eq!(removed_adrs(&files), names(&["0006-e.md", "0021-b.md"]));
    }

    // Goal: a path from the filesystem's root, before or after a rename, is not GitHub's and
    // stops the check.
    #[test]
    fn absolute_paths_are_refused() {
        let absolute = [file("/docs/design/0001-a.md", "added", None)];
        let renamed = [file("docs/design/0002-b.md", "renamed", Some("/docs/design/0001-b.md"))];
        for files in [&absolute[..], &renamed[..]] {
            let added = std::panic::catch_unwind(|| added_adrs(files));
            let removed = std::panic::catch_unwind(|| removed_adrs(files));
            assert!(added.is_err() && removed.is_err(), "{files:?}");
        }
    }

    // Goal: a number that main keeps or another PR adds under another file name is a clash; the
    // same file name is the same ADR, a file the PR removes from main is gone, and numbers that
    // the PR does not add are not its business.
    #[test]
    fn clashes_are_numbers_under_other_names() {
        let own = names(&["0022-ratchet.md", "0023-queue.md", "0040-solo.md"]);
        let main =
            ["0021-old.md", "0022-other.md", "0023-queue.md", "0040-gone.md"].map(str::to_owned);
        let removed = names(&["0040-gone.md"]);
        let others = [
            (45, names(&["0022-arch.md", "0024-arch.md"])),
            (46, names(&["0023-queue.md"])),
            (47, names(&["0040-new.md", "notes.md"])),
        ];
        let found = clashes(&own, &main, &removed, &others);
        let expected: BTreeSet<Clash> = [
            Clash { number: 22, holder: Holder::Main },
            Clash { number: 22, holder: Holder::Pull(45) },
            Clash { number: 40, holder: Holder::Pull(47) },
        ]
        .into();
        assert_eq!(found, expected);
        assert!(clashes(&BTreeSet::new(), &main, &removed, &others).is_empty(), "adds nothing");
    }

    // Goal: the next free number is one above the highest anywhere; with no ADR it is 0.
    #[test]
    fn the_next_free_number_is_above_the_highest() {
        let all = ["0001-a.md", "0031-b.md", "README.md", "0007-c.md"].map(str::to_owned);
        assert_eq!(next_free(&all), 32);
        assert_eq!(next_free(&[]), 0);
    }

    // Goal: the comment starts with the marker, names each number with its holder and the next
    // free number, and says who renumbers; the resolved comment keeps the marker.
    #[test]
    fn comments_name_numbers_and_holders() {
        let found: BTreeSet<Clash> = [
            Clash { number: 22, holder: Holder::Main },
            Clash { number: 23, holder: Holder::Pull(45) },
        ]
        .into();
        let text = render(&found, 32);
        assert!(text.starts_with(MARKER), "{text}");
        assert!(text.contains("- ADR-0022 is already on `main`: this PR lands second"), "{text}");
        assert!(text.contains("- ADR-0023 is also added by #45: whichever"), "{text}");
        assert!(text.contains("The next free number is ADR-0032."), "{text}");
        assert!(text.contains("`docs/design/README.md`") && text.contains("ADR-0000"), "{text}");
        assert!(resolved().starts_with(MARKER) && resolved().contains("No clashes"));
    }
}
