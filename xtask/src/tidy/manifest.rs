//! Rules about manifests: runtime dependencies and their overrides (ADR-0001).
//!
//! xtask has no TOML parser (ADR-0001), so manifests are read line by line. That is enough for
//! the way manifests are written here. Where a line is ambiguous the rule reports it, so that an
//! unusual spelling cannot hide a dependency.

use super::Violation;
use std::fs;
use std::io;
use std::path::Path;

/// The rule that the dependency tables are checked against.
const RULE: &str = "runtime-dependency";

/// The rule that reports `[patch]` and `[replace]` in the workspace manifest.
const OVERRIDE_RULE: &str = "dependency-override";

/// Top-level keys of the workspace manifest that make a dependency resolve to other source
/// code than its name and version say.
const OVERRIDE_KEYS: [&str; 2] = ["patch", "replace"];

/// Tables whose entries are linked into the crate or its build script. Dev-dependencies are
/// governed by the cargo-deny allow-list instead.
const RUNTIME_TABLES: [&str; 2] = ["dependencies", "build-dependencies"];

/// The key that makes a dependency resolve to a crate other than the one its name says.
const RENAME_KEY: &str = "package";

/// Lint levels that fail the gate. A warning counts, because `just` and CI deny warnings
/// (ADR-0006).
const ENFORCED_LEVELS: [&str; 3] = ["warn", "deny", "forbid"];

/// What a `[header]` says about the lines that follow it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Section {
    /// `[dependencies]`, `[build-dependencies]` and their `[target.<x>.…]` forms: every key
    /// names a dependency.
    Table,
    /// `[dependencies.NAME]` and its build and target forms: the keys describe `NAME`.
    Entry(String),
    /// Anything else, dev-dependencies included.
    Other,
}

/// The names of the crates under `crates/`, sorted. A workspace crate is named after its
/// directory (ADR-0015).
pub fn workspace_crates(root: &Path) -> io::Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(root.join("crates"))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    names.sort();
    assert!(names.iter().all(|name| !name.is_empty()), "crate directories have names");
    Ok(names)
}

/// Checks the normal and build dependencies of one crate manifest: each is `libm` or a
/// workspace crate, and none is renamed.
pub fn check_manifest(relative: &Path, text: &str, workspace_crates: &[String]) -> Vec<Violation> {
    let is_allowed = |name: &str| name == "libm" || workspace_crates.iter().any(|ok| ok == name);
    let mut violations = Vec::new();
    let mut section = Section::Other;
    for (line_index, line) in text.lines().enumerate() {
        let line_number = u32::try_from(line_index + 1).expect("manifest has < 2^32 lines");
        let line = line.trim();
        let mut report = |message: String| {
            violations.push(Violation::new(relative, line_number, RULE, message));
        };
        if line.starts_with('[') {
            section = section_of(line, &RUNTIME_TABLES);
            if let Section::Entry(name) = &section
                && !is_allowed(name)
            {
                report(disallowed(name));
            }
        } else if line.is_empty() || line.starts_with('#') {
            // Blank lines and comments say nothing about dependencies.
        } else if section == Section::Table {
            report_table_line(line, &is_allowed, &mut report);
        } else if let Section::Entry(name) = &section
            && key_of(line).is_some_and(|key| key.first().is_some_and(|key| key == RENAME_KEY))
        {
            report(renamed(name));
        }
    }
    violations
}

/// Checks the workspace manifest. Crates inherit `[workspace.dependencies]` with
/// `name.workspace = true`, so a rename there would hide a crate from [`check_manifest`]. A
/// `[patch]` or `[replace]` there would swap the source of `libm` under its own name.
pub fn check_workspace_manifest(relative: &Path, text: &str) -> Vec<Violation> {
    let mut violations = check_overrides(relative, text);
    let mut section = Section::Other;
    for (line_index, line) in text.lines().enumerate() {
        let line_number = u32::try_from(line_index + 1).expect("manifest has < 2^32 lines");
        let line = line.trim();
        let mut report = |message: String| {
            violations.push(Violation::new(relative, line_number, RULE, message));
        };
        if line.starts_with('[') {
            let segments = header_segments(line);
            let is_workspace = segments.first().is_some_and(|first| first == "workspace");
            section =
                if is_workspace { section_of(line, &["dependencies"]) } else { Section::Other };
        } else if line.is_empty() || line.starts_with('#') {
            // Blank lines and comments say nothing about dependencies.
        } else if section == Section::Table {
            report_table_line(line, &|_name: &str| true, &mut report);
        } else if let Section::Entry(name) = &section
            && key_of(line).is_some_and(|key| key.first().is_some_and(|key| key == RENAME_KEY))
        {
            report(renamed(name));
        }
    }
    violations
}

