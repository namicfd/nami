//! ADR IDs that a pull request takes while `main` or another open pull request also uses them
//! (issue #51, ADR-20261128-1856).
//!
//! Two ADR files with one ID merge without a git conflict, and tidy's `adr-number` rule then
//! fails whichever PR reaches the merge queue second. IDs are UTC minutes, so a clash means two
//! ADRs started in the same minute. This finds it while both PRs are still open and fails the
//! PR that moves its ADR: the one whose clash is with `main`, or with an older open PR. The
//! older of two open PRs keeps its ID, so the two never both move.
//!
//! A clash is an ID that another file name also carries: the same file in two places, such as
//! a PR stacked on another, is the same ADR and no clash. Only the ID of a file is ever quoted,
//! never the rest of its name, because another PR's file names are not ours to repeat.

use crate::adr::id::AdrId;
use std::collections::{BTreeMap, BTreeSet};

use super::forge::PullFile;

/// The directory that holds the ADRs, relative to the repository root.
pub const ADR_DIRECTORY: &str = "docs/design/";

/// The first line of the comment that this check posts, so that it finds the comment again.
pub const MARKER: &str = "<!-- nami:adr-clashes -->";

/// Minutes that [`free_after`] tries after a clash.
const MINUTES_MAX: u32 = 1000;

/// Where the other user of an ADR ID is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Holder {
    /// The base branch already has it.
    Main,
    /// Another open pull request adds it.
    Pull(u32),
}

/// One ADR ID that the pull request adds and someone else also uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clash {
    /// The ID, such as `20261128-1856`.
    pub id: AdrId,
    /// Who else uses it.
    pub holder: Holder,
}

impl Clash {
    /// True if pull request `pull`, which has this clash, moves its ADR: the other holder is
    /// `main`, or an open pull request with a lower number.
    #[must_use]
    pub fn moves(self, pull: u32) -> bool {
        match self.holder {
            Holder::Main => true,
            Holder::Pull(other) => {
                assert!(other != pull, "a pull request does not clash with itself");
                other < pull
            }
        }
    }
}

/// The ADR ID of a file name, such as `0022` for `0022-title.md`. `None` for other files.
#[must_use]
pub fn adr_id(name: &str) -> Option<AdrId> {
    let id = AdrId::of_file_name(name);
    let is_markdown = std::path::Path::new(name).extension().is_some_and(|ext| ext == "md");
    assert!(id.is_none() || is_markdown, "ADR files are Markdown");
    id
}

/// The file name of `path` if it is an ADR in [`ADR_DIRECTORY`].
fn adr_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix(ADR_DIRECTORY).filter(|name| !name.contains('/'))?;
    adr_id(name).map(|_| name)
}

/// GitHub gives a PR's paths from the repository root. A path from the filesystem's root,
/// before or after a rename, means the data is not what this check understands.
fn check_relative(files: &[PullFile]) -> Result<(), String> {
    let absolute = files.iter().find(|file| {
        file.path.starts_with('/')
            || file.previous.as_deref().is_some_and(|path| path.starts_with('/'))
    });
    match absolute {
        Some(file) => Err(format!("GitHub sent the absolute path `{}` for a PR's file", file.path)),
        None => Ok(()),
    }
}

/// The ADR file names that a pull request's `files` bring in: added, copied, or renamed to.
///
/// # Errors
///
/// When a path is absolute.
pub fn added_adrs(files: &[PullFile]) -> Result<BTreeSet<String>, String> {
    check_relative(files)?;
    Ok(files
        .iter()
        .filter(|file| matches!(file.status.as_str(), "added" | "copied" | "renamed"))
        .filter_map(|file| adr_name(&file.path).map(str::to_owned))
        .collect())
}

