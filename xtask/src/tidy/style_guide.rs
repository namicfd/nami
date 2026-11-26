//! The mechanism tags of the style guide (`docs/TIGER_STYLE.md`).
//!
//! Every rule in the style guide ends with tags in backticks, such as `[tidy:usize]` or
//! `[review]`, that name what enforces it. The rule `doc-tag` checks each tag against the tree:
//! a tag may claim only what the tooling does today. That makes the tags a snapshot of the lint
//! table too: a lint that a tag names cannot be removed or set to `allow` without tidy failing.
//!
//! The legend above the first `## ` heading describes the forms with placeholders, so tags are
//! read from that heading on.
//!
//! A rule in the style guide states the rule, its reason and its tag, with an example where one
//! helps. What a mechanism sees and misses lives in the registry (a rule's `detail`) or in a
//! lint's `### What it does` doc, and `docs/RULES.md` shows it. Then tightening a rule changes
//! only the rule paths, not the code-owned style guide (issue #53). The rule `doc-tool-name`
//! holds that line: rule prose that names a checking tool is describing the tool.

use super::{Violation, lints, manifest, registry};
use std::fs;
use std::io;
use std::path::Path;

// Which file is the style guide decides what `doc-tag` checks, so it is core (ADR-0022, pending).
pub use super::STYLE_GUIDE;

/// The tag of a rule that no tool checks.
const REVIEW: &str = "review";

/// Tags that stand alone.
const PLAIN_MECHANISMS: [&str; 4] = ["compiler", "fmt", "deny", REVIEW];

/// Tags that name a rule, a lint or a planned mechanism after a colon.
const NAMED_MECHANISMS: [&str; 5] = ["tidy", "clippy", "rustc", "dylint", "planned"];

/// Former tags, each with what to write now.
const RETIRED_MECHANISMS: [(&str, &str); 2] = [
    ("hook", "hooks only give feedback and gate nothing; name what gates the rule, or `[review]`"),
    (
        "reference",
        "the reference suite is not built yet; write the `[planned:<id>]` that covers it",
    ),
];

/// Where the reason for the tag rules is written down.
const REFERENCE: &str = "(TIGER_STYLE.md, How rules are enforced)";

/// Words and phrases, in lower case, that name a checking tool. Rule prose that names one is
/// describing what the tool sees, which belongs in the registry (`doc-tool-name`). `just` is
/// left out: it is an everyday English word.
const TOOL_NAMES: [&str; 13] = [
    "tidy",
    "clippy",
    "dylint",
    "rustfmt",
    "rustc",
    "lints",
    "nextest",
    "miri",
    "cargo-deny",
    "cargo deny",
    "cargo-mutants",
    "cargo mutants",
    "the lint",
];

/// One mechanism tag in the style guide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag<'a> {
    /// One-based line number.
    pub line: u32,
    /// The text between `[` and `]`, such as `tidy:usize`.
    pub content: &'a str,
}

/// What exists in the tree for a tag to name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mechanisms {
    /// Lints switched on under `[workspace.lints.clippy]`.
    pub clippy: Vec<String>,
    /// Lints switched on under `[workspace.lints.rust]`.
    pub rustc: Vec<String>,
    /// Lints that the dylint library declares at a level that fails the gate.
    pub dylint: Vec<String>,
}

impl Mechanisms {
    /// Reads the lint tables of the root manifest and the lint declarations in `lints/src/`.
    pub fn load(root: &Path) -> io::Result<Self> {
        let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
        let declared = lints::declared(root)?;
        let enforced = declared.into_iter().filter(lints::DylintLint::is_enforced);
        Ok(Self {
            clippy: manifest::enforced_lints(&manifest, &["workspace", "lints", "clippy"]),
            rustc: manifest::enforced_lints(&manifest, &["workspace", "lints", "rust"]),
            dylint: enforced.map(|lint| lint.name).collect(),
        })
    }
}