/// Reports every `[patch.<source>]` and `[replace]` table of the workspace manifest, and the
/// same keys written as dotted or inline keys in its root table. Cargo applies only the root
/// manifest's overrides, so crate manifests are not read.
///
/// `cargo deny check sources` rejects a git override but accepts a path override, because a
/// path dependency has no source to check. So the rule bans both, and does not rely on it.
///
/// Tidy does not decode TOML escapes, so `"patch"` would read as another key. A top-level
/// key with a backslash is reported too, because it could spell an override.
fn check_overrides(relative: &Path, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut in_root_table = true;
    for (line_index, line) in text.lines().enumerate() {
        let line_number = u32::try_from(line_index + 1).expect("manifest has < 2^32 lines");
        let line = line.trim();
        let key = if line.starts_with('[') {
            in_root_table = false;
            header_segments(line).into_iter().next()
        } else if in_root_table && !line.starts_with('#') {
            key_of(line).and_then(|segments| segments.into_iter().next())
        } else {
            None
        };
        let message = match key {
            Some(key) if OVERRIDE_KEYS.contains(&key.as_str()) => Some(overridden(&key)),
            Some(key) if key.contains('\\') => Some(escaped(&key)),
            Some(_) | None => None,
        };
        if let Some(message) = message {
            violations.push(Violation::new(relative, line_number, OVERRIDE_RULE, message));
        }
    }
    let line_count = text.lines().count();
    assert!(
        violations.is_sorted_by(|earlier, later| earlier.line < later.line),
        "at most one violation per line, in line order"
    );
    assert!(
        violations.iter().all(|violation| {
            usize::try_from(violation.line).is_ok_and(|line| (1..=line_count).contains(&line))
        }),
        "every reported line is a line of the manifest"
    );
    violations
}

fn escaped(key: &str) -> String {
    format!(
        "cannot read the top-level key `{key}` of the workspace manifest: it holds an escape, \
         which tidy does not decode, so it could spell `patch` or `replace`. Write the key \
         without escapes (ADR-0001)"
    )
}

fn overridden(key: &str) -> String {
    format!(
        "`[{key}]` in the workspace manifest swaps a dependency's source under its own name, \
         and `cargo deny` does not see a path override; remove it and depend on the released \
         crate. Changing `libm` needs an ADR approved by a human (ADR-0001)"
    )
}

/// The lints that the table `[<table>]` of a manifest switches on: those set to `warn`, `deny`
/// or `forbid`, as a plain string or as `{ level = "..." }`. A lint set to `allow` is not listed.
pub fn enforced_lints(manifest: &str, table: &[&str]) -> Vec<String> {
    let mut lints = Vec::new();
    let mut in_table = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_table = header_segments(line) == table;
        } else if in_table
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            let segments = key_segments(key);
            let level = match segments.get(1).map(String::as_str) {
                None => lint_level(value),
                Some("level") => quoted(value),
                Some(_) => None,
            };
            if let Some(name) = segments.first()
                && level.is_some_and(|level| ENFORCED_LEVELS.contains(&level))
            {
                lints.push(name.clone());
            }
        }
    }
    lints
}

/// The level in the value of a lint: `"deny"` or `{ level = "deny", priority = -1 }`.
fn lint_level(value: &str) -> Option<&str> {
    if !value.trim_start().starts_with('{') {
        return quoted(value);
    }
    let mut settings = value.split(['{', ',', '}']).filter_map(|piece| piece.split_once('='));
    settings.find(|(key, _)| key.trim() == "level").and_then(|(_, level)| quoted(level))
}

/// The content of the double-quoted string that `value` starts with.
fn quoted(value: &str) -> Option<&str> {
    let mut parts = value.split('"');
    let before = parts.next()?;
    let content = parts.next()?;
    let _after = parts.next()?;
    before.trim().is_empty().then_some(content)
}