/// The ADR file names that a pull request's `files` take away: removed, or renamed from.
///
/// # Errors
///
/// When a path is absolute.
pub fn removed_adrs(files: &[PullFile]) -> Result<BTreeSet<String>, String> {
    check_relative(files)?;
    Ok(files
        .iter()
        .filter_map(|file| match file.status.as_str() {
            "removed" => Some(file.path.as_str()),
            "renamed" => file.previous.as_deref(),
            _ => None,
        })
        .filter_map(|path| adr_name(path).map(str::to_owned))
        .collect())
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
        let Some(id) = adr_id(name) else {
            continue;
        };
        let clashes_here = own.iter().any(|mine| adr_id(mine) == Some(id) && mine != name);
        if clashes_here {
            let _ = found.insert(Clash { id, holder });
        }
    }
    assert!(
        found.iter().all(|clash| own.iter().any(|mine| adr_id(mine) == Some(clash.id))),
        "every clash is on an ID that the pull request adds"
    );
    found
}

/// The IDs of the ADR file names in `names`.
#[must_use]
pub fn ids_of<'names>(names: impl IntoIterator<Item = &'names String>) -> BTreeSet<AdrId> {
    names.into_iter().filter_map(|name| adr_id(name)).collect()
}

/// The first minute after `id` that no ADR in `taken` has: where a moved ADR goes. `None` for
/// a legacy number, which no new ADR may take.
#[must_use]
pub fn free_after(id: AdrId, taken: &BTreeSet<AdrId>) -> Option<AdrId> {
    let AdrId::Stamp(mut stamp) = id else {
        return None;
    };
    for _ in 0..MINUTES_MAX {
        stamp = stamp.next_minute();
        if !taken.contains(&AdrId::Stamp(stamp)) {
            return Some(AdrId::Stamp(stamp));
        }
    }
    panic!("{MINUTES_MAX} minutes after ADR-{id} are all taken");
}

/// Where pull request `pull` moves each of its ADRs that clashes, in words: the first free
/// minute after it, given the IDs `taken` anywhere, and never a minute already handed to
/// another of its ADRs. A legacy number gets a new ID from `cargo xtask adr new`.
#[must_use]
pub fn destinations(
    pull: u32,
    clashes: &BTreeSet<Clash>,
    taken: &BTreeSet<AdrId>,
) -> BTreeMap<AdrId, String> {
    let mut handed = taken.clone();
    let mut chosen: Vec<AdrId> = Vec::new();
    let mut found = BTreeMap::new();
    for clash in clashes.iter().filter(|clash| clash.moves(pull)) {
        if found.contains_key(&clash.id) {
            continue;
        }
        let text = match free_after(clash.id, &handed) {
            Some(free) => {
                let _ = handed.insert(free);
                chosen.push(free);
                format!("ADR-{free}, the next free minute")
            }
            None => "a new ID from `cargo xtask adr new <slug>`".to_owned(),
        };
        let _ = found.insert(clash.id, text);
    }
    let distinct: BTreeSet<&AdrId> = chosen.iter().collect();
    assert!(
        distinct.len() == chosen.len() && chosen.iter().all(|free| !taken.contains(free)),
        "every destination is a free minute, and no two ADRs share one"
    );
    found
}

/// The comment for pull request `pull` with `clashes`, given the IDs `taken` anywhere.
#[must_use]
pub fn render(pull: u32, clashes: &BTreeSet<Clash>, taken: &BTreeSet<AdrId>) -> String {
    assert!(!clashes.is_empty(), "a comment without clashes is `resolved`");
    let moves = destinations(pull, clashes, taken);
    let to = |clash: &Clash| {
        moves.get(&clash.id).cloned().expect("every clash that moves has a destination")
    };
    let lines: Vec<String> = clashes
        .iter()
        .map(|clash| match clash.holder {
            Holder::Main => format!(
                "- ADR-{} is already on `main`: move this PR's ADR to {}.",
                clash.id,
                to(clash)
            ),
            Holder::Pull(other) if clash.moves(pull) => format!(
                "- ADR-{} is also added by #{other}, which is older: move this PR's ADR to {}.",
                clash.id,
                to(clash)
            ),
            Holder::Pull(other) => format!(
                "- ADR-{} is also added by #{other}, which is newer and moves its ADR.",
                clash.id
            ),
        })
        .collect();
    let text = format!(
        "{MARKER}\n### ADR IDs in use elsewhere\n\n{}\n\nGit merges two ADRs with one ID \
         without a conflict, and tidy's `adr-number` rule then fails the second PR in the merge \
         queue. This check fails the PR that moves: the one whose clash is with `main` or with \
         an older PR. To move an ADR, rename its file, change its `# ADR-…:` title and every \
         reference to it, and run `cargo xtask adr index --write` (ADR-20261128-1856). \
         `cargo xtask queue clashes --pr <n>` checks again, and names another minute if another \
         PR took this one first.\n",
        lines.join("\n")
    );
    assert!(text.starts_with(MARKER), "the marker finds the comment again");
    text
}