/// The mechanism tags in the rules of the style guide: every `[tag]` in backticks from the
/// first `## ` heading on, outside fenced code blocks.
pub fn tags(text: &str) -> Vec<Tag<'_>> {
    let mut found = Vec::new();
    for (line_number, line) in rule_lines(text) {
        let spans = line.split("`[").skip(1).filter_map(|piece| piece.split_once("]`"));
        let contents = spans.map(|(content, _rest)| content).filter(|content| is_tag(content));
        found.extend(contents.map(|content| Tag { line: line_number, content }));
    }
    found
}

/// The lines of the rules, with their one-based numbers: from the first `## ` heading on,
/// outside fenced code blocks and their fence lines.
fn rule_lines(text: &str) -> Vec<(u32, &str)> {
    let mut found = Vec::new();
    let mut in_rules = false;
    let mut in_code = false;
    for (line_index, line) in text.lines().enumerate() {
        in_rules |= line.starts_with("## ");
        let is_fence = line.trim_start().starts_with("```");
        in_code ^= is_fence;
        if !in_rules || in_code || is_fence {
            continue;
        }
        let line_number = u32::try_from(line_index + 1).expect("documents have < 2^32 lines");
        found.push((line_number, line));
    }
    let first_heading = text.lines().position(|line| line.starts_with("## "));
    let after_heading = |number: u32| {
        first_heading.is_some_and(|index| usize::try_from(number).is_ok_and(|line| line > index))
    };
    assert!(
        found.iter().all(|(number, _)| after_heading(*number)),
        "rule lines start at the first `## ` heading"
    );
    found
}

/// The number of `[review]` tags: rules that no tool checks.
pub fn review_count(text: &str) -> usize {
    tags(text).iter().filter(|tag| tag.content == REVIEW).count()
}

/// The rules of the style guide: `doc-tag`, every tag has a known form and names something
/// that exists, and `doc-tool-name` ([`check_tool_names`]).
pub fn check(relative: &Path, text: &str, mechanisms: &Mechanisms) -> Vec<Violation> {
    let mut violations = check_tool_names(relative, text);
    for tag in tags(text) {
        if let Some(problem) = problem(tag.content, mechanisms) {
            let message = format!("{problem} {REFERENCE}");
            violations.push(Violation::new(relative, tag.line, "doc-tag", message));
        }
    }
    violations
}

/// The rule `doc-tool-name`: the prose of a rule names no checking tool, and no code span in it
/// is a command of one. Other code spans, tags among them, are not prose. A line with an odd
/// number of backticks is reported, because a code span across lines would swap code and prose.
pub fn check_tool_names(relative: &Path, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (line_number, line) in rule_lines(text) {
        let lower = line.to_lowercase();
        if !lower.matches('`').count().is_multiple_of(2) {
            let message = format!(
                "this rule line has an odd number of backticks: a code span runs across lines or \
                 is not closed. Keep each code span on one line, so that code and prose can be \
                 told apart {REFERENCE}"
            );
            violations.push(Violation::new(relative, line_number, "doc-tool-name", message));
            continue;
        }
        let (prose, spans) = split_code_spans(&lower);
        let named = |name: &str| {
            contains_word(&prose, name) || spans.iter().any(|span| is_command_of(span, name))
        };
        for name in TOOL_NAMES.into_iter().filter(|name| named(name)) {
            let message = format!(
                "rule prose names the tool `{name}`; the tag already names the mechanism. Keep \
                 the rule, its reason and an example here, and put what the mechanism sees and \
                 misses in the `detail` of its row in xtask/src/tidy/registry.rs, or in the \
                 `### What it does` doc of a dylint lint, then run `cargo xtask rules --write` \
                 {REFERENCE}"
            );
            violations.push(Violation::new(relative, line_number, "doc-tool-name", message));
        }
    }
    violations
}

/// `line` split into its prose, with each code span replaced by a space, and the contents of
/// its code spans. The line has an even number of backticks, so they pair up.
fn split_code_spans(line: &str) -> (String, Vec<&str>) {
    assert!(
        line.matches('`').count().is_multiple_of(2),
        "only a line whose backticks pair up is split"
    );
    let mut prose = String::with_capacity(line.len());
    let mut spans = Vec::new();
    for (index, piece) in line.split('`').enumerate() {
        if index % 2 == 1 {
            spans.push(piece);
            prose.push(' ');
        } else {
            prose.push_str(piece);
        }
    }
    (prose, spans)
}