/// Reports what is wrong with one `name = ...` line of a dependency table.
fn report_table_line(
    line: &str,
    is_allowed: &impl Fn(&str) -> bool,
    report: &mut impl FnMut(String),
) {
    let Some((key, value)) = line.split_once('=') else {
        report(format!(
            "cannot read `{line}` as `name = ...`; write each dependency on one line, so that \
             tidy can check it (ADR-0001)"
        ));
        return;
    };
    let segments = key_segments(key);
    let name = segments.first().map_or("", String::as_str);
    if !is_allowed(name) {
        report(disallowed(name));
    }
    let renamed_by_dotted_key = segments.get(1).is_some_and(|sub_key| sub_key == RENAME_KEY);
    if renamed_by_dotted_key
        || inline_table_keys(value).iter().any(|inline_key| inline_key == RENAME_KEY)
    {
        report(renamed(name));
    }
}

fn disallowed(name: &str) -> String {
    format!(
        "runtime dependency `{name}`; the only runtime dependency allowed is `libm`. Adding a \
         crate needs an ADR approved by a human (ADR-0001)"
    )
}

fn renamed(name: &str) -> String {
    format!(
        "dependency `{name}` is renamed with `{RENAME_KEY} = \"...\"`; a rename hides the real \
         crate behind an allowed name. Depend on the crate under its own name (ADR-0001)"
    )
}

/// Classifies a `[header]` line. `tables` are the table names that hold dependencies.
fn section_of(header: &str, tables: &[&str]) -> Section {
    let segments = header_segments(header);
    let mut after_table = segments.iter().skip_while(|segment| !tables.contains(&segment.as_str()));
    match (after_table.next(), after_table.next()) {
        (Some(_table), Some(name)) => Section::Entry(name.clone()),
        (Some(_table), None) => Section::Table,
        (None, _) => Section::Other,
    }
}

/// The dotted key inside a `[header]` or `[[header]]`, split into its segments.
fn header_segments(header: &str) -> Vec<String> {
    let inner = header.trim_start_matches('[');
    let mut quote: Option<char> = None;
    let mut end = inner.len();
    for (index, character) in inner.char_indices() {
        if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && (character == '"' || character == '\'') {
            quote = Some(character);
        } else if quote.is_none() && character == ']' {
            end = index;
            break;
        }
    }
    key_segments(inner.get(..end).unwrap_or(inner))
}

/// The dotted key of a `key = value` line, split into its segments.
fn key_of(line: &str) -> Option<Vec<String>> {
    line.split_once('=').map(|(key, _value)| key_segments(key))
}

/// Splits a dotted TOML key at the dots outside quotes, and strips whitespace and quotes from
/// each segment: `target.'cfg(unix)'.dependencies` has three segments.
fn key_segments(key: &str) -> Vec<String> {
    let mut segments = vec![String::new()];
    let mut quote: Option<char> = None;
    for character in key.chars() {
        if quote == Some(character) {
            quote = None;
        } else if quote.is_none() && (character == '"' || character == '\'') {
            quote = Some(character);
        } else if quote.is_none() && character == '.' {
            segments.push(String::new());
        } else if let Some(segment) = segments.last_mut() {
            segment.push(character);
        }
    }
    segments.iter().map(|segment| segment.trim().to_owned()).collect()
}

/// The keys of an inline table `{ a = 1, b.c = 2 }`: here `a` and `b`. The split is on commas
/// and braces wherever they are, so a value may add keys that are not there; the rule that
/// reads them is a ban, which makes that the safe direction.
fn inline_table_keys(value: &str) -> Vec<String> {
    let pieces = value.split(['{', ',']);
    let keys = pieces.filter_map(key_of);
    keys.filter_map(|segments| segments.first().cloned()).collect()
}

#[cfg(test)]
mod tests {
    use super::{Section, check_manifest, check_workspace_manifest, workspace_crates};
    use super::{enforced_lints, header_segments, inline_table_keys, key_segments, section_of};
    use std::path::Path;

    /// No line is reported.
    const NONE: [u32; 0] = [];

    /// The line numbers that the rule reports in a crate manifest. `base` and `kernels` are
    /// the workspace crates.
    fn lines(manifest: &str) -> Vec<u32> {
        let workspace = ["base".to_owned(), "kernels".to_owned()];
        let violations = check_manifest(Path::new("crates/a/Cargo.toml"), manifest, &workspace);
        assert!(violations.iter().all(|violation| violation.rule == "runtime-dependency"));
        violations.iter().map(|violation| violation.line).collect()
    }

