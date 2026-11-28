//! `cargo xtask adr`: ADR IDs, new ADRs and the generated index (ADR-20261128-1856).
//!
//! - `adr new <slug>` writes `docs/design/YYYYMMDD-HHMM-<slug>.md` from the template, named
//!   after the current UTC minute, and regenerates the index. Agents never type an ID: two
//!   branches then clash only if they start an ADR in the same minute, and the `adr clashes`
//!   check on each pull request fails the one that moves.
//! - `adr index [--write]` prints or writes `docs/design/README.md`.
//!
//! ADRs 0000 to 0043 keep their numbers ([`id::LEGACY_NUMBERS`]).

pub mod id;
pub mod index;
pub mod rules;

use crate::tidy::{ADR_DIRECTORY, Violation};
use id::{AdrId, Stamp, is_slug};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask adr new <slug> | index [--write]";

/// Minutes that [`new_adr`] tries after the current one when that one is taken.
const MINUTES_MAX: u32 = 1000;

/// Runs one `cargo xtask adr` command in the checkout around the current directory.
pub fn run(arguments: &[&str]) -> ExitCode {
    let root = crate::workspace_root();
    let result = match arguments {
        ["new", slug] => now().and_then(|stamp| new_adr(&root, slug, stamp)).map(|path| {
            format!(
                "adr: wrote {}; write its title, summary and sections, then run `{}`\n",
                path.display(),
                index::WRITE_COMMAND
            )
        }),
        ["index"] => index::entries(&root)
            .map(|entries| index::render(&entries))
            .map_err(|e| format!("cannot read the ADRs: {e}")),
        ["index", "--write"] => {
            write_index(&root).map(|()| format!("adr: wrote {}\n", index::INDEX))
        }
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("adr: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The current UTC minute.
///
/// # Errors
///
/// When the clock is before 2026 or after 9999, which makes no ID.
#[expect(
    clippy::disallowed_methods,
    reason = "an ADR's ID is the UTC minute it is written in; nothing else in xtask reads the \
              clock (ADR-20261128-1856)"
)]
fn now() -> Result<Stamp, String> {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let seconds = since
        .map_err(|_| "the clock is before 1970, which no ADR ID names; set the clock".to_owned())?
        .as_secs();
    Stamp::from_unix_seconds(seconds).ok_or_else(|| {
        format!("the clock says {seconds} s after 1970, which no ADR ID names; set the clock")
    })
}

/// Writes a new ADR named `slug` in the tree at `root`, with the ID `now`, or the first later
/// minute that no ADR has, and regenerates the index. Returns the new file, relative to `root`.
///
/// # Errors
///
/// When the slug is malformed, or the directory cannot be read or written.
pub fn new_adr(root: &Path, slug: &str, now: Stamp) -> Result<PathBuf, String> {
    if !is_slug(slug) {
        return Err(format!(
            "`{slug}` is no slug: write lower-case words joined by dashes, such as \
             `adr-ids-are-minutes`; {USAGE}"
        ));
    }
    let read = |error: io::Error| format!("cannot read the ADRs: {error}");
    let taken: BTreeSet<AdrId> = index::entries(root).map_err(read)?.iter().map(|e| e.id).collect();
    let mut stamp = now;
    for _ in 0..MINUTES_MAX {
        if !taken.contains(&AdrId::Stamp(stamp)) {
            break;
        }
        stamp = stamp.next_minute();
    }
    let id = AdrId::Stamp(stamp);
    assert!(!taken.contains(&id), "{MINUTES_MAX} minutes after {now:?} are all taken");
    let relative = Path::new(ADR_DIRECTORY).join(format!("{id}-{slug}.md"));
    let write = |error: io::Error| format!("cannot write {}: {error}", relative.display());
    fs::write(root.join(&relative), template(id, stamp, slug)).map_err(write)?;
    write_index(root)?;
    Ok(relative)
}

/// The text of a new ADR: the header and the six sections that ADR-0000 asks for, each with
/// what belongs in it.
fn template(id: AdrId, stamp: Stamp, slug: &str) -> String {
    let mut title = slug.replace('-', " ");
    if let Some(first) = title.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let text = format!(
        "# ADR-{id}: {title}\n\n\
         - Status: Proposed\n\
         - Date: {date}\n\
         - Deciders: maintainer\n\
         - Summary: {summary}\n\n\
         ## Context\n\nWhat forces the decision: the problem, what happened, what it cost.\n\n\
         ## Decision\n\nWhat is decided, as numbered rules.\n\n\
         ## Enforcement\n\nThe mechanism that makes each rule hold: a lint, a tidy rule, a \
         dylint lint, a hook, a CI job or CODEOWNERS. A rule without one says so.\n\n\
         ## Consequences\n\nWhat changes, what gets harder, and the known gaps.\n\n\
         ## Alternatives considered\n\nEach alternative and why it lost.\n\n\
         ## Evidence\n\nWhat the decision rests on: measurements, incidents, sources.\n",
        date = stamp.date(),
        summary = index::PLACEHOLDER_SUMMARY
    );
    assert!(text.starts_with(&format!("# ADR-{id}: ")), "the title carries the ID");
    text
}

/// Writes the generated index into the tree at `root`.
fn write_index(root: &Path) -> Result<(), String> {
    let entries = index::entries(root).map_err(|error| format!("cannot read the ADRs: {error}"))?;
    let text = index::render(&entries);
    fs::write(root.join(index::INDEX), text)
        .map_err(|error| format!("cannot write {}: {error}", index::INDEX))
}

/// The workspace rules about ADRs that do not compare the tree with this binary: `adr-id`,
/// `adr-reference` and `adr-summary`. The index rule, `adr-index-doc`, is [`index::check`].
///
/// # Errors
///
/// When a directory or file cannot be read.
pub fn check(root: &Path) -> io::Result<Vec<Violation>> {
    let entries = index::entries(root)?;
    let ids: BTreeSet<AdrId> = entries.iter().map(|entry| entry.id).collect();
    let mut violations = rules::check_file_names(root)?;
    violations.extend(rules::check_references(root, &ids)?);
    violations.extend(index::check_summaries(&entries));
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::id::Stamp;
    use super::{check, index, new_adr};
    use crate::tidy::scratch::Scratch;
    use std::fs;

    fn stamp(hour: u32, minute: u32) -> Stamp {
        Stamp::new(2026, 11, 28, hour, minute).expect("a valid minute")
    }

    // Goal: a new ADR gets the current minute, or the next free one when that is taken, starts
    // from the template with its ID in the title, and is in the regenerated index.
    #[test]
    fn new_adrs_take_a_free_minute() {
        let scratch = Scratch::new("adr-new");
        scratch.write("docs/design/README.md", "");
        // The index's preamble cites these two.
        for (number, name) in [("0000", "records"), ("0015", "names")] {
            let text = format!("# ADR-{number}: T\n\n- Status: Accepted\n- Summary: S.\n");
            scratch.write(&format!("docs/design/{number}-{name}.md"), &text);
        }
        let first = new_adr(scratch.root(), "first-rule", stamp(18, 56)).unwrap();
        assert_eq!(first.to_string_lossy(), "docs/design/20261128-1856-first-rule.md");
        let text = fs::read_to_string(scratch.root().join(&first)).unwrap();
        assert!(text.starts_with("# ADR-20261128-1856: First rule\n\n- Status: Proposed\n"));
        assert!(text.contains("- Date: 2026-11-28\n"), "{text}");
        let second = new_adr(scratch.root(), "second", stamp(18, 56)).unwrap();
        assert_eq!(second.to_string_lossy(), "docs/design/20261128-1857-second.md");
        let written = fs::read_to_string(scratch.root().join(index::INDEX)).unwrap();
        assert_eq!(written, index::render(&index::entries(scratch.root()).unwrap()));
        assert!(written.contains("[20261128-1857](20261128-1857-second.md)"), "{written}");
        assert_eq!(index::check(scratch.root()).unwrap(), Vec::new(), "the index is current");
        let unedited = check(scratch.root()).unwrap();
        let rules: Vec<&str> = unedited.iter().map(|violation| violation.rule).collect();
        assert_eq!(rules, ["adr-summary"; 2], "an unedited summary cannot merge");
        assert!(unedited[0].message.contains("the template's summary"), "{}", unedited[0]);
        let edited = text.replace(index::PLACEHOLDER_SUMMARY, "A rule.");
        scratch.write(&first.to_string_lossy(), &edited);
        assert_eq!(check(scratch.root()).unwrap().len(), 1, "only the second is unedited");
        let error = new_adr(scratch.root(), "Bad_Slug", stamp(19, 3)).unwrap_err();
        assert!(error.contains("is no slug"), "{error}");
    }

    // Goal: the template has the six sections and the header that the per-file ADR rules ask
    // for, so the edit hook is quiet while the author writes; only the workspace rule
    // `adr-summary` asks for the real summary.
    #[test]
    fn the_template_passes_the_per_file_adr_rules() {
        let scratch = Scratch::new("adr-template");
        scratch.write("docs/design/README.md", "");
        let path = new_adr(scratch.root(), "x", stamp(23, 59)).unwrap();
        let text = fs::read_to_string(scratch.root().join(&path)).unwrap();
        let violations = crate::tidy::check_single_file(scratch.root(), &path, &text).unwrap();
        assert_eq!(violations, Vec::new());
    }
}
