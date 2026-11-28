//! Mutation evidence carried into the merge queue (ADR-0022, amending ADR-0009 for the ratchet
//! job; review of #55, thread 7).
//!
//! The ratchet is a required check, so it runs on every merge group, and the queue's timeout
//! is 30 minutes. Mutation testing and the null mutant run the head's tests once per mutant,
//! which takes longer than that on a lint change. So on a merge group the head job observes
//! without them, and the judge re-runs only the cheap checks on the queued merge against its
//! new base: the rule set, the corpus, end to end and the review gate. The mutation evidence
//! comes from the pull request's last observation, which its own `guardrails` run made.
//!
//! The evidence counts only for the same rule code. The judge is given the observation and the
//! two trees it observed (the test merge and its first parent), and it compares, as data, the
//! rule-path diff of those trees with the rule-path diff of the queued merge: the same rule
//! paths changed, each with the same text on both sides. Only then do the line numbers of the
//! mutants and the files of the null run mean the same in both. Any difference, a missing
//! observation, or one of another pull request head, is no verdict. What the carried runs say
//! is judged again by this judge, so a run with a missed mutant fails here too.
//!
//! What a carried run does not see: a change to the trusted core, outside the rule paths,
//! between the observation and the queue. The corpus and end to end are run again on the
//! queued merge, so how the rules apply is shown there; the mutants only ever show that the
//! head's tests pin its own rule code.

use super::Reason;
use super::observations::Observations;
use super::source::read_optional;
use std::path::{Path, PathBuf};

/// The name of the observations in a carried directory, as `ratchet (head)` uploads them.
pub const OBSERVATIONS: &str = "observations.json";

/// The mutation evidence of an earlier observation, and the two trees it observed.
#[derive(Clone, Debug)]
pub struct Carried {
    /// The observation, with its mutation and null-mutant runs.
    pub observations: Observations,
    /// The tree the observation's head was compared with: the test merge's first parent.
    pub base: PathBuf,
    /// The tree it observed: the test merge.
    pub head: PathBuf,
}

impl Carried {
    /// Reads a carried directory: [`OBSERVATIONS`], and the trees `base/` and `head/`.
    ///
    /// # Errors
    ///
    /// No verdict: a file or tree that is missing or cannot be read.
    pub fn read(directory: &Path) -> Result<Self, String> {
        let observations = Observations::read(&directory.join(OBSERVATIONS))?;
        let (base, head) = (directory.join("base"), directory.join("head"));
        if !base.is_dir() || !head.is_dir() {
            return Err(format!(
                "the carried evidence in {} lacks the trees `base/` and `head/` it observed",
                directory.display()
            ));
        }
        Ok(Self { observations, base, head })
    }
}

/// One changed rule path, with its text on each side; `None` where it does not exist.
type Change = (String, Option<String>, Option<String>);

/// The rule-path diff of `base` to `head`, as data: every changed rule path, in order, with its
/// text on each side, read as the judge reads the head.
///
/// # Errors
///
/// No verdict: a tree that cannot be compared, or a file that cannot be read as a regular one.
pub fn rule_diff(base: &Path, head: &Path) -> Result<Vec<Change>, String> {
    let changed = super::tree::changed_paths(base, head)
        .map_err(|error| format!("cannot compare the trees: {error}"))?;
    let mut diff = Vec::new();
    for path in changed.into_iter().filter(|path| super::scope::is_rule_path(path)) {
        let (before, after) = (read_optional(base, &path)?, read_optional(head, &path)?);
        assert!(before != after, "a changed path differs between the trees");
        diff.push((path, before, after));
    }
    Ok(diff)
}