    // Goal: only libm and workspace crates may be runtime dependencies; dev-dependencies are
    // exempt. A crate that merely looks like a workspace crate (`base-extra`) is rejected.
    #[test]
    fn runtime_dependency_rule_fires() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\nlibm.workspace = true\n\
                        base = { path = \"../base\" }\nserde = \"1\"\nbase-extra = \"1\"\n\n\
                        [dev-dependencies]\nproptest = \"1\"\n";
        assert_eq!(lines(manifest), [7, 8]);
    }

    // Goal: build-dependencies and target-specific dependencies count as runtime, and a
    // comment after the header does not hide the section.
    #[test]
    fn build_and_target_dependencies_are_runtime() {
        let manifest =
            "[build-dependencies]\ncc = \"1\"\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n";
        assert_eq!(lines(manifest), [2, 4]);
        assert_eq!(
            lines("[target.x86_64-unknown-linux-gnu.build-dependencies]\ncc = \"1\"\n"),
            [2]
        );
        assert_eq!(lines("[dependencies] # the runtime ones\nserde = \"1\"\n"), [2]);
        assert_eq!(lines("[ dependencies ]\nserde = \"1\"\n"), [2]);
    }

    // Goal: a dependency written as its own table is recognised in every form: plain, build
    // and target-specific, with or without quotes. Dev-dependencies stay exempt.
    #[test]
    fn dependency_tables_are_recognised() {
        assert_eq!(lines("[dependencies.serde]\nversion = \"1\"\n"), [1]);
        assert_eq!(lines("[build-dependencies.cc]\nversion = \"1\"\n"), [1]);
        assert_eq!(lines("[target.'cfg(unix)'.dependencies.libc]\nversion = \"0.2\"\n"), [1]);
        assert_eq!(lines("[target.\"cfg(a.b)\".build-dependencies.cc]\nversion = \"1\"\n"), [1]);
        assert_eq!(lines("[dependencies.\"serde\"]\nversion = \"1\"\n"), [1]);
        assert_eq!(lines("[dependencies.libm]\nworkspace = true\n"), NONE);
        assert_eq!(lines("[dependencies.kernels]\npath = \"../kernels\"\n"), NONE);
        assert_eq!(lines("[dev-dependencies.proptest]\nversion = \"1\"\n"), NONE);
        assert_eq!(lines("[target.'cfg(unix)'.dev-dependencies.a]\nversion = \"1\"\n"), NONE);
    }

    // Goal: a rename is reported in every spelling, because the key then says nothing about
    // the crate that is linked. Dev-dependencies and other keys are not affected.
    #[test]
    fn renamed_dependencies_are_reported() {
        assert_eq!(lines("[dependencies]\nlibm = { package = \"serde\", version = \"1\" }\n"), [2]);
        assert_eq!(lines("[dependencies]\nlibm = { version = \"1\", package = \"serde\" }\n"), [2]);
        assert_eq!(lines("[dependencies]\nlibm = {package=\"serde\"}\n"), [2]);
        assert_eq!(lines("[dependencies]\nlibm = { \"package\" = \"serde\" }\n"), [2]);
        assert_eq!(lines("[dependencies]\nlibm.package = \"serde\"\n"), [2]);
        assert_eq!(lines("[dependencies.libm]\npackage = \"serde\"\nversion = \"1\"\n"), [2]);
        assert_eq!(lines("[build-dependencies.base]\n\"package\" = \"cc\"\n"), [2]);
        assert_eq!(lines("[dependencies]\nserde = { package = \"libm\" }\n"), [2, 2]);
        assert_eq!(lines("[dev-dependencies]\nlibm = { package = \"serde\" }\n"), NONE);
        assert_eq!(
            lines("[dependencies]\nlibm = { version = \"1\", features = [\"arch\"] }\n"),
            NONE
        );
        assert_eq!(lines("[package]\npackage = \"x\"\n"), NONE);
    }

    // Goal: a line that is not `name = ...` inside a dependency table is reported instead of
    // skipped, so that a dependency cannot hide in an unusual layout.
    #[test]
    fn unreadable_dependency_lines_are_reported() {
        let manifest = "[dependencies]\nlibm = { version = \"1\", features = [\n  \"arch\",\n] }\n";
        assert_eq!(lines(manifest), [3, 4]);
        assert_eq!(lines("[dependencies]\n\n# only libm\nlibm.workspace = true\n"), NONE);
    }

