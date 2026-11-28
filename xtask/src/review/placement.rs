//! Where a finding's thread goes on the pull request's diff (#59).
//!
//! GitHub takes an inline review comment only on a line inside one of the pull request's
//! hunks, on the side of the head (`RIGHT`). A finding on such a line stays where it is. A
//! finding elsewhere in a changed file goes to the nearest such line of that file, and one in
//! a file the pull request does not change, or in a file without a patch, goes to the first
//! such line of the pull request. A moved finding names its real place in the comment, as the
//! `review-pr` skill does.

/// The lines of one changed file that a thread may be placed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commentable {
    /// The file's path in the head.
    pub path: String,
    /// Its head-side lines inside the hunks, added or context, ascending.
    pub lines: Vec<u32>,
}

/// Where a finding's thread goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The file the thread is on.
    pub path: String,
    /// The head-side line the thread is on.
    pub line: u32,
    /// True when this is not the finding's own place, which the comment then names.
    pub moved: bool,
}

/// Most lines of one patch read: far above what GitHub sends for one file.
const PATCH_LINES_MAX: usize = 1_000_000;

/// The head-side lines that GitHub's `patch` of one file covers: from each hunk header's
/// `+start`, every line that begins with `+` or a space, counted up; `-` lines and `\ No
/// newline` markers do not move the head-side count. A header that does not parse ends the
/// reading: the lines read before it stay.
#[must_use]
pub fn head_lines(patch: &str) -> Vec<u32> {
    let mut lines = Vec::new();
    let mut next: Option<u32> = None;
    for text in patch.lines().take(PATCH_LINES_MAX) {
        if text.starts_with("@@") {
            next = hunk_start(text);
            if next.is_none() {
                break;
            }
            continue;
        }
        let Some(line) = next else { continue };
        if text.starts_with('+') || text.starts_with(' ') || text.is_empty() {
            lines.push(line);
            next = line.checked_add(1);
        }
    }
    lines.sort_unstable();
    lines.dedup();
    assert!(lines.windows(2).all(|pair| pair.first() < pair.last()), "lines ascend");
    lines
}

/// The head-side start of the hunk header `@@ -a,b +c,d @@ ...`: `c`, or 1 for `+0,0`, the
/// header of a deleted file's hunk, which has no head-side lines anyway.
fn hunk_start(header: &str) -> Option<u32> {
    let rest = header.strip_prefix("@@ -")?;
    let (_, after) = rest.split_once(" +")?;
    let (range, _) = after.split_once(" @@")?;
    let start = range.split_once(',').map_or(range, |(start, _)| start);
    let start: u32 = start.parse().ok()?;
    Some(start.max(1))
}

/// Where the thread of a finding at `path`, `line` goes among `files`; `None` when no file
/// has a line a thread can be placed on.
#[must_use]
pub fn place(files: &[Commentable], path: &str, line: u32) -> Option<Placed> {
    assert!(line > 0, "lines count from 1");
    let same = files.iter().find(|file| file.path == path && !file.lines.is_empty());
    if let Some(file) = same {
        let nearest = file.lines.iter().min_by_key(|candidate| candidate.abs_diff(line))?;
        let placed = Placed { path: path.to_owned(), line: *nearest, moved: *nearest != line };
        assert!(file.lines.contains(&placed.line), "the thread is on a line of the diff");
        return Some(placed);
    }
    let first = files.iter().find(|file| !file.lines.is_empty())?;
    let first_line = *first.lines.first()?;
    Some(Placed { path: first.path.clone(), line: first_line, moved: true })
}

#[cfg(test)]
mod tests {
    use super::{Commentable, Placed, head_lines, place};

    // Goal: head-side lines are counted from each hunk's start, over added and context lines,
    // and not over removed lines or the no-newline marker.
    #[test]
    fn head_lines_follow_the_hunks() {
        let patch = "@@ -1,3 +1,4 @@\n a\n-b\n+c\n+d\n e\n@@ -20,2 +21,2 @@ fn x()\n f\n-g\n+h\n\
                     \\ No newline at end of file";
        assert_eq!(head_lines(patch), [1, 2, 3, 4, 21, 22]);
        assert_eq!(head_lines("@@ -1,2 +0,0 @@\n-a\n-b"), Vec::<u32>::new());
        assert_eq!(head_lines("@@ broken\n+a"), Vec::<u32>::new());
    }

    // Goal: a finding on a diff line stays; one elsewhere in a changed file moves to the
    // nearest diff line; one in another file, or in a file without diff lines, moves to the
    // first diff line of the pull request; with no diff line at all, nothing is placed.
    #[test]
    fn findings_go_to_the_nearest_diff_line() {
        let files = [
            Commentable { path: "bin.dat".to_owned(), lines: vec![] },
            Commentable { path: "a.rs".to_owned(), lines: vec![10, 11, 30] },
            Commentable { path: "b.rs".to_owned(), lines: vec![5] },
        ];
        let at = |path: &str, line, moved| Placed { path: path.to_owned(), line, moved };
        assert_eq!(place(&files, "a.rs", 11), Some(at("a.rs", 11, false)));
        assert_eq!(place(&files, "a.rs", 25), Some(at("a.rs", 30, true)));
        assert_eq!(place(&files, "b.rs", 1), Some(at("b.rs", 5, true)));
        assert_eq!(place(&files, "other.rs", 3), Some(at("a.rs", 10, true)));
        assert_eq!(place(&files, "bin.dat", 3), Some(at("a.rs", 10, true)));
        assert_eq!(place(&files[..1], "a.rs", 3), None);
    }
}
