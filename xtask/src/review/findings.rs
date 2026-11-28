//! What the CI review's model returns, read as untrusted data (#59; ADR-0023, rule 4).
//!
//! The review job runs Claude Code on the pull request with read-only tools, and writes its
//! JSON result to a file. The model read the pull request, so everything it wrote is the pull
//! request's text as far as trust goes. This module reads that file strictly: the shape that
//! [`SCHEMA`] asks for, the reviewers it names, bounded lengths and counts. Anything else is
//! no verdict, never an approval. The text that is posted has its mentions broken, so that no
//! review notifies anyone ([`unmention`]).

use crate::json::{self, Value};

/// The reviewers that the CI review runs, as `.claude/agents/` names them.
pub const REVIEWERS: [&str; 2] = ["tigerstyle-reviewer", "numerics-verifier"];

/// Most blocking findings one review posts. A review with more is no verdict: the pull request
/// is too large to review in one run, or the model went wrong.
pub const BLOCKING_MAX: usize = 30;

/// Most suggestions one review lists.
const SUGGESTIONS_MAX: usize = 20;

/// Most characters of a finding's or a suggestion's text.
const TEXT_CHARS_MAX: usize = 4000;

/// Most characters of the review's summary.
const SUMMARY_CHARS_MAX: usize = 2000;

/// Most threads one review may mark as fixed: as many as the review gate reads on one page.
const FIXED_MAX: usize = 100;

/// The JSON schema that the review job passes to Claude Code (`--json-schema`), so that the
/// model's last answer has the shape that [`read_result`] reads.
pub const SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["summary", "reviewers_run", "blocking", "suggestions", "fixed_threads", "dropped"],
  "properties": {
    "summary": {"type": "string", "maxLength": 2000},
    "reviewers_run": {
      "type": "array",
      "items": {"enum": ["tigerstyle-reviewer", "numerics-verifier"]}
    },
    "blocking": {
      "type": "array",
      "maxItems": 30,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["reviewer", "path", "line", "body"],
        "properties": {
          "reviewer": {"enum": ["tigerstyle-reviewer", "numerics-verifier"]},
          "path": {"type": "string"},
          "line": {"type": "integer", "minimum": 1},
          "body": {"type": "string", "maxLength": 4000}
        }
      }
    },
    "suggestions": {
      "type": "array",
      "maxItems": 20,
      "items": {"type": "string", "maxLength": 4000}
    },
    "fixed_threads": {"type": "array", "maxItems": 100, "items": {"type": "string"}},
    "dropped": {"type": "integer", "minimum": 0}
  }
}"#;

/// One blocking finding, as the model placed it: a file of the pull request and a line of
/// that file in the pull request's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The reviewer that found it, one of [`REVIEWERS`].
    pub reviewer: String,
    /// The file, relative to the repository root.
    pub path: String,
    /// The line in the head's version of the file, from 1.
    pub line: u32,
    /// What is wrong, why it matters and what would fix it, with its mentions broken.
    pub body: String,
}

/// A review that ran to the end and answered in the schema's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Findings {
    /// One paragraph on what was reviewed, with its mentions broken.
    pub summary: String,
    /// The reviewers that ran, each once, in [`REVIEWERS`] order.
    pub reviewers_run: Vec<String>,
    /// The blocking findings: each becomes a review thread.
    pub blocking: Vec<Finding>,
    /// The non-blocking suggestions, listed in the review's body.
    pub suggestions: Vec<String>,
    /// The IDs of the reviewer's open threads that the new head fixes.
    pub fixed_threads: Vec<String>,
    /// How many of the subagents' findings the lead dropped as unconfirmed.
    pub dropped: u32,
}

/// What the review job's result says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The review ran, and these are its findings.
    Reviewed(Findings),
    /// The review gave no verdict, for this reason: nothing is approved.
    NoVerdict(String),
}

/// The review job's result: its outcome, and what it cost as Claude Code reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The findings, or why there are none.
    pub outcome: Outcome,
    /// The run's cost in US dollars, as the number text that Claude Code printed, or `unknown`.
    pub cost: String,
}