    // Goal: each message names the dependency, says what to do and cites the ADR.
    #[test]
    fn messages_name_the_dependency_and_the_fix() {
        let manifest = "[dependencies]\nserde = \"1\"\nlibm = { package = \"cc\" }\n\"arch\",\n";
        let violations = check_manifest(Path::new("crates/a/Cargo.toml"), manifest, &[]);
        let messages: Vec<&str> =
            violations.iter().map(|violation| violation.message.as_str()).collect();
        assert_eq!(messages.len(), 3, "a disallowed crate, a rename and an unreadable line");
        assert!(messages[0].starts_with("runtime dependency `serde`; the only runtime dependency"));
        assert!(messages[1].starts_with("dependency `libm` is renamed with `package = \"...\"`;"));
        assert!(messages[2].starts_with("cannot read `\"arch\",` as `name = ...`; write each"));
        assert!(messages.iter().all(|message| message.ends_with("(ADR-0001)")), "the reason");
    }

    // Goal: in the workspace manifest, a rename in `[workspace.dependencies]` is reported, in
    // the inline and in the table form. Names are not restricted there, and other tables are
    // not read.
    #[test]
    fn workspace_renames_are_reported() {
        let lines = |manifest: &str| -> Vec<u32> {
            let violations = check_workspace_manifest(Path::new("Cargo.toml"), manifest);
            violations.iter().map(|violation| violation.line).collect()
        };
        let good = "[workspace.dependencies]\nlibm = { version = \"=0.2.16\" }\nproptest = \"1\"\n";
        assert_eq!(lines(good), NONE);
        assert_eq!(lines("[workspace.dependencies]\nlibm = { package = \"serde\" }\n"), [2]);
        assert_eq!(lines("[workspace.dependencies.libm]\npackage = \"serde\"\n"), [2]);
        assert_eq!(lines("[workspace.package]\npackage = \"x\"\n"), NONE);
        assert_eq!(lines("[workspace.metadata.x]\nlibm = { package = \"serde\" }\n"), NONE);
        assert_eq!(lines("[dependencies]\nlibm = { package = \"serde\" }\n"), NONE);
    }

    // Goal: a `[patch]` or `[replace]` in the workspace manifest is reported in every spelling,
    // path and git alike, because cargo deny does not see a path override. Keys of the same
    // name inside other tables are not overrides.
    #[test]
    fn dependency_overrides_are_reported() {
        let lines = |manifest: &str| -> Vec<u32> {
            let violations = check_workspace_manifest(Path::new("Cargo.toml"), manifest);
            assert!(violations.iter().all(|violation| violation.rule == "dependency-override"));
            violations.iter().map(|violation| violation.line).collect()
        };
        assert_eq!(lines("[patch.crates-io]\nlibm = { path = \"../libm\" }\n"), [1]);
        assert_eq!(lines("[patch.'https://github.com/a/b']\nlibm = { git = \"x\" }\n"), [1]);
        assert_eq!(lines("[patch.crates-io.libm]\npath = \"../libm\"\n"), [1]);
        assert_eq!(lines("[replace]\n\"libm:0.2.16\" = { path = \"../libm\" }\n"), [1]);
        assert_eq!(lines("[ \"patch\" . crates-io ] # fork\nlibm = { path = \"x\" }\n"), [1]);
        assert_eq!(lines("patch.crates-io.libm = { path = \"x\" }\n[workspace]\n"), [1]);
        assert_eq!(lines("replace = { \"libm:0.2.16\" = { path = \"x\" } }\n"), [1]);
        assert_eq!(lines("# [patch.crates-io]\n[workspace]\nmembers = []\n"), NONE);
        assert_eq!(lines("[workspace.metadata.patch]\npatch = 1\nreplace = 2\n"), NONE);
        assert_eq!(lines("[workspace]\npatch.crates-io.libm = { path = \"x\" }\n"), NONE);
        // Escapes are not decoded, so an escaped top-level key is reported: it could spell
        // `patch`. An escape below the top level cannot.
        assert_eq!(lines("[\"p\\u0061tch\".crates-io]\nlibm = { path = \"x\" }\n"), [1]);
        assert_eq!(lines("\"p\\u0061tch\".crates-io.libm = { path = \"x\" }\n"), [1]);
        assert_eq!(lines("\"r\\u0065place\" = { \"libm:0.2.16\" = { path = \"x\" } }\n"), [1]);
        assert_eq!(lines("[workspace]\n\"p\\u0061tch\" = 1\n"), NONE);
        assert_eq!(lines("[workspace.metadata.\"a\\u0062\"]\n"), NONE);
        let message_of = |manifest: &str| -> String {
            let violations = check_workspace_manifest(Path::new("Cargo.toml"), manifest);
            violations.first().map_or(String::new(), |violation| violation.message.clone())
        };
        let escape = message_of("[\"p\\u0061tch\"]\n");
        assert!(escape.starts_with("cannot read the top-level key `p\\u0061tch`"), "{escape}");
        assert!(escape.ends_with("(ADR-0001)"), "the message cites the reason");
        let replace = message_of("[replace]\n");
        assert!(replace.starts_with("`[replace]` in the workspace manifest swaps"), "{replace}");
        assert!(replace.ends_with("(ADR-0001)"), "the message cites the reason");
    }

