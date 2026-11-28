//! Where the base's cases are planted in the head tree: at their own virtual path, and at paths
//! drawn per run from the head tree, so that rule code cannot pass by recognising the planted
//! cases (ADR-0022, §6). This is hardening, not proof.
//!
//! A drawn path counts only if the base's own tidy reports the same `(rule, line)` pairs there
//! as at the case's virtual path. Two paths lie in the case's own scope: one overwrites a real
//! file of the tree, the other is a new file with a random name in a real directory. A firing
//! case also goes to a new file in each scope class of the tree ([`scope_placements`]): each
//! crate, each crate's tests, and `xtask`, `lints`, `tools` and `docs`, so that a rule narrowed
//! in code to spare one of them meets the case there (review of #57, round 3).

use super::tree::is_plain_relative;
use crate::tidy::Scope;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Candidate paths tried per kind of placement before giving up on that kind.
const TRIES_MAX: u32 = 48;

/// A small deterministic generator (splitmix64). The seed comes from the head's content in CI
/// ([`seed_of`]) and is fixed in tests (ADR-0009).
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A generator that starts at `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^ (mixed >> 31)
    }

    /// A number in `0..count`.
    ///
    /// # Panics
    ///
    /// Panics if `count` is zero.
    pub fn below(&mut self, count: usize) -> usize {
        assert!(count > 0, "a choice needs at least one option");
        let count = u64::try_from(count).expect("counts fit in 64 bits");
        usize::try_from(self.next_u64() % count).expect("the result is below a usize")
    }

    /// One of `items`, at random.
    ///
    /// # Panics
    ///
    /// Panics if `items` is empty.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let index = self.below(items.len());
        items.get(index).expect("the index is below the length")
    }

    /// A random file stem: `r` and eight hex digits.
    pub fn stem(&mut self) -> String {
        format!("r{:08x}", self.next_u64() & 0xffff_ffff)
    }
}

/// Mixed into every seed, so that placements differ from other uses of the fingerprint. It is
/// no secret: the observing job runs the head's code and holds none.
const SALT: &str = "nami-ratchet-placements-1";

/// The seed for the base corpus with the fingerprint `base` and the head `head`: its commit,
/// or the fingerprint of its tree when no commit is given. The same pair always gets the same
/// placements, so re-running the check cannot draw new ones, and the judge can draw them again
/// to check that every one was run.
#[must_use]
pub fn seed_of(base: &str, head: &str) -> u64 {
    assert!(!base.is_empty() && !head.is_empty(), "a base and a head to seed from");
    let salted = crate::dylint::fnv1a(crate::dylint::FNV_OFFSET, SALT.as_bytes());
    let with_base = crate::dylint::fnv1a(crate::dylint::fnv1a(salted, &[0]), base.as_bytes());
    crate::dylint::fnv1a(crate::dylint::fnv1a(with_base, &[0]), head.as_bytes())
}

/// The prefix of the header that names a fixture's virtual path.
const PATH_HEADER: &str = "// path:";

/// `text` with its `// path:` header, which would let rule code recognise a planted case,
/// replaced by a comment with a random name, so that no line moves.
#[must_use]
pub fn disguise(text: &str, rng: &mut Rng) -> String {
    let Some((first, rest)) = text.split_once('\n') else {
        return text.to_owned();
    };
    if !first.starts_with(PATH_HEADER) {
        return text.to_owned();
    }
    let disguised = format!("// {}\n{rest}", rng.stem());
    assert_eq!(disguised.lines().count(), text.lines().count(), "no line moves");
    disguised
}

/// The suffix of a file name that decides its kind: everything from the first `.`, or the
/// whole name if it has none.
fn suffix(name: &str) -> &str {
    name.find('.').map_or(name, |index| name.get(index..).unwrap_or(name))
}

