//! Line-based rules.

use super::Violation;
use std::path::Path;

/// Maximum line length in Rust sources (`TIGER_STYLE.md`, Style by the numbers).
const LINE_COLUMNS_MAX: usize = 100;

/// Debt markers that must not reach `main`. Built with `concat!` so this file passes its own check.
const DEBT_MARKERS: [&str; 3] = [concat!("TO", "DO"), concat!("FIX", "ME"), concat!("X", "XX")];

/// Suppression attributes counted against the budget. Built with `concat!` for the same reason.
const EXPECT_ATTRIBUTES: [&str; 2] = [concat!("#[", "expect("), concat!("#![", "expect(")];

/// The attribute that marks a test function.
const TEST_ATTRIBUTE: &str = "#[test]";

/// Runs every line-based rule on one file.
pub fn check(path: &Path, text: &str) -> Vec<Violation> {
    let lines: Vec<&str> = text.lines().collect();
    let mut violations = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let line_number = to_line_number(line_index);
        if line.chars().count() > LINE_COLUMNS_MAX {
            let message = "line is longer than 100 columns; wrap it (TIGER_STYLE.md)";
            violations.push(Violation::new(path, line_number, "line-length", message));
        }
        if DEBT_MARKERS.iter().any(|marker| line.contains(marker)) {
            let message = "debt marker in source; finish the work, or record it as a task in \
                           docs/exec-plans/ (TIGER_STYLE.md, Zero technical debt)";
            violations.push(Violation::new(path, line_number, "debt-marker", message));
        }
        if line.trim() == TEST_ATTRIBUTE && !has_goal_comment(&lines, line_index) {
            let message = "test without a goal comment; start it with `// Goal: ...` saying \
                           what it checks and how (TIGER_STYLE.md, Comments)";
            violations.push(Violation::new(path, line_number, "test-goal", message));
        }
    }
    violations
}

/// Counts `expect` suppression attributes, inner and outer, in one file.
pub fn expect_count(text: &str) -> u32 {
    let count: usize =
        EXPECT_ATTRIBUTES.iter().map(|attribute| text.matches(attribute).count()).sum();
    u32::try_from(count).expect("expect count fits in u32")
}

/// True if the first line above the test's attributes is a comment.
fn has_goal_comment(lines: &[&str], test_line_index: usize) -> bool {
    let above = lines.iter().take(test_line_index).rev();
    let mut non_attributes = above.filter(|line| !line.trim_start().starts_with("#["));
    non_attributes.next().is_some_and(|line| line.trim_start().starts_with("//"))
}

fn to_line_number(line_index: usize) -> u32 {
    u32::try_from(line_index + 1).expect("source files have fewer than 2^32 lines")
}

#[cfg(test)]
mod tests {
    use super::{check, expect_count};
    use std::path::Path;

    fn rules(text: &str) -> Vec<&'static str> {
        check(Path::new("fixture.rs"), text).iter().map(|violation| violation.rule).collect()
    }

    // Goal: lines over 100 columns are flagged, lines of exactly 100 are not.
    #[test]
    fn line_length_rule_fires() {
        assert_eq!(rules(&"x".repeat(101)), ["line-length"]);
        assert_eq!(rules(&"x".repeat(100)), Vec::<&str>::new());
    }

    // Goal: debt markers are flagged with their line number.
    #[test]
    fn debt_marker_rule_fires() {
        let source = format!("fn main() {{}}\n// {}: later\n", concat!("TO", "DO"));
        let violations = check(Path::new("fixture.rs"), &source);
        assert_eq!(violations.len(), 1);
        assert_eq!((violations[0].rule, violations[0].line), ("debt-marker", 2));
    }

    // Goal: a test needs a comment above its attributes; other attributes may sit in between.
    #[test]
    fn test_goal_rule_fires() {
        assert_eq!(rules("\n#[test]\nfn a() {}\n"), ["test-goal"]);
        assert_eq!(rules("// Goal: x.\n#[test]\n#[should_panic]\nfn a() {}\n"), Vec::<&str>::new());
        assert_eq!(rules("// Goal: x.\n#[cfg(miri)]\n#[test]\nfn a() {}\n"), Vec::<&str>::new());
    }

    // Goal: both inner and outer expect attributes count against the budget.
    #[test]
    fn expect_attributes_are_counted() {
        let source = format!("{}a)]\n{}b)]\n", concat!("#[", "expect("), concat!("#![", "expect("));
        assert_eq!(expect_count(&source), 2);
    }
}