/// Reads Claude Code's JSON result (`--output-format json`). Every way it can be wrong is a
/// [`Outcome::NoVerdict`] that says what was wrong, never an error and never a panic: the
/// text is untrusted.
#[must_use]
pub fn read_result(text: &str) -> Answer {
    let parsed = json::parse(text);
    let Ok(value) = parsed else {
        let outcome = Outcome::NoVerdict("the review job's result is not JSON".to_owned());
        return Answer { outcome, cost: "unknown".to_owned() };
    };
    let cost = match value.get("total_cost_usd") {
        Some(Value::Number(number)) => number.clone(),
        _ => "unknown".to_owned(),
    };
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("missing");
    let failed = value.get("is_error").and_then(Value::as_bool) != Some(false);
    let outcome = if subtype == "error_max_budget_usd" {
        Outcome::NoVerdict("the review ran over its budget".to_owned())
    } else if subtype != "success" || failed {
        Outcome::NoVerdict(format!("the review stopped early (`{}`)", plain(subtype)))
    } else {
        match value.get("structured_output").map(findings_from) {
            Some(Ok(findings)) => Outcome::Reviewed(findings),
            Some(Err(reason)) => Outcome::NoVerdict(format!("the review's answer {reason}")),
            None => Outcome::NoVerdict("the review gave no structured answer".to_owned()),
        }
    };
    assert!(!cost.is_empty(), "a cost is named, if only as unknown");
    Answer { outcome, cost }
}

/// `text` if it is a short plain word, as Claude Code's `subtype` is; otherwise `other`, so
/// that no unexpected text reaches the review's body.
fn plain(text: &str) -> &str {
    let is_plain = !text.is_empty()
        && text.len() <= 40
        && text.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_');
    if is_plain { text } else { "other" }
}

/// The findings in `value`, in the shape of [`SCHEMA`]; or what is wrong with them, worded
/// to follow "the review's answer".
fn findings_from(value: &Value) -> Result<Findings, String> {
    let object = value.as_object().ok_or("is not an object")?;
    let known = ["summary", "reviewers_run", "blocking", "suggestions", "fixed_threads", "dropped"];
    if let Some((key, _)) = object.iter().find(|(key, _)| !known.contains(&key.as_str())) {
        return Err(format!("has an unknown field `{}`", plain(key)));
    }
    let summary = text_at(value, "summary", SUMMARY_CHARS_MAX)?;
    let reviewers_run = reviewers_from(value.get("reviewers_run"))?;
    let blocking = list(value, "blocking", BLOCKING_MAX)?
        .iter()
        .map(finding_from)
        .collect::<Result<Vec<_>, _>>()?;
    let suggestions = list(value, "suggestions", SUGGESTIONS_MAX)?
        .iter()
        .map(|item| bounded(item, "a suggestion", TEXT_CHARS_MAX))
        .collect::<Result<Vec<_>, _>>()?;
    let fixed_threads = list(value, "fixed_threads", FIXED_MAX)?
        .iter()
        .map(thread_id)
        .collect::<Result<Vec<_>, _>>()?;
    let dropped = count(value.get("dropped")).ok_or("has no `dropped` count")?;
    assert!(blocking.len() <= BLOCKING_MAX, "the blocking findings are bounded");
    Ok(Findings { summary, reviewers_run, blocking, suggestions, fixed_threads, dropped })
}

/// The array at `key`, with at most `maximum` items.
fn list<'v>(value: &'v Value, key: &str, maximum: usize) -> Result<&'v [Value], String> {
    let items = value.get(key).and_then(Value::as_array).ok_or(format!("has no `{key}` list"))?;
    if items.len() > maximum {
        return Err(format!("has {} items in `{key}`, more than {maximum}", items.len()));
    }
    Ok(items)
}

/// The reviewers that ran: known names, each once, in [`REVIEWERS`] order; at least one.
fn reviewers_from(value: Option<&Value>) -> Result<Vec<String>, String> {
    let names = value.and_then(Value::as_array).ok_or("has no `reviewers_run` list")?;
    let mut run = Vec::new();
    for reviewer in REVIEWERS {
        let times = names.iter().filter(|name| name.as_str() == Some(reviewer)).count();
        if times > 1 {
            return Err(format!("names `{reviewer}` more than once"));
        }
        if times == 1 {
            run.push(reviewer.to_owned());
        }
    }
    if run.len() != names.len() {
        return Err("names a reviewer that the CI review does not run".to_owned());
    }
    if !run.iter().any(|name| name == "tigerstyle-reviewer") {
        return Err("does not say that `tigerstyle-reviewer` ran, which every review runs".into());
    }
    Ok(run)
}

