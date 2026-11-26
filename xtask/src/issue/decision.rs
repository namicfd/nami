//! The decision record: how the bot shows that the maintainer decided an issue (ADR-0021,
//! point 7).
//!
//! The maintainer often decides in a conversation with a session, not on GitHub. The session
//! then posts a record in this fixed form and removes `needs-decision`:
//!
//! ```text
//! <!-- nami:decision -->
//! Decision: <one paragraph, on one line>
//! Decided by: @<login>, <YYYY-MM-DD>
//! Source: <the session or PR where it was decided>
//! ```
//!
//! The parser is strict: four lines in this order and nothing else, so that a comment that
//! only looks like a record never counts. Whether the decider is a code owner, and whether the
//! record comes right before the removal, is decided in [`super::trust`].

/// The first line of every decision record. An HTML comment, so GitHub does not show it.
pub const MARKER: &str = "<!-- nami:decision -->";

/// The prefixes of the record's lines after the marker, in order.
const DECISION: &str = "Decision: ";
const DECIDED_BY: &str = "Decided by: @";
const SOURCE: &str = "Source: ";

/// Longest GitHub login.
const LOGIN_MAX: usize = 39;

/// A decision record, as posted and as parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// What was decided: one paragraph, on one line.
    pub decision: String,
    /// The login of the human who decided, without the `@`.
    pub decider: String,
    /// The day of the decision, `YYYY-MM-DD`.
    pub date: String,
    /// Where it was decided: a session or a PR.
    pub source: String,
}

impl Record {
    /// Reads a comment's body as a record.
    ///
    /// # Errors
    ///
    /// Names the first part that is not in the record's form. The message never quotes the
    /// body.
    pub fn parse(body: &str) -> Result<Self, String> {
        let lines: Vec<&str> = body.lines().collect();
        let [marker, decision, decided_by, source] = lines[..] else {
            return Err(format!("it has {} lines, not the four of a record", lines.len()));
        };
        if marker != MARKER {
            return Err(format!("its first line is not `{MARKER}`"));
        }
        let decision = text_after(decision, DECISION)?;
        let (decider, date) = decided_by
            .strip_prefix(DECIDED_BY)
            .and_then(|rest| rest.split_once(", "))
            .ok_or_else(|| format!("its third line is not `{DECIDED_BY}<login>, <YYYY-MM-DD>`"))?;
        if !is_login(decider) {
            return Err("its decider is not a GitHub login".to_owned());
        }
        if !is_date(date) {
            return Err("its date is not a day in the form YYYY-MM-DD".to_owned());
        }
        let source = text_after(source, SOURCE)?;
        let record = Self {
            decision: decision.to_owned(),
            decider: decider.to_owned(),
            date: date.to_owned(),
            source: source.to_owned(),
        };
        assert_eq!(record.render(), lines.join("\n") + "\n", "a parsed record renders back");
        Ok(record)
    }

    /// The comment's body for this record.
    #[must_use]
    pub fn render(&self) -> String {
        assert!(is_login(&self.decider) && is_date(&self.date), "a record has a decider and day");
        format!(
            "{MARKER}\n{DECISION}{}\n{DECIDED_BY}{}, {}\n{SOURCE}{}\n",
            self.decision, self.decider, self.date, self.source
        )
    }
}

/// The text of `line` after `prefix`: not empty, on one line, and without surrounding space.
fn text_after<'line>(line: &'line str, prefix: &str) -> Result<&'line str, String> {
    let text = line.strip_prefix(prefix).filter(|text| is_one_line(text));
    text.ok_or_else(|| format!("it has no line `{prefix}<text>` where one belongs"))
}

/// True for text that fits a record's line: not empty, no line break, no surrounding space.
#[must_use]
pub fn is_one_line(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && !text.contains(['\n', '\r'])
}

/// True for a GitHub login: letters, digits and single inner hyphens, at most 39 characters.
#[must_use]
pub fn is_login(text: &str) -> bool {
    let is_shaped = !text.starts_with('-') && !text.ends_with('-') && !text.contains("--");
    let is_login = is_shaped
        && (1..=LOGIN_MAX).contains(&text.len())
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    assert!(!is_login || !text.contains('['), "a user's login is never the bot's");
    is_login
}

/// True for a calendar day written `YYYY-MM-DD`.
#[must_use]
pub fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let is_shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !is_shaped {
        return false;
    }
    let number = |range: std::ops::Range<usize>| text.get(range).and_then(|t| t.parse().ok());
    let (Some(year), Some(month), Some(day)) = (number(0..4), number(5..7), number(8..10)) else {
        return false;
    };
    (1..=days_in_month(year, month)).contains(&day)
}