    // Goal: headers and keys split at dots outside quotes; sections follow from the segments.
    #[test]
    fn headers_and_keys_are_split() {
        assert_eq!(key_segments("libm.workspace "), ["libm", "workspace"]);
        assert_eq!(key_segments(" \"a.b\" . c"), ["a.b", "c"]);
        assert_eq!(
            header_segments("[target.'cfg(a.b)'.dependencies]"),
            ["target", "cfg(a.b)", "dependencies"]
        );
        assert_eq!(header_segments("[[bin]]"), ["bin"]);
        assert_eq!(header_segments("[a.']'.b] # ]"), ["a", "]", "b"]);
        let tables = ["dependencies", "build-dependencies"];
        assert_eq!(section_of("[dependencies]", &tables), Section::Table);
        assert_eq!(section_of("[dependencies.a]", &tables), Section::Entry("a".to_owned()));
        assert_eq!(section_of("[package.metadata.dependencies]", &tables), Section::Table);
        assert_eq!(section_of("[dev-dependencies]", &tables), Section::Other);
        assert_eq!(section_of("[package]", &tables), Section::Other);
        assert_eq!(inline_table_keys(" { a = 1, b.c = 2, \"d\" = 3 }"), ["a", "b", "d"]);
        assert_eq!(inline_table_keys(" \"1\""), Vec::<String>::new());
    }

    // Goal: a lint counts as switched on when its level is warn, deny or forbid, in the plain,
    // inline-table and dotted forms. `allow`, other tables and comments do not count.
    #[test]
    fn enforced_lints_are_read_from_one_table() {
        let manifest = "[workspace.lints.rust]\nunsafe_code = \"deny\"\n\n\
                        [workspace.lints.clippy]\n# a_comment = \"deny\"\n\
                        pedantic = { level = \"warn\", priority = -1 }\n\
                        unwrap_used = \"deny\" # no unwrap\nfloat_cmp = \"forbid\"\n\
                        similar_names = \"allow\"\nquiet = { level = \"allow\" }\n\
                        dotted.level = \"warn\"\ndotted.priority = 1\nbare = deny\n\
                        unlevelled = { priority = \"warn\" }\n\n[lints]\nworkspace = true\n";
        let clippy = enforced_lints(manifest, &["workspace", "lints", "clippy"]);
        assert_eq!(clippy, ["pedantic", "unwrap_used", "float_cmp", "dotted"]);
        assert_eq!(enforced_lints(manifest, &["workspace", "lints", "rust"]), ["unsafe_code"]);
        assert_eq!(enforced_lints(manifest, &["workspace", "lints"]), Vec::<String>::new());
        assert_eq!(enforced_lints(manifest, &["lints"]), Vec::<String>::new());
    }

    // Goal: the workspace crates are the directory names under `crates/`, in sorted order.
    #[test]
    fn workspace_crates_are_the_crate_directories() {
        let names = workspace_crates(&crate::workspace_root()).unwrap();
        assert!(names.is_sorted(), "names are sorted");
        assert!(names.iter().any(|name| name == "kernels"), "kernels is a workspace crate");
        assert!(!names.iter().any(|name| name == "xtask"), "xtask is not under crates/");
    }
}