/// Checks that `carried` holds evidence for the queued merge `head` of `base`, of the pull
/// request head `pr_head` if given, and returns the note that says where it comes from.
///
/// # Errors
///
/// No verdict, with its kind. Missing evidence ([`Reason::Evidence`]): an observation of
/// another pull request head, one that skipped the head, or one of another tree than the
/// carried head (by fingerprint). [`Reason::CarriedDiff`]: one whose rule-path diff is not the
/// queued merge's, path for path and text for text.
pub fn check(
    base: &Path,
    head: &Path,
    carried: &Carried,
    pr_head: Option<&str>,
) -> Result<String, (Reason, String)> {
    let missing = |reason: String| (Reason::Evidence, reason);
    let observed = &carried.observations;
    if let Some(expected) = pr_head
        && observed.pr_head != expected
    {
        return Err(missing(format!(
            "the carried mutation evidence observed the pull request head `{}`, not `{expected}`",
            observed.pr_head
        )));
    }
    if observed.skipped {
        return Err(missing(
            "the carried observation skipped the head, so it holds no mutation evidence".to_owned(),
        ));
    }
    let (observed_tree, links) = super::tree::fingerprint(&carried.head)
        .map_err(|error| missing(format!("cannot read the carried head tree: {error}")))?;
    if observed.head != observed_tree || !links.is_empty() {
        return Err(missing(format!(
            "the carried observation is of another head tree (fingerprint {}, the carried tree's \
             {observed_tree}), so its runs say nothing about these trees",
            observed.head
        )));
    }
    let queued = rule_diff(base, head).map_err(missing)?;
    let earlier = rule_diff(&carried.base, &carried.head).map_err(missing)?;
    if queued != earlier {
        let paths = |diff: &[Change]| -> Vec<String> {
            diff.iter().map(|(path, _, _)| path.clone()).collect()
        };
        return Err((
            Reason::CarriedDiff,
            format!(
                "the carried mutation evidence observed another rule-path diff than the queued \
             merge's (observed {:?}, queued {:?}, or the same paths with other text), as when \
             another rule PR that touches these files merged first. Push again, rebased onto \
             the new main, so that the ratchet observes this PR on its new base, then queue it \
             again; re-running the workflow does not help, because it observes the same merge",
                paths(&earlier),
                paths(&queued)
            ),
        ));
    }
    Ok(format!(
        "Mutation evidence carried over from the observation of {} (pull request head {}): its \
         rule-path diff of {} path(s) is the queued merge's (ADR-0022, amending ADR-0009).",
        if observed.commit.is_empty() { "an earlier run" } else { &observed.commit },
        if observed.pr_head.is_empty() { "not recorded" } else { &observed.pr_head },
        queued.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::{Carried, Reason, check, rule_diff};
    use crate::ratchet::observations::Observations;

    /// Writes `files` into `root`, relative paths and texts.
    fn tree(scratch: &crate::tidy::scratch::Scratch, root: &str, files: &[(&str, &str)]) {
        for (path, text) in files {
            scratch.write(&format!("{root}/{path}"), text);
        }
    }

    // Goal: carried evidence counts only for the identical rule-path diff: the same changed
    // rule paths, each with the same text on both sides, whatever changed outside them. Another
    // rule-path change, other text on either side, or another pull request head is no verdict.
    #[test]
    fn carried_evidence_needs_the_identical_rule_path_diff() {
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-carry");
        let rule = "xtask/src/tidy/text.rs";
        let base = [(rule, "fn a() {}\n"), ("xtask/src/tidy.rs", "core\n")];
        let head = [(rule, "fn a() {}\nfn b() {}\n"), ("xtask/src/tidy.rs", "core\n")];
        tree(&scratch, "carried/base", &base);
        tree(&scratch, "carried/head", &head);
        tree(&scratch, "queue/base", &[(rule, "fn a() {}\n"), ("xtask/src/tidy.rs", "newer\n")]);
        tree(&scratch, "queue/head", &[(rule, "fn a() {}\nfn b() {}\n"), ("crates/x.rs", "y\n")]);
        let root = scratch.root();
        let observed_tree =
            crate::ratchet::tree::fingerprint(&root.join("carried/head")).unwrap().0;
        let observations = Observations {
            pr_head: "abc".to_owned(),
            head: observed_tree,
            ..Observations::default()
        };
        let mut carried = Carried {
            observations,
            base: root.join("carried/base"),
            head: root.join("carried/head"),
        };
        let (queue_base, queue_head) = (root.join("queue/base"), root.join("queue/head"));
        let note = check(&queue_base, &queue_head, &carried, Some("abc")).unwrap();
        assert!(note.contains("rule-path diff of 1 path(s) is the queued merge's"), "{note}");
        let refused = |evidence: &Carried, pr_head: Option<&str>, why: Reason| {
            let (kind, text) = check(&queue_base, &queue_head, evidence, pr_head).unwrap_err();
            assert_eq!(kind, why, "{text}");
            text
        };
        let other = refused(&carried, Some("def"), Reason::Evidence);
        assert!(other.contains("observed the pull request head `abc`, not `def`"), "{other}");
        carried.observations.skipped = true;
        let skipped = refused(&carried, None, Reason::Evidence);
        assert!(skipped.contains("skipped the head"), "{skipped}");
        carried.observations.skipped = false;
        scratch.write("carried/head/docs/later.md", "x\n");
        let moved = refused(&carried, None, Reason::Evidence);
        assert!(moved.contains("is of another head tree"), "{moved}");
        std::fs::remove_file(root.join("carried/head/docs/later.md")).unwrap();
        scratch.write(&format!("queue/head/{rule}"), "fn a() {}\nfn c() {}\n");
        let text = refused(&carried, None, Reason::CarriedDiff);
        assert!(text.contains("another rule-path diff"), "{text}");
        assert!(text.contains("Push again, rebased onto the new main"), "{text}");
        assert!(text.contains("re-running the workflow does not help"), "{text}");
        scratch.write(&format!("queue/head/{rule}"), "fn a() {}\nfn b() {}\n");
        scratch.write("queue/head/lints/ui/new.rs", "fn main() {}\n");
        let paths = refused(&carried, None, Reason::CarriedDiff);
        assert!(paths.contains("\"lints/ui/new.rs\""), "{paths}");
        assert_eq!(rule_diff(&queue_base, &queue_base).unwrap(), Vec::new(), "no change");
    }

    // Goal: a carried directory needs its observations and both trees.
    #[test]
    fn a_carried_directory_is_read_whole() {
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-carry-read");
        scratch.write(super::OBSERVATIONS, &Observations::default().to_json());
        let missing = Carried::read(scratch.root()).unwrap_err();
        assert!(missing.contains("lacks the trees `base/` and `head/`"), "{missing}");
        scratch.write("base/a", "x");
        assert!(Carried::read(scratch.root()).is_err(), "the base without the head");
        scratch.write("head/a", "y");
        let carried = Carried::read(scratch.root()).unwrap();
        assert_eq!(carried.observations, Observations::default());
        let empty = crate::tidy::scratch::Scratch::new("ratchet-carry-none");
        assert!(Carried::read(empty.root()).is_err(), "no observations");
    }
}