/// The number of days in `month` of `year`, or 0 if there is no such month.
fn days_in_month(year: u32, month: u32) -> u32 {
    let is_leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap => 29,
        2 => 28,
        _ => 0,
    };
    assert!(days <= 31, "no month is longer than 31 days");
    days
}

#[cfg(test)]
mod tests {
    use super::{MARKER, Record, is_date, is_login, is_one_line};

    const RECORD: &str = "<!-- nami:decision -->\nDecision: Agents may remove the label.\n\
                          Decided by: @Owner, 2026-11-26\nSource: lead session, 2026-11-26\n";

    // Goal: a record in the fixed form is read field by field and renders back to itself.
    #[test]
    fn records_are_read() {
        let record = Record::parse(RECORD).expect("a record");
        assert_eq!(record.decision, "Agents may remove the label.");
        assert_eq!((record.decider.as_str(), record.date.as_str()), ("Owner", "2026-11-26"));
        assert_eq!(record.source, "lead session, 2026-11-26");
        assert_eq!(record.render(), RECORD);
        assert_eq!(Record::parse(RECORD.trim_end()), Ok(record), "the last newline is optional");
    }

    // Goal: every near miss of the form is refused, and the reason names the part.
    #[test]
    fn near_misses_are_not_records() {
        let cases = [
            (RECORD.replace(MARKER, "<!-- nami:decided -->"), "first line"),
            (RECORD.replace(MARKER, " <!-- nami:decision -->"), "first line"),
            (format!("\n{RECORD}"), "5 lines"),
            (format!("{RECORD}\n"), "5 lines"),
            (format!("{RECORD}More.\n"), "5 lines"),
            (RECORD.replace("Source: lead session, 2026-11-26\n", ""), "3 lines"),
            (RECORD.replace("Decision: ", "Decision:"), "`Decision: <text>`"),
            (RECORD.replace("the label.", "the label. "), "`Decision: <text>`"),
            (RECORD.replace("Agents may remove the label.", ""), "`Decision: <text>`"),
            (RECORD.replace("Source: ", "From: "), "`Source: <text>`"),
            (RECORD.replace("Source: lead session, 2026-11-26", "Source: "), "`Source: <text>`"),
            (RECORD.replace("@Owner", "Owner"), "third line"),
            (RECORD.replace("@Owner, ", "@Owner "), "third line"),
            (RECORD.replace("@Owner", "@"), "not a GitHub login"),
            (RECORD.replace("@Owner", "@nami-agent[bot]"), "not a GitHub login"),
            (RECORD.replace("2026-11-26\nSource", "2026-02-29\nSource"), "date"),
            (RECORD.replace("2026-11-26\nSource", "26-11-2026\nSource"), "date"),
            (RECORD.replace("2026-11-26\nSource", "2026-11-26.\nSource"), "date"),
        ];
        for (body, expected) in cases {
            let error = Record::parse(&body).expect_err("not a record");
            assert!(error.contains(expected), "{expected}: {error}\n{body}");
        }
    }

    // Goal: only calendar days in the form YYYY-MM-DD are dates, leap days included.
    #[test]
    fn dates_are_calendar_days() {
        for day in ["2026-11-26", "2024-02-29", "2000-02-29", "2026-01-31", "2026-04-30"] {
            assert!(is_date(day), "{day}");
        }
        for day in [
            "2026-02-29",
            "1900-02-29",
            "2100-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-00-10",
            "2026-11-00",
            "2026-1-05",
            "2026/11/26",
            "2026-11-2a",
            "+026-11-26",
            "2026-11-266",
            "",
        ] {
            assert!(!is_date(day), "{day}");
        }
    }

    // Goal: logins are GitHub's: letters, digits and inner single hyphens, up to 39 long.
    #[test]
    fn logins_and_lines_have_their_forms() {
        for login in ["cxkoda", "a", "Some-One2", &"a".repeat(39)] {
            assert!(is_login(login), "{login}");
        }
        for login in ["", "-a", "a-", "a--b", "a_b", "a b", "nami-agent[bot]", &"a".repeat(40)] {
            assert!(!is_login(login), "{login}");
        }
        assert!(is_one_line("PR #64") && !is_one_line("") && !is_one_line(" a"));
        assert!(!is_one_line("a\nb") && !is_one_line("a\rb") && !is_one_line("a "));
    }
}