/// One blocking finding.
fn finding_from(value: &Value) -> Result<Finding, String> {
    let fields = value.as_object().ok_or("has a finding that is not an object")?;
    if fields.len() != 4 {
        return Err("has a finding with fields other than reviewer, path, line and body".into());
    }
    let reviewer = value.get("reviewer").and_then(Value::as_str).unwrap_or_default();
    if !REVIEWERS.contains(&reviewer) {
        return Err("has a finding by an unknown reviewer".to_owned());
    }
    let path = value.get("path").and_then(Value::as_str).unwrap_or_default();
    if !is_repository_path(path) {
        return Err("has a finding whose path is not a file in the repository".to_owned());
    }
    let line = count(value.get("line")).filter(|line| *line > 0);
    let line = line.ok_or("has a finding without a line from 1 on")?;
    let body = text_at(value, "body", TEXT_CHARS_MAX)?;
    if body.trim().is_empty() {
        return Err("has a finding with no text".to_owned());
    }
    Ok(Finding { reviewer: reviewer.to_owned(), path: path.to_owned(), line, body })
}

/// True for a relative path of plain segments: no `..`, no `.`, no empty segment, nothing that
/// starts with `/` and no control character. Whether the pull request changes it is checked
/// when the finding is placed.
fn is_repository_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 400
        && !path.chars().any(char::is_control)
        && path.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// A review thread's GraphQL node ID: short, and made of the characters GitHub uses.
fn thread_id(value: &Value) -> Result<String, String> {
    let id = value.as_str().ok_or("has a fixed thread that is not a string")?;
    let is_id = !id.is_empty()
        && id.len() <= 100
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if !is_id {
        return Err("has a fixed thread whose ID GitHub would not give".to_owned());
    }
    Ok(id.to_owned())
}

/// The string at `key`, bounded and with its mentions broken.
fn text_at(value: &Value, key: &str, maximum: usize) -> Result<String, String> {
    let text = value.get(key).ok_or(format!("has no `{key}`"))?;
    bounded(text, &format!("`{key}`"), maximum)
}

/// The string `value`, at most `maximum` characters, with its mentions broken; `what` names
/// it in the error.
fn bounded(value: &Value, what: &str, maximum: usize) -> Result<String, String> {
    let text = value.as_str().ok_or(format!("has {what} that is not a string"))?;
    let length = text.chars().count();
    if length > maximum {
        return Err(format!("has {what} of {length} characters, more than {maximum}"));
    }
    Ok(unmention(text))
}

/// A non-negative integer that fits `u32`.
fn count(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::Number(text)) => text.parse::<u32>().ok(),
        _ => None,
    }
}

/// `text` with every `@` outside code spans followed by a zero-width space, so that the
/// posted review notifies nobody: GitHub links `@name` and `@org/team` only when the name
/// follows the `@` directly. Code spans and fenced blocks are left as they are, because GitHub
/// does not link mentions there, and code must stay as written.
#[must_use]
pub fn unmention(text: &str) -> String {
    let mut broken = String::with_capacity(text.len());
    let mut in_code = false;
    for character in text.chars() {
        if character == '`' {
            in_code = !in_code;
        }
        broken.push(character);
        if character == '@' && !in_code {
            broken.push('\u{200B}');
        }
    }
    assert!(broken.len() >= text.len(), "nothing is removed");
    broken
}

#[cfg(test)]
mod tests {
    use super::{Outcome, read_result, unmention};