/// The paths to plant one case at: its virtual path, then a real file of the tree and a new
/// file in a real directory, each where the base's tidy reports `expected` as well, and where
/// tidy's walk of the tree at `tree` reads it. `files` are that tree's files, relative to it.
#[must_use]
pub fn placements(
    base: &Path,
    tree: &Path,
    virtual_path: &Path,
    text: &str,
    expected: &BTreeSet<(String, u32)>,
    files: &[String],
    rng: &mut Rng,
) -> Vec<PathBuf> {
    assert!(is_plain_relative(virtual_path), "a case's path stays inside the tree");
    let name = virtual_path.file_name().map_or_default(|name| name.to_string_lossy());
    let wanted = suffix(&name).to_owned();
    let scope = Scope::of(virtual_path);
    let same_kind: Vec<PathBuf> = files
        .iter()
        .map(PathBuf::from)
        .filter(|file| file.file_name().is_some_and(|n| suffix(&n.to_string_lossy()) == wanted))
        .filter(|file| file != virtual_path && Scope::of(file) == scope)
        .filter(|file| crate::tidy::is_walked(tree, file))
        .collect();
    let new_files: Vec<PathBuf> = files
        .iter()
        .filter_map(|file| Path::new(file).parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|directory| directory.join(format!("{}{wanted}", rng.stem())))
        .filter(|file| wanted.starts_with('.') && Scope::of(file) == scope)
        .filter(|file| crate::tidy::is_walked(tree, file))
        .collect();
    let mut chosen = vec![virtual_path.to_path_buf()];
    for candidates in [same_kind, new_files] {
        if let Some(found) = first_that_holds(base, text, expected, &candidates, rng) {
            chosen.push(found);
        }
    }
    assert!(
        chosen.iter().all(|path| is_plain_relative(path) && Scope::of(path) == scope),
        "every placement stays inside the tree, in the case's scope"
    );
    chosen
}

/// The top-level directories outside `crates/` that are a scope class each.
const TOOLING_CLASSES: [&str; 4] = ["xtask", "lints", "tools", "docs"];

/// The scope class of a directory relative to the root, if it has one: each crate's code
/// (`crates/<name>`, `crates/kernels` among them), each crate's integration tests
/// (`crates/<name>/tests`), and `xtask`, `lints`, `tools` and `docs`. A firing case is planted
/// once in each, so that a rule narrowed in code to spare one crate, its tests or a tooling
/// directory meets a case there. A narrowing to a single file or a deeper directory may still
/// go unmet.
fn scope_class(directory: &Path) -> Option<String> {
    let parts: Vec<String> = directory
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let class = match parts.as_slice() {
        [top, name, tests, ..] if top == "crates" && tests == "tests" => {
            format!("crates/{name}/tests")
        }
        [top, name, ..] if top == "crates" => format!("crates/{name}"),
        [top, ..] if TOOLING_CLASSES.contains(&top.as_str()) => top.clone(),
        _ => return None,
    };
    assert!(directory.starts_with(&class), "a directory lies inside its class");
    Some(class)
}

/// One path for a firing case in each scope class of the tree at `tree` ([`scope_class`]): a
/// new file with a random name and the case's kind, in a directory of that class drawn from
/// `files`, where tidy's walk reads it and the base's tidy reports `expected`, the same
/// `(rule, line)` pairs as at the case's own path. A class where no drawn path holds gets none.
#[must_use]
pub fn scope_placements(
    base: &Path,
    tree: &Path,
    virtual_path: &Path,
    text: &str,
    expected: &BTreeSet<(String, u32)>,
    files: &[String],
    rng: &mut Rng,
) -> Vec<PathBuf> {
    let name = virtual_path.file_name().map_or_default(|name| name.to_string_lossy());
    let wanted = suffix(&name).to_owned();
    let mut classes: BTreeMap<String, BTreeSet<&Path>> = BTreeMap::new();
    for directory in files.iter().filter_map(|file| Path::new(file).parent()) {
        if let Some(class) = scope_class(directory) {
            let _new = classes.entry(class).or_default().insert(directory);
        }
    }
    let mut chosen = Vec::new();
    for directories in classes.values() {
        let candidates: Vec<PathBuf> = directories
            .iter()
            .map(|directory| directory.join(format!("{}{wanted}", rng.stem())))
            .filter(|file| wanted.starts_with('.') && crate::tidy::is_walked(tree, file))
            .collect();
        chosen.extend(first_that_holds(base, text, expected, &candidates, rng));
    }
    assert!(chosen.len() <= classes.len(), "at most one path a class");
    assert!(chosen.iter().all(|path| is_plain_relative(path)), "the paths stay inside the tree");
    chosen
}