/// True if the code span `span` runs the tool `name`, alone or with arguments. Leading
/// `VAR=value` words, a runner (`just`, `cargo`, `cargo xtask`) and cargo's options
/// (`+nightly`, `--locked`) are skipped. `clippy.toml` and `clippy::float_cmp` do not run it.
fn is_command_of(span: &str, name: &str) -> bool {
    assert!(!name.is_empty(), "every span starts with the empty name");
    let is_assignment = |word: &&str| word.contains('=') && !word.starts_with('-');
    let words: Vec<&str> = span.split_whitespace().skip_while(is_assignment).collect();
    let mut candidates = vec![words.join(" ")];
    if let Some((runner, tool)) = runner_and_tool(&words) {
        candidates.push(tool.join(" "));
        if runner == "cargo" {
            candidates.push(format!("cargo {}", tool.join(" ")));
        }
    }
    candidates.iter().any(|text| {
        text.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

/// For a command run by `just` or `cargo`: the runner, and the words of the tool it runs, with
/// the options before the tool and an `xtask` between skipped. `None` for any other command.
fn runner_and_tool<'a>(words: &[&'a str]) -> Option<(&'a str, Vec<&'a str>)> {
    let is_option = |word: &&str| word.starts_with('-') || word.starts_with('+');
    let (runner, rest) =
        words.split_first().filter(|(word, _)| ["just", "cargo"].contains(*word))?;
    let tool: Vec<&str> = rest.iter().copied().skip_while(is_option).collect();
    let xtask = tool.split_first().filter(|(word, _)| *runner == "cargo" && **word == "xtask");
    let tool = xtask
        .map_or(tool.clone(), |(_, tail)| tail.iter().copied().skip_while(is_option).collect());
    Some((runner, tool))
}

/// True if `phrase` occurs in `text` as whole words: not inside a longer identifier or word. A
/// dash ends a word, so `clippy-clean` names `clippy`, and `cargo-deny` is one name.
fn contains_word(text: &str, phrase: &str) -> bool {
    assert!(!phrase.is_empty(), "an empty phrase is in every text");
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    text.match_indices(phrase).any(|(start, _)| {
        let before = text.get(..start).and_then(|head| head.chars().next_back());
        let after = text.get(start + phrase.len()..).and_then(|tail| tail.chars().next());
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// True if a backticked `[content]` is meant as a tag, not as code such as `[f64; NVAR]`: a
/// lowercase word, alone or followed by a colon, or anything that starts with a mechanism's
/// name. A misspelt tag such as `[tydy:usize]` or `[review needed]` is then reported, not
/// skipped.
fn is_tag(content: &str) -> bool {
    let head_length = content.chars().take_while(char::is_ascii_lowercase).count();
    let (head, rest) = content.split_at_checked(head_length).unwrap_or((content, ""));
    let is_mechanism = PLAIN_MECHANISMS.contains(&head)
        || NAMED_MECHANISMS.contains(&head)
        || RETIRED_MECHANISMS.iter().any(|(retired, _)| *retired == head);
    !head.is_empty() && (rest.is_empty() || rest.starts_with(':') || is_mechanism)
}

/// What is wrong with a tag, and how to fix it. `None` if the tag is valid.
fn problem(content: &str, mechanisms: &Mechanisms) -> Option<String> {
    let (mechanism, name) = match content.split_once(':') {
        Some((mechanism, name)) => (mechanism, Some(name)),
        None => (content, None),
    };
    if let Some((_, advice)) = RETIRED_MECHANISMS.iter().find(|(retired, _)| *retired == mechanism)
    {
        return Some(format!("tag `[{content}]` is no longer a mechanism: {advice}"));
    }
    if PLAIN_MECHANISMS.contains(&mechanism) {
        let advice = format!("tag `[{content}]`: `[{mechanism}]` takes no name; write it alone");
        return name.map(|_| advice);
    }
    if !NAMED_MECHANISMS.contains(&mechanism) {
        return Some(format!(
            "unknown tag `[{content}]`; the tags are `[tidy:<rule-id>]`, `[clippy:<lint>]`, \
             `[rustc:<lint>]`, `[dylint:<lint>]`, `[planned:<id>]`, `[compiler]`, `[fmt]`, \
             `[deny]` and `[review]`. If this is code, write it so that it does not read as a tag"
        ));
    }
    let Some(name) = name else {
        return Some(format!(
            "tag `[{mechanism}]` does not say which one; write `[{mechanism}:<name>]`, so that \
             tidy can check that it exists"
        ));
    };
    named_problem(mechanism, name, mechanisms)
}

/// Checks a `[mechanism:name]` tag against what exists.
fn named_problem(mechanism: &str, name: &str, mechanisms: &Mechanisms) -> Option<String> {
    let lists = |lints: &[String]| lints.iter().any(|lint| lint == name);
    let (exists, missing) = match mechanism {
        "tidy" => (registry::is_registered(name), "is not a tidy rule in docs/RULES.md".to_owned()),
        "planned" => (
            registry::is_planned(name),
            "is not in PLANNED in xtask/src/tidy/registry.rs; add it there with a task".to_owned(),
        ),
        "dylint" => (
            lists(&mechanisms.dylint),
            "is not declared at `Deny` in lints/src/, so `just dylint` does not enforce it"
                .to_owned(),
        ),
        "clippy" => (lists(&mechanisms.clippy), lint_table_advice("clippy")),
        "rustc" => (lists(&mechanisms.rustc), lint_table_advice("rust")),
        other => panic!("`{other}` is not a named mechanism"),
    };
    let advice = "A tag may claim only what the tooling does today: restore the mechanism, or \
                  retag the rule `[review]` or `[planned:<id>]`";
    (!exists).then(|| format!("tag `[{mechanism}:{name}]`: `{name}` {missing}. {advice}"))
}

fn lint_table_advice(table: &str) -> String {
    format!("is not set to `warn` or `deny` under `[workspace.lints.{table}]` in Cargo.toml")
}

#[cfg(test)]
mod tests {
    use super::{Mechanisms, Tag, check, check_tool_names, contains_word};
    use super::{is_command_of, is_tag, review_count, split_code_spans, tags};
    use crate::tidy::registry;
    use std::path::Path;

    /// A planned mechanism to name in tags. The tests do not depend on which one it is, so
    /// they survive the day it is built and leaves the table.
    fn planned_id() -> &'static str {
        registry::PLANNED.first().expect("something is planned").id
    }

    fn mechanisms() -> Mechanisms {
        Mechanisms {
            clippy: vec!["pedantic".to_owned(), "unwrap_used".to_owned()],
            rustc: vec!["missing_docs".to_owned()],
            dylint: vec!["nami_recursion".to_owned()],
        }
    }

    /// The messages that `doc-tag` reports for a rule line that carries `tags`. Tags are code
    /// spans, never prose, so no other rule may fire on that line.
    fn problems(tags: &str) -> Vec<String> {
        let text = format!("## Rules\n\n- A rule. {tags}\n");
        let violations = check(Path::new("docs/TIGER_STYLE.md"), &text, &mechanisms());
        assert!(violations.iter().all(|violation| violation.line == 3), "the tag's line");
        let rules: Vec<&str> = violations.iter().map(|violation| violation.rule).collect();
        assert!(!rules.contains(&"doc-tool-name"), "a tag is not tool prose: {rules:?}");
        assert!(rules.iter().all(|rule| *rule == "doc-tag"), "only doc-tag: {rules:?}");
        violations.into_iter().map(|violation| violation.message).collect()
    }

    // Goal: every valid form passes: plain tags, and named tags whose target exists.
    #[test]
    fn valid_tags_pass() {
        let valid = "`[compiler]` `[fmt]` `[deny]` `[review]` `[tidy:usize]` `[tidy:direct-libm]` \
                     `[clippy:unwrap_used]` `[clippy:pedantic]` `[rustc:missing_docs]` \
                     `[dylint:nami_recursion]`";
        assert_eq!(problems(valid), Vec::<String>::new());
        assert_eq!(problems(&format!("`[planned:{}]`", planned_id())), Vec::<String>::new());
    }

    // Goal: a named tag whose target does not exist is reported, with the place to look.
    #[test]
    fn tags_that_name_nothing_are_reported() {
        let missing = [
            ("`[tidy:no-such-rule]`", "is not a tidy rule"),
            ("`[planned:no-such-plan]`", "is not in PLANNED"),
            ("`[planned:usize]`", "is not in PLANNED"),
            ("`[dylint:nami_float_reduction]`", "is not declared at `Deny` in lints/src/"),
            ("`[clippy:float_cmp]`", "under `[workspace.lints.clippy]`"),
            ("`[clippy:missing_docs]`", "under `[workspace.lints.clippy]`"),
            ("`[rustc:unwrap_used]`", "under `[workspace.lints.rust]`"),
            ("`[tidy:]`", "is not a tidy rule"),
        ];
        for (tag, expected) in missing {
            let found = problems(tag);
            assert_eq!(found.len(), 1, "{tag}");
            assert!(found[0].contains(expected), "{tag}: {}", found[0]);
            assert!(found[0].ends_with("(TIGER_STYLE.md, How rules are enforced)"), "{tag}");
        }
        let planned_as_built = problems(&format!("`[tidy:{}]`", planned_id()));
        assert_eq!(planned_as_built.len(), 1, "a planned mechanism is not a tidy rule yet");
    }

    // Goal: tags of an unknown, bare, retired or misspelt form are reported, each with the fix.
    #[test]
    fn malformed_tags_are_reported() {
        let malformed = [
            ("`[tidy]`", "does not say which one; write `[tidy:<name>]`"),
            ("`[clippy]`", "does not say which one"),
            ("`[dylint]`", "does not say which one"),
            ("`[planned]`", "does not say which one"),
            ("`[hook]`", "no longer a mechanism: hooks only give feedback"),
            ("`[reference]`", "write the `[planned:<id>]` that covers it"),
            ("`[reference:euler]`", "no longer a mechanism"),
            ("`[review:later]`", "takes no name"),
            ("`[tydy:usize]`", "unknown tag `[tydy:usize]`"),
            ("`[reveiw]`", "unknown tag `[reveiw]`"),
            ("`[review needed]`", "unknown tag `[review needed]`"),
            ("`[tidy usize]`", "unknown tag `[tidy usize]`"),
            ("`[Tidy:usize]`", ""),
        ];
        for (tag, expected) in malformed {
            let found = problems(tag);
            let count = usize::from(!expected.is_empty());
            assert_eq!(found.len(), count, "{tag}: {found:?}");
            assert!(found.iter().all(|problem| problem.contains(expected)), "{tag}: {found:?}");
        }
        assert_eq!(problems("`[tidy]` `[hook]` `[review]`").len(), 2, "each tag is checked");
    }

    // Goal: code in backticks is not a tag; a lowercase word in brackets is.
    #[test]
    fn code_is_not_a_tag() {
        for code in ["f64; NVAR", "u8", "T", "0.0; N", "a, b", "workspace.lints.clippy", ""] {
            assert!(!is_tag(code), "`[{code}]` is code");
        }
        for tag in ["review", "tidy:usize", "tidy:", "bool", "hook", "review needed", "tidy usize"]
        {
            assert!(is_tag(tag), "`[{tag}]` reads as a tag");
        }
    }

    // Goal: tags are read from the first `## ` heading on, with their line numbers, and not
    // from the legend above it, from code blocks, or from brackets without backticks.
    #[test]
    fn tags_are_read_from_the_rules_only() {
        let text = "# Title\n\nLegend: `[tidy:<rule-id>]` and `[review]`.\n\n## Rules\n\n\
                    - One. `[review]` `[tidy:usize]`\n- Two, with `[f64; NVAR]` states.\n  \
                    `[review]`\n\n```rust\n// `[review]` in a code block\n```\n\n\
                    - Three [review] and `field[CellIndex]`. `[fmt]`\n";
        let expected = [
            Tag { line: 7, content: "review" },
            Tag { line: 7, content: "tidy:usize" },
            Tag { line: 9, content: "review" },
            Tag { line: 15, content: "fmt" },
        ];
        assert_eq!(tags(text), expected);
        assert_eq!(review_count(text), 2);
        assert_eq!(tags("`[review]` before any heading\n"), []);
        assert_eq!(review_count(""), 0);
    }

    /// The lines on which `doc-tool-name` reports a tool name in `text`, one per name.
    fn tool_names(text: &str) -> Vec<u32> {
        let violations = check_tool_names(Path::new("docs/TIGER_STYLE.md"), text);
        assert!(violations.iter().all(|violation| violation.rule == "doc-tool-name"));
        violations.into_iter().map(|violation| violation.line).collect()
    }

    // Goal: rule prose that names a checking tool is reported once per name and line, in any
    // case, and the message says where the text goes instead.
    #[test]
    fn tool_names_in_rule_prose_are_reported() {
        let both = check(Path::new("x.md"), "## R\n\n- Tidy sees it. `[tidy]`\n", &mechanisms());
        let fired: Vec<&str> = both.iter().map(|violation| violation.rule).collect();
        assert_eq!(fired, ["doc-tool-name", "doc-tag"], "the style guide runs both rules");
        let text = "## Rules\n\n- A rule. The lint sees `sum`. `[review]`\n- Tidy checks it.\n\
                    - Clippy's `float_cmp` and rustfmt.\n- `cargo deny` and cargo-deny.\n\
                    - The workspace lints, and dylint.\n\
                    - `cargo xtask tidy --policy`, rustc, nextest, Miri, `cargo mutants`.\n\
                    - `cargo clippy`, `cargo-mutants`, a clippy-clean and tidy-checked build.\n\
                    - `just dylint` and the rustfmt-default width.\n";
        assert_eq!(tool_names(text), [3, 4, 5, 5, 6, 6, 7, 7, 8, 8, 8, 8, 8, 9, 9, 9, 10, 10]);
        let violations = check_tool_names(Path::new("docs/TIGER_STYLE.md"), text);
        assert!(violations[0].message.contains("`the lint`"), "{}", violations[0].message);
        assert!(violations[0].message.contains("registry.rs"), "names where the text goes");
        assert!(violations[0].message.ends_with("(TIGER_STYLE.md, How rules are enforced)"));
    }

    // Goal: a rule line whose backticks do not pair up is reported once, whatever it names, so
    // that a code span across lines cannot swap code and prose on the next line.
    #[test]
    fn unpaired_backticks_are_reported() {
        let text = "## Rules\n\n- A span that opens `here and\n  closes` here; tidy.\n- `ok`.\n";
        assert_eq!(tool_names(text), [3, 4]);
        let violations = check_tool_names(Path::new("docs/TIGER_STYLE.md"), text);
        assert!(violations[1].message.contains("odd number of backticks"), "says what is wrong");
        assert!(violations[1].message.contains("on one line"), "says how to fix it");
    }

    // Goal: tags, the legend, code blocks, and words that merely contain a tool name are not
    // rule prose that names a tool.
    #[test]
    fn tags_code_and_longer_words_are_not_tool_names() {
        let fence = "```";
        let text = format!(
            "# Title\n\nTidy checks the tags.\n\n## Rules\n\n\
             - Never silence a lint. `[tidy:expect-budget]` `[clippy:pedantic]`\n\
             - Untidy code, `tidying`, `my_clippy` and `lints_x`. `[dylint:nami_recursion]`\n\
             - `clippy.toml`, `clippy::float_cmp`, `[Tidy:usize]` and `cargo fmt`.\n\
             {fence}rust\n// tidy and clippy in a code block\n{fence}\n"
        );
        assert_eq!(tool_names(&text), Vec::<u32>::new());
        assert_eq!(tool_names("- tidy before any heading\n"), Vec::<u32>::new());
    }

    // Goal: a line splits into prose, with a space for each code span, and the spans.
    #[test]
    fn code_spans_are_split_from_prose() {
        let tagged = ("A rule.    ".to_owned(), vec!["[tidy:usize]", "[review]"]);
        assert_eq!(split_code_spans("A rule. `[tidy:usize]` `[review]`"), tagged);
        let ends = ("  and  ".to_owned(), vec!["a", ""]);
        assert_eq!(split_code_spans("`a` and ``"), ends, "spans at either end, and empty");
        assert_eq!(split_code_spans(""), (String::new(), Vec::new()));
    }

    // Goal: a code span is a tool's command only when it runs the tool: directly, or after
    // environment assignments, a runner (`just`, `cargo`, `cargo xtask`) and cargo's options.
    #[test]
    fn commands_are_told_from_paths() {
        assert!(is_command_of("tidy", "tidy"), "the name alone");
        assert!(is_command_of(" cargo deny check ", "cargo deny"), "with arguments, trimmed");
        assert!(is_command_of("cargo xtask tidy", "tidy"), "through cargo xtask");
        assert!(is_command_of("cargo clippy --workspace", "clippy"), "through cargo");
        assert!(is_command_of("just dylint", "dylint"), "through just");
        assert!(is_command_of("just clippy", "clippy"), "another just recipe");
        assert!(is_command_of("cargo +nightly clippy", "clippy"), "after a toolchain");
        assert!(is_command_of("cargo --locked clippy", "clippy"), "after a flag");
        assert!(is_command_of("cargo -q xtask --locked tidy", "tidy"), "flags around xtask");
        assert!(is_command_of("cargo --locked deny check", "cargo deny"), "the cargo phrase");
        assert!(is_command_of("RUSTC_WRAPPER= cargo clippy", "clippy"), "after an empty variable");
        assert!(is_command_of("A=1 B=2 just dylint", "dylint"), "after two variables");
        assert!(is_command_of("cargo mutants --in-diff", "cargo mutants"), "a two-word name");
        assert!(!is_command_of("clippy.toml", "clippy"), "a file");
        assert!(!is_command_of("clippy::float_cmp", "clippy"), "a path");
        assert!(!is_command_of("tidying", "tidy"), "a longer word");
        assert!(!is_command_of("cargo fmt", "tidy"), "another tool");
        assert!(!is_command_of("just check", "tidy"), "another recipe");
        assert!(!is_command_of("deny check", "cargo deny"), "no runner, no cargo phrase");
        assert!(!is_command_of("just deny", "cargo deny"), "just is not cargo");
        assert!(!is_command_of("cargo xtask", "tidy"), "a runner without a tool");
        assert!(!is_command_of("x=tidy", "tidy"), "an assignment only");
        assert!(!is_command_of("make tidy", "tidy"), "another runner");
    }

    // Goal: a phrase matches only as whole words, at either end of the text too.
    #[test]
    fn phrases_match_whole_words() {
        assert!(contains_word("tidy", "tidy"), "the whole text");
        assert!(contains_word("so tidy checks", "tidy"), "between spaces");
        assert!(contains_word("clippy's lint", "clippy"), "before an apostrophe");
        assert!(contains_word("(tidy)", "tidy"), "between brackets");
        assert!(!contains_word("untidy", "tidy"), "the end of a word");
        assert!(!contains_word("tidying", "tidy"), "the start of a word");
        assert!(contains_word("a clippy-clean build", "clippy"), "a dash ends a word");
        assert!(contains_word("tidy-checked", "tidy"), "a compound names the tool");
        assert!(contains_word("run cargo-deny.", "cargo-deny"), "a dashed name is one name");
        assert!(contains_word("cargo-mutants", "cargo-mutants"), "the whole dashed name");
        assert!(!contains_word("cargo-mutantsx", "cargo-mutants"), "a letter continues it");
        assert!(!contains_word("my_tidy", "tidy"), "an underscore continues a word");
        assert!(contains_word("untidy and tidy", "tidy"), "a later match counts");
    }
}