    /// A result as Claude Code prints it, with `answer` as its structured output.
    fn result(answer: &str) -> String {
        format!(
            r#"{{"type": "result", "subtype": "success", "is_error": false,
                "total_cost_usd": 0.4125, "structured_output": {answer}}}"#
        )
    }

    const CLEAN: &str = r#"{"summary": "Reviewed the diff.", "reviewers_run":
        ["tigerstyle-reviewer"], "blocking": [], "suggestions": ["Name it `x`."],
        "fixed_threads": ["PRRT_kwDO1"], "dropped": 2}"#;

    // Goal: a clean answer is read with every field, and its cost is kept as printed.
    #[test]
    fn a_clean_answer_is_read() {
        let read = read_result(&result(CLEAN));
        assert_eq!(read.cost, "0.4125");
        let Outcome::Reviewed(findings) = read.outcome else { panic!("reviewed: {read:?}") };
        assert_eq!(findings.reviewers_run, ["tigerstyle-reviewer"]);
        assert_eq!(findings.blocking, []);
        assert_eq!(findings.fixed_threads, ["PRRT_kwDO1"]);
        assert_eq!(findings.dropped, 2);
    }

    // Goal: a blocking finding is read with its place and reviewer, and its mentions broken.
    #[test]
    fn a_blocking_finding_is_read() {
        let answer = CLEAN.replace(
            r#""blocking": []"#,
            r#""blocking": [{"reviewer": "numerics-verifier", "path": "crates/kernels/src/a.rs",
                "line": 42, "body": "**Blocking**: ask @someone"}]"#,
        );
        let answer = answer.replace(
            r#"["tigerstyle-reviewer"]"#,
            r#"["numerics-verifier",
            "tigerstyle-reviewer"]"#,
        );
        let Outcome::Reviewed(findings) = read_result(&result(&answer)).outcome else {
            panic!("reviewed")
        };
        assert_eq!(findings.reviewers_run, ["tigerstyle-reviewer", "numerics-verifier"]);
        let finding = &findings.blocking[0];
        assert_eq!((finding.path.as_str(), finding.line), ("crates/kernels/src/a.rs", 42));
        assert_eq!(finding.body, "**Blocking**: ask @\u{200B}someone");
    }

    // Goal: every way the result can be wrong is no verdict, never a review: over budget,
    // stopped, not JSON, no structured output, a missing or unknown field, an unknown
    // reviewer, a path out of the repository, line 0, an empty body, too many findings, a
    // thread ID with other characters, and a review without `tigerstyle-reviewer`.
    #[test]
    fn a_wrong_result_is_no_verdict() {
        let finding = |path: &str, line: &str, body: &str| {
            CLEAN.replace(
                r#""blocking": []"#,
                &format!(
                    r#""blocking": [{{"reviewer": "tigerstyle-reviewer", "path": "{path}",
                    "line": {line}, "body": "{body}"}}]"#
                ),
            )
        };
        let many = format!(
            r#""blocking": [{}]"#,
            vec![r#"{"reviewer": "tigerstyle-reviewer", "path": "a", "line": 1, "body": "b"}"#; 31]
                .join(",")
        );
        let cases = [
            r#"{"subtype": "error_max_budget_usd", "is_error": true}"#.to_owned(),
            r#"{"subtype": "error_during_execution", "is_error": true}"#.to_owned(),
            r#"{"subtype": "success", "is_error": false}"#.to_owned(),
            "not json".to_owned(),
            result("[]"),
            result(&CLEAN.replace(r#", "dropped": 2"#, "")),
            result(&CLEAN.replace(r#""dropped": 2"#, r#""dropped": 2, "approve": true"#)),
            result(&CLEAN.replace("tigerstyle-reviewer", "other-reviewer")),
            result(&CLEAN.replace(r#"["tigerstyle-reviewer"]"#, r#"["numerics-verifier"]"#)),
            result(&finding("../etc/passwd", "1", "x")),
            result(&finding("/abs", "1", "x")),
            result(&finding("a.rs", "0", "x")),
            result(&finding("a.rs", "1", " ")),
            result(&CLEAN.replace(r#""blocking": []"#, &many)),
            result(&CLEAN.replace("PRRT_kwDO1", "PRRT kw/DO1")),
        ];
        for case in cases {
            let read = read_result(&case);
            assert!(matches!(read.outcome, Outcome::NoVerdict(_)), "{case}: {read:?}");
        }
    }

    // Goal: the reason for no verdict never quotes the model's text beyond a plain subtype.
    #[test]
    fn a_reason_quotes_no_model_text() {
        let odd = r#"{"subtype": "@everyone look", "is_error": true}"#;
        let Outcome::NoVerdict(reason) = read_result(odd).outcome else { panic!("no verdict") };
        assert_eq!(reason, "the review stopped early (`other`)");
        let key = result(&CLEAN.replace(r#""dropped""#, r#""@x""#));
        let Outcome::NoVerdict(field) = read_result(&key).outcome else { panic!("no verdict") };
        assert_eq!(field, "the review's answer has an unknown field `other`");
    }

    // Goal: mentions outside code are broken; code spans keep their `@` as written.
    #[test]
    fn mentions_are_broken_outside_code() {
        assert_eq!(unmention("@a and `x @ 1` and @b/c"), "@\u{200B}a and `x @ 1` and @\u{200B}b/c");
        assert_eq!(unmention("no mention"), "no mention");
    }
}