/// A candidate, drawn at random up to [`TRIES_MAX`] times, where the base reports `expected`.
fn first_that_holds(
    base: &Path,
    text: &str,
    expected: &BTreeSet<(String, u32)>,
    candidates: &[PathBuf],
    rng: &mut Rng,
) -> Option<PathBuf> {
    if candidates.is_empty() {
        return None;
    }
    for _ in 0..TRIES_MAX {
        let candidate = rng.pick(candidates);
        let holds = is_plain_relative(candidate)
            && reports_at(base, candidate, text).as_ref() == Some(expected);
        if holds {
            return Some(candidate.clone());
        }
    }
    None
}

/// What the base's tidy reports on `text` at `relative`, as `(rule, line)` pairs.
#[must_use]
pub fn reports_at(base: &Path, relative: &Path, text: &str) -> Option<BTreeSet<(String, u32)>> {
    let violations = crate::tidy::check_single_file(base, relative, text).ok()?;
    Some(violations.iter().map(|violation| (violation.rule.to_owned(), violation.line)).collect())
}

#[cfg(test)]
mod tests {
    use super::{Rng, disguise, placements, reports_at, suffix};
    use std::path::Path;

    // Goal: the generator is deterministic for a seed, differs between seeds, and stays in range.
    #[test]
    fn the_generator_is_seeded() {
        let draw = |seed: u64| -> Vec<u64> {
            let mut rng = Rng::new(seed);
            (0..4).map(|_| rng.next_u64()).collect()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
        assert_eq!(Rng::new(0).next_u64(), 0xe220_a839_7b1d_cdaf, "splitmix64's first output");
        let mut rng = Rng::new(1);
        let draws: Vec<usize> = (0..100).map(|_| rng.below(3)).collect();
        assert!(draws.iter().all(|value| *value < 3), "in range");
        assert!((0..3).all(|value| draws.contains(&value)), "every value is drawn");
        assert_eq!(super::seed_of("ab", "00ff"), super::seed_of("ab", "00ff"), "one seed a pair");
        assert_ne!(super::seed_of("ab", "00ff"), super::seed_of("ab", "00fe"), "another head");
        assert_ne!(super::seed_of("ab", "00ff"), super::seed_of("ac", "00ff"), "another base");
        assert_eq!(rng.stem().len(), 9);
    }

    // Goal: the `// path:` header is replaced by a random comment on the same line, and a text
    // without one is kept.
    #[test]
    fn the_header_is_disguised() {
        let mut rng = Rng::new(5);
        let text = "// path: crates/a/src/x.rs\nfn a() {}\n";
        let disguised = disguise(text, &mut rng);
        assert!(
            !disguised.contains("path:") && disguised.ends_with("\nfn a() {}\n"),
            "{disguised}"
        );
        assert_ne!(disguised, disguise(text, &mut rng), "each placement draws its own");
        assert_eq!(disguise("fn a() {}\n", &mut rng), "fn a() {}\n");
        assert_eq!(disguise("// path: x", &mut rng), "// path: x", "no line after the header");
    }

    // Goal: a kind is the name from its first dot, or the whole name.
    #[test]
    fn suffixes_name_the_kind() {
        assert_eq!(suffix("example.rs"), ".rs");
        assert_eq!(suffix("update__pencil.snap.new"), ".snap.new");
        assert_eq!(suffix("Cargo.toml"), ".toml");
        assert_eq!(suffix("justfile"), "justfile");
    }

    // Goal: a case is planted at its own path, at a real file and at a new file, each in the
    // same scope and where the base's tidy reports the same; the draw follows the seed.
    #[test]
    fn placements_keep_what_the_base_reports() {
        let root = crate::workspace_root();
        let text = "// path: crates/solver/src/example.rs\nfn a() {\n    loop {}\n}\n";
        let path = Path::new("crates/solver/src/example.rs");
        let expected = reports_at(&root, path, text).unwrap();
        let files: Vec<String> = [
            "crates/kernels/src/reduce.rs",
            "crates/base/src/lib.rs",
            "xtask/src/hook.rs",
            "docs/a.md",
        ]
        .map(str::to_owned)
        .to_vec();
        let chosen = placements(&root, &root, path, text, &expected, &files, &mut Rng::new(3));
        assert_eq!(chosen.len(), 3, "{chosen:?}");
        assert_eq!(chosen[0], path);
        assert_eq!(chosen[1], Path::new("crates/base/src/lib.rs"), "{chosen:?}");
        assert!(
            chosen[2].starts_with("crates/base/src") || chosen[2].starts_with("crates/kernels/src")
        );
        assert_eq!(
            chosen,
            placements(&root, &root, path, text, &expected, &files, &mut Rng::new(3))
        );
        let tooling_only = ["xtask/src/hook.rs".to_owned()];
        let alone =
            placements(&root, &root, path, text, &expected, &tooling_only, &mut Rng::new(3));
        assert_eq!(alone, [path], "tooling is another scope");
    }

    // Goal: each crate, each crate's tests and each tooling directory is a class of its own,
    // and a firing case gets one path in every class where the base reports the same.
    #[test]
    fn a_firing_case_is_planted_in_every_class() {
        let class = |directory: &str| super::scope_class(Path::new(directory));
        assert_eq!(class("crates/solver/src/boundary"), Some("crates/solver".to_owned()));
        assert_eq!(class("crates/solver"), Some("crates/solver".to_owned()));
        assert_eq!(class("crates/solver/tests"), Some("crates/solver/tests".to_owned()));
        assert_eq!(class("crates/kernels/src"), Some("crates/kernels".to_owned()));
        assert_eq!(class("xtask/src/tidy"), Some("xtask".to_owned()));
        assert_eq!(class("docs/design"), Some("docs".to_owned()));
        assert_eq!(class("tools"), Some("tools".to_owned()));
        assert_eq!(class(".github/workflows"), None);
        assert_eq!(class("crates"), None);
        let root = crate::workspace_root();
        let text = "// path: crates/kernels/src/example.rs\nuse libm::sqrt;\n";
        let path = Path::new("crates/kernels/src/example.rs");
        let expected = reports_at(&root, path, text).unwrap();
        assert!(expected.iter().any(|(rule, _)| rule == "direct-libm"), "{expected:?}");
        let files: Vec<String> = [
            "crates/kernels/src/reduce.rs",
            "crates/solver/src/lib.rs",
            "crates/base/src/lib.rs",
            "crates/base/tests/index.rs",
            "xtask/src/hook.rs",
            ".github/workflows/a.rs",
        ]
        .map(str::to_owned)
        .to_vec();
        let draw = |seed| {
            let mut rng = Rng::new(seed);
            super::scope_placements(&root, &root, path, text, &expected, &files, &mut rng)
        };
        let chosen = draw(3);
        let prefixes =
            ["crates/base/src/", "crates/base/tests/", "crates/kernels/src/", "crates/solver/src/"];
        assert_eq!(chosen.len(), 5, "{chosen:?}");
        for (place, prefix) in chosen.iter().zip(prefixes.iter().chain(&["xtask/src/"])) {
            assert!(place.starts_with(prefix) && place.extension().is_some_and(|e| e == "rs"));
        }
        assert_eq!(chosen, draw(3), "the draw follows the seed");
        assert_ne!(chosen, draw(4), "and differs with it");
    }
}