/// The comment once the clashes it reported are gone.
#[must_use]
pub fn resolved() -> String {
    format!("{MARKER}\n### ADR IDs in use elsewhere\n\nNo clashes any more.\n")
}

#[cfg(test)]
mod tests {
    use super::{Clash, Holder, MARKER, added_adrs, adr_id, clashes, destinations, free_after};
    use super::{ids_of, removed_adrs, render, resolved};
    use crate::adr::id::{AdrId, Stamp};
    use crate::queue::forge::PullFile;
    use std::collections::{BTreeMap, BTreeSet};

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

    fn stamp(hour: u32, minute: u32) -> AdrId {
        AdrId::Stamp(Stamp::new(2026, 11, 28, hour, minute).expect("a valid minute"))
    }

    // Goal: both forms of ADR file name carry an ID, and other files none.
    #[test]
    fn adr_ids_come_from_file_names() {
        assert_eq!(adr_id("0022-ratchet.md"), Some(AdrId::Legacy(22)));
        assert_eq!(adr_id("0000-records.md"), Some(AdrId::Legacy(0)));
        assert_eq!(adr_id("20261128-1856-ids.md"), Some(stamp(18, 56)));
        assert_eq!(adr_id("README.md"), None);
        assert_eq!(adr_id("0022_ratchet.md"), None);
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
            file("docs/design/20261128-1856-i.md", "added", None),
        ];
        assert_eq!(
            added_adrs(&files),
            Ok(names(&["0022-a.md", "0023-b.md", "0024-c.md", "0032-h.md", "20261128-1856-i.md"]))
        );
        assert_eq!(removed_adrs(&files), Ok(names(&["0006-e.md", "0021-b.md"])));
    }

    // Goal: a path from the filesystem's root, before or after a rename, is not GitHub's and
    // is an error that names it.
    #[test]
    fn absolute_paths_are_refused() {
        let absolute = [file("/docs/design/0001-a.md", "added", None)];
        let renamed = [file("docs/design/0002-b.md", "renamed", Some("/docs/design/0001-b.md"))];
        for files in [&absolute[..], &renamed[..]] {
            let added = added_adrs(files).expect_err("an absolute path");
            assert!(added.starts_with("GitHub sent the absolute path `"), "{added}");
            assert_eq!(removed_adrs(files), Err(added), "{files:?}");
        }
    }

    // Goal: an ID that main keeps or another PR adds under another file name is a clash; the
    // same file name is the same ADR, a file the PR removes from main is gone, and IDs that the
    // PR does not add are not its business. A stamp clashes with its own minute only.
    #[test]
    fn clashes_are_ids_under_other_names() {
        let own = names(&["0022-ratchet.md", "0023-queue.md", "20261128-1856-ids.md"]);
        let main = ["0021-old.md", "0022-other.md", "0023-queue.md", "20261128-1856-gone.md"]
            .map(str::to_owned);
        let removed = names(&["20261128-1856-gone.md"]);
        let others = [
            (45, names(&["0022-arch.md", "20261128-1857-arch.md"])),
            (46, names(&["0023-queue.md"])),
            (47, names(&["20261128-1856-new.md", "notes.md"])),
        ];
        let found = clashes(&own, &main, &removed, &others);
        let expected: BTreeSet<Clash> = [
            Clash { id: AdrId::Legacy(22), holder: Holder::Main },
            Clash { id: AdrId::Legacy(22), holder: Holder::Pull(45) },
            Clash { id: stamp(18, 56), holder: Holder::Pull(47) },
        ]
        .into();
        assert_eq!(found, expected);
        assert!(clashes(&BTreeSet::new(), &main, &removed, &others).is_empty(), "adds nothing");
    }

    // Goal: the PR moves its ADR when the clash is with main or an older PR, and keeps it when
    // the other PR is newer, so that two open PRs never both move.
    #[test]
    fn the_newer_pull_request_moves() {
        let on_main = Clash { id: stamp(18, 56), holder: Holder::Main };
        let with_45 = Clash { id: stamp(18, 56), holder: Holder::Pull(45) };
        assert!(on_main.moves(45) && on_main.moves(51));
        assert!(with_45.moves(51), "45 is older than 51");
        assert!(!with_45.moves(44), "45 is newer than 44");
    }

    // Goal: a moved stamp goes to the first later minute that no ADR has; a legacy number has no
    // free minute after it.
    #[test]
    fn a_moved_adr_takes_the_next_free_minute() {
        let taken =
            ids_of(&["20261128-1856-a.md", "20261128-1857-b.md", "README.md"].map(String::from));
        assert_eq!(taken, BTreeSet::from([stamp(18, 56), stamp(18, 57)]));
        assert_eq!(free_after(stamp(18, 56), &taken), Some(stamp(18, 58)));
        assert_eq!(free_after(stamp(18, 57), &BTreeSet::new()), Some(stamp(18, 58)));
        assert_eq!(free_after(AdrId::Legacy(22), &taken), None);
    }

    // Goal: two ADRs of one PR that both clash are sent to two different free minutes, an ID that
    // clashes twice gets one destination, and a clash the PR does not move gets none.
    #[test]
    fn each_moved_adr_gets_its_own_minute() {
        let found: BTreeSet<Clash> = [
            Clash { id: stamp(18, 56), holder: Holder::Main },
            Clash { id: stamp(18, 56), holder: Holder::Pull(45) },
            Clash { id: stamp(18, 57), holder: Holder::Main },
            Clash { id: stamp(19, 0), holder: Holder::Pull(60) },
        ]
        .into();
        let taken = BTreeSet::from([stamp(18, 56), stamp(18, 57), stamp(19, 0)]);
        let moves = destinations(51, &found, &taken);
        let to = |id: AdrId| format!("ADR-{id}, the next free minute");
        let expected = BTreeMap::from([
            (stamp(18, 56), to(stamp(18, 58))),
            (stamp(18, 57), to(stamp(18, 59))),
        ]);
        assert_eq!(moves, expected, "#60 is newer and moves its own ADR");
    }

    // Goal: the comment starts with the marker, names each ID with its holder and says who moves
    // and where to; the resolved comment keeps the marker.
    #[test]
    fn comments_name_ids_and_holders() {
        let found: BTreeSet<Clash> = [
            Clash { id: AdrId::Legacy(22), holder: Holder::Main },
            Clash { id: stamp(18, 56), holder: Holder::Pull(45) },
            Clash { id: stamp(18, 56), holder: Holder::Pull(60) },
        ]
        .into();
        let taken = BTreeSet::from([stamp(18, 56), stamp(18, 57)]);
        let text = render(51, &found, &taken);
        assert!(text.starts_with(MARKER), "{text}");
        assert!(text.contains("- ADR-0022 is already on `main`: move this PR's ADR to a new ID"));
        // Built at run time, so that this file cites no missing ADR itself.
        let moved = format!(
            "- ADR-20261128-1856 is also added by #45, which is older: move this PR's ADR to \
             ADR-{}, the next free minute.",
            "20261128-1858"
        );
        assert!(text.contains(&moved), "{text}");
        assert!(text.contains("by #60, which is newer and moves its ADR."), "{text}");
        assert!(text.contains("cargo xtask adr index --write"), "{text}");
        assert!(resolved().starts_with(MARKER) && resolved().contains("No clashes"));
    }
}
