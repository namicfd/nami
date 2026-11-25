//! Plans and their `tasks.json` files (ADR-0011): loading and validation.
//!
//! Layout: `docs/exec-plans/{active,completed}/<plan>/plan.md` and `tasks.json`. Each task has
//! a `### <task-id>` section in its `plan.md` with the details a worker needs; the JSON holds
//! only the fields that tools read, so that agents have little reason to edit it.

use crate::json::{self, Value};
use crate::tidy::Violation;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where plans live, relative to the workspace root.
pub const PLANS_DIRECTORY: &str = "docs/exec-plans";

/// The fields of a task, in the order the schema lists them (ADR-0011).
const TASK_FIELDS: [&str; 6] = ["id", "title", "depends_on", "owner_paths", "acceptance", "passes"];

/// Upper bound on tasks across all plans, so the graph checks stay bounded.
const TASKS_MAX: usize = 10_000;

/// One task of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Unique identifier: lowercase letters, digits and dashes.
    pub id: String,
    /// One-line summary.
    pub title: String,
    /// Tasks that must pass first.
    pub depends_on: Vec<String>,
    /// Files and directories the task may touch.
    pub owner_paths: Vec<String>,
    /// Test names that must pass on `main` before CI marks the task done.
    pub acceptance: Vec<String>,
    /// Set only by CI.
    pub passes: bool,
    /// The `tasks.json` holding the task, relative to the workspace root.
    pub file: PathBuf,
    /// True for plans under `active/`.
    pub active: bool,
}

/// Loads every plan under `root`. Returns the tasks that parsed, and every violation found
/// while loading and validating them.
///
/// # Errors
///
/// Returns an error if a plan directory or file cannot be read.
pub fn load(root: &Path) -> io::Result<(Vec<Task>, Vec<Violation>)> {
    let mut tasks = Vec::new();
    let mut violations = Vec::new();
    for (stage, active) in [("active", true), ("completed", false)] {
        let stage_directory = root.join(PLANS_DIRECTORY).join(stage);
        if !stage_directory.is_dir() {
            continue;
        }
        let mut plans: Vec<PathBuf> = fs::read_dir(&stage_directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        plans.sort();
        for plan in plans.iter().filter(|path| path.is_dir()) {
            let relative = plan.strip_prefix(root).expect("plans lie below the root");
            let (plan_tasks, plan_violations) = load_plan(root, relative, active)?;
            tasks.extend(plan_tasks);
            violations.extend(plan_violations);
        }
    }
    violations.extend(check_graph(&tasks));
    Ok((tasks, violations))
}

/// Validates every plan: the tidy entry point.
///
/// # Errors
///
/// Returns an error if a plan directory or file cannot be read.
pub fn check(root: &Path) -> io::Result<Vec<Violation>> {
    load(root).map(|(_, violations)| violations)
}

fn load_plan(
    root: &Path,
    relative: &Path,
    active: bool,
) -> io::Result<(Vec<Task>, Vec<Violation>)> {
    let tasks_file = relative.join("tasks.json");
    let plan_file = relative.join("plan.md");
    if !root.join(&tasks_file).is_file() || !root.join(&plan_file).is_file() {
        let message = "a plan directory needs both plan.md and tasks.json (ADR-0011)";
        return Ok((Vec::new(), vec![Violation::new(relative, 0, "plan-files", message)]));
    }
    let (tasks, mut violations) =
        parse_tasks(&tasks_file, &fs::read_to_string(root.join(&tasks_file))?, active);
    let plan = fs::read_to_string(root.join(&plan_file))?;
    violations.extend(check_sections(&plan_file, &plan, &tasks));
    if let Some(open) = tasks.iter().find(|task| !active && !task.passes) {
        let message = format!(
            "completed plan has the unfinished task `{}`; move the plan back to active/",
            open.id
        );
        violations.push(Violation::new(&tasks_file, 0, "plan-stage", message));
    }
    Ok((tasks, violations))
}

/// Parses one `tasks.json`. Tasks with schema errors are reported and left out.
#[must_use]
pub fn parse_tasks(relative: &Path, text: &str, active: bool) -> (Vec<Task>, Vec<Violation>) {
    let document = match json::parse(text) {
        Ok(document) => document,
        Err(error) => {
            let message = format!("invalid JSON: {}", error.message);
            return (Vec::new(), vec![Violation::new(relative, error.line, "tasks-json", message)]);
        }
    };
    let entries = match document.as_object() {
        Some([(key, Value::Array(entries))]) if key == "tasks" => entries,
        _ => {
            let message = "tasks.json must be an object with exactly one key, `tasks`, holding \
                           an array (ADR-0011)";
            return (Vec::new(), vec![Violation::new(relative, 1, "tasks-json", message)]);
        }
    };
    let mut tasks = Vec::new();
    let mut violations = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        match task_from_json(entry) {
            Ok(fields) => tasks.push(Task { file: relative.to_path_buf(), active, ..fields }),
            Err(problem) => {
                let message = format!("task #{}: {problem} (ADR-0011)", position + 1);
                violations.push(Violation::new(relative, 0, "tasks-json", message));
            }
        }
    }
    (tasks, violations)
}

fn task_from_json(entry: &Value) -> Result<Task, String> {
    let members = entry.as_object().ok_or_else(|| format!("is {}, not an object", entry.kind()))?;
    let names: Vec<&str> = members.iter().map(|(name, _)| name.as_str()).collect();
    if names != TASK_FIELDS {
        return Err(format!("has the fields {names:?}; expected exactly {TASK_FIELDS:?}"));
    }
    let id = string_field(entry, "id")?;
    let valid_id =
        id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if id.is_empty() || !valid_id || id.starts_with('-') {
        return Err(format!("id `{id}` must be lowercase letters, digits and dashes"));
    }
    let task = Task {
        title: string_field(entry, "title")?,
        depends_on: string_list(entry, "depends_on")?,
        owner_paths: string_list(entry, "owner_paths")?,
        acceptance: string_list(entry, "acceptance")?,
        passes: entry.get("passes").and_then(Value::as_bool).ok_or("`passes` must be a boolean")?,
        file: PathBuf::new(),
        active: true,
        id,
    };
    if task.title.trim().is_empty() {
        return Err(format!("`{}` has an empty title", task.id));
    }
    if task.owner_paths.is_empty() || task.acceptance.is_empty() {
        return Err(format!("`{}` needs at least one owner path and one acceptance test", task.id));
    }
    if let Some(path) = task.owner_paths.iter().find(|path| !is_clean_relative(path)) {
        return Err(format!(
            "`{}` has owner path `{path}`, which is not a clean relative path",
            task.id
        ));
    }
    Ok(task)
}

fn string_field(entry: &Value, name: &str) -> Result<String, String> {
    let value = entry.get(name).expect("the field list was checked");
    value.as_str().map(str::to_owned).ok_or_else(|| format!("`{name}` must be a string"))
}

fn string_list(entry: &Value, name: &str) -> Result<Vec<String>, String> {
    let items = entry.get(name).and_then(Value::as_array);
    let items = items.ok_or_else(|| format!("`{name}` must be an array of strings"))?;
    let strings: Option<Vec<String>> = items
        .iter()
        .map(|item| item.as_str().filter(|text| !text.is_empty()).map(str::to_owned))
        .collect();
    strings.ok_or_else(|| format!("`{name}` must hold only non-empty strings"))
}

/// A relative path without `.`, `..`, empty components or a trailing slash.
fn is_clean_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Every task needs a `### <id>` section in its `plan.md`.
fn check_sections(plan_file: &Path, plan: &str, tasks: &[Task]) -> Vec<Violation> {
    let headings: BTreeSet<&str> =
        plan.lines().filter_map(|line| line.strip_prefix("### ")).map(str::trim).collect();
    let missing = tasks.iter().filter(|task| !headings.contains(task.id.as_str()));
    missing
        .map(|task| {
            let message = format!(
                "task `{}` has no `### {}` section describing it (ADR-0011)",
                task.id, task.id
            );
            Violation::new(plan_file, 0, "plan-section", message)
        })
        .collect()
}

/// Checks the dependency graph across all plans.
#[must_use]
pub fn check_graph(tasks: &[Task]) -> Vec<Violation> {
    assert!(tasks.len() <= TASKS_MAX, "more tasks than the graph checks are bounded for");
    let mut violations = Vec::new();
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (position, task) in tasks.iter().enumerate() {
        if index.insert(task.id.as_str(), position).is_some() {
            let message = format!("task id `{}` is used more than once (ADR-0011)", task.id);
            violations.push(Violation::new(&task.file, 0, "task-graph", message));
        }
    }
    for task in tasks {
        for dependency in &task.depends_on {
            let problem = match index.get(dependency.as_str()) {
                None => Some(format!("depends on `{dependency}`, which does not exist")),
                Some(_) if *dependency == task.id => Some("depends on itself".to_owned()),
                Some(&other) if task.passes && !tasks.get(other).is_some_and(|t| t.passes) => {
                    Some(format!("passes, but its dependency `{dependency}` does not"))
                }
                Some(_) => None,
            };
            if let Some(problem) = problem {
                let message = format!("task `{}` {problem} (ADR-0011)", task.id);
                violations.push(Violation::new(&task.file, 0, "task-graph", message));
            }
        }
    }
    if violations.is_empty() {
        let ancestors = ancestors(tasks, &index);
        violations.extend(check_cycles(tasks, &ancestors));
        if violations.is_empty() {
            violations.extend(check_disjoint(tasks, &ancestors));
        }
    }
    violations
}

/// For each task, the set of tasks it depends on, directly or not. Breadth-first, bounded by the
/// number of tasks.
fn ancestors(tasks: &[Task], index: &BTreeMap<&str, usize>) -> Vec<BTreeSet<usize>> {
    let direct: Vec<Vec<usize>> = tasks
        .iter()
        .map(|task| {
            task.depends_on.iter().filter_map(|id| index.get(id.as_str()).copied()).collect()
        })
        .collect();
    let all: Vec<BTreeSet<usize>> =
        (0..tasks.len()).map(|start| reachable(&direct, start)).collect();
    assert_eq!(all.len(), tasks.len(), "one ancestor set per task");
    all
}

/// The nodes reachable from `start` along `edges`, not counting `start` unless it lies on a
/// cycle. Breadth-first; each round adds at least one node, so it ends within `edges.len()`.
fn reachable(edges: &[Vec<usize>], start: usize) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut frontier = edges.get(start).cloned().unwrap_or_default();
    for _ in 0..=edges.len() {
        let next: Vec<usize> = frontier
            .into_iter()
            .filter(|&node| seen.insert(node))
            .flat_map(|node| edges.get(node).into_iter().flatten().copied())
            .collect();
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    assert!(seen.len() <= edges.len(), "only known nodes are reachable");
    seen
}

fn check_cycles(tasks: &[Task], ancestors: &[BTreeSet<usize>]) -> Vec<Violation> {
    let cyclic = tasks
        .iter()
        .zip(ancestors)
        .enumerate()
        .filter(|(position, (_, set))| set.contains(position));
    cyclic
        .map(|(_, (task, _))| {
            let message =
                format!("task `{}` depends on itself through a cycle (ADR-0011)", task.id);
            Violation::new(&task.file, 0, "task-graph", message)
        })
        .collect()
}

/// Two unfinished tasks that may run at the same time must not share owner paths, or their PRs
/// would conflict (ADR-0011).
fn check_disjoint(tasks: &[Task], ancestors: &[BTreeSet<usize>]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let open: Vec<usize> =
        (0..tasks.len()).filter(|&i| tasks.get(i).is_some_and(|t| !t.passes)).collect();
    for (offset, &first) in open.iter().enumerate() {
        for &second in open.iter().skip(offset + 1) {
            let ordered = ancestors.get(first).is_some_and(|set| set.contains(&second))
                || ancestors.get(second).is_some_and(|set| set.contains(&first));
            let (Some(a), Some(b)) = (tasks.get(first), tasks.get(second)) else {
                unreachable!("positions come from the task list");
            };
            let shared = a
                .owner_paths
                .iter()
                .find(|path| b.owner_paths.iter().any(|other| overlaps(path, other)));
            if let (false, Some(path)) = (ordered, shared) {
                let message = format!(
                    "tasks `{}` and `{}` can run at the same time but both own `{path}`; make one \
                     depend on the other, or split the path (ADR-0011)",
                    a.id, b.id
                );
                violations.push(Violation::new(&b.file, 0, "task-owner-paths", message));
            }
        }
    }
    violations
}

/// True if one path is the other or lies inside it.
#[must_use]
pub fn overlaps(first: &str, second: &str) -> bool {
    let inside = |path: &str, directory: &str| {
        path.strip_prefix(directory).is_some_and(|rest| rest.starts_with('/'))
    };
    first == second || inside(first, second) || inside(second, first)
}

#[cfg(test)]
mod tests {
    use super::{Task, check_graph, check_sections, overlaps, parse_tasks};
    use std::path::{Path, PathBuf};

    fn task(id: &str, depends_on: &[&str], owner_paths: &[&str], passes: bool) -> Task {
        Task {
            id: id.to_owned(),
            title: format!("Task {id}"),
            depends_on: depends_on.iter().map(|&d| d.to_owned()).collect(),
            owner_paths: owner_paths.iter().map(|&p| p.to_owned()).collect(),
            acceptance: vec![format!("tests::{id}")],
            passes,
            file: PathBuf::from("tasks.json"),
            active: true,
        }
    }

    fn rules(violations: &[crate::tidy::Violation]) -> Vec<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: a well-formed tasks.json parses into tasks, keeping every field.
    #[test]
    fn valid_tasks_parse() {
        let text = r#"{"tasks": [{"id": "a-1", "title": "First", "depends_on": [],
            "owner_paths": ["crates/x/src/a.rs"], "acceptance": ["x::a"], "passes": false}]}"#;
        let (tasks, violations) = parse_tasks(Path::new("p/tasks.json"), text, true);
        assert_eq!(violations, Vec::new());
        let expected = Task {
            title: "First".to_owned(),
            acceptance: vec!["x::a".to_owned()],
            file: PathBuf::from("p/tasks.json"),
            ..task("a-1", &[], &["crates/x/src/a.rs"], false)
        };
        assert_eq!(tasks, [expected]);
    }

    // Goal: every schema rule rejects a task that breaks it, and says which rule.
    #[test]
    fn schema_errors_are_reported() {
        let good = concat!(
            r#""title": "T", "depends_on": [], "owner_paths": ["a"], "#,
            r#""acceptance": ["t"], "passes": false"#
        );
        let cases = [
            ("[]".to_owned(), "exactly one key"),
            ("{\"tasks\": [], \"x\": 1}".to_owned(), "exactly one key"),
            ("{\"other\": []}".to_owned(), "exactly one key"),
            ("{\"tasks\": [1]}".to_owned(), "not an object"),
            (format!("{{\"tasks\": [{{\"id\": \"A\", {good}}}]}}"), "lowercase"),
            (format!("{{\"tasks\": [{{\"id\": \"-a\", {good}}}]}}"), "lowercase"),
            (format!("{{\"tasks\": [{{{good}, \"id\": \"a\"}}]}}"), "expected exactly"),
            (format!("{{\"tasks\": [{{\"id\": 1, {good}}}]}}"), "`id` must be a string"),
            (
                format!("{{\"tasks\": [{{\"id\": \"a\", {}}}]}}", good.replace("\"T\"", "\" \"")),
                "empty title",
            ),
            (
                format!("{{\"tasks\": [{{\"id\": \"a\", {}}}]}}", good.replace("[\"a\"]", "[]")),
                "at least one",
            ),
            (
                format!(
                    "{{\"tasks\": [{{\"id\": \"a\", {}}}]}}",
                    good.replace("[\"t\"]", "[\"\"]")
                ),
                "non-empty strings",
            ),
            (
                format!(
                    "{{\"tasks\": [{{\"id\": \"a\", {}}}]}}",
                    good.replace("[\"a\"]", "[\"../a\"]")
                ),
                "clean relative",
            ),
            (
                format!("{{\"tasks\": [{{\"id\": \"a\", {}}}]}}", good.replace("false", "\"no\"")),
                "boolean",
            ),
            ("{\"tasks\": [}".to_owned(), "invalid JSON"),
        ];
        for (text, expected) in cases {
            let (tasks, violations) = parse_tasks(Path::new("tasks.json"), &text, true);
            assert!(tasks.is_empty(), "{text}: the task is left out");
            assert_eq!(violations.len(), 1, "{text}");
            assert!(violations[0].message.contains(expected), "{text}: {}", violations[0].message);
        }
    }

    // Goal: a task without a `### id` section in plan.md is reported.
    #[test]
    fn plan_sections_are_required() {
        let tasks = [task("a", &[], &["x"], false), task("b", &[], &["y"], false)];
        let violations =
            check_sections(Path::new("plan.md"), "# Plan\n### a\ntext\n## b\n", &tasks);
        assert_eq!(rules(&violations), ["plan-section"]);
        assert!(violations[0].message.contains("`b`"), "the missing task is named");
    }

    // Goal: duplicates, unknown or self dependencies, cycles and inconsistent `passes` are
    // reported; a valid graph is not.
    #[test]
    fn graph_errors_are_reported() {
        let valid = [task("a", &[], &["x"], true), task("b", &["a"], &["x"], false)];
        assert_eq!(check_graph(&valid), Vec::new());
        let cases: [(Vec<Task>, &str); 5] = [
            (vec![task("a", &[], &["x"], false), task("a", &[], &["y"], false)], "more than once"),
            (vec![task("a", &["z"], &["x"], false)], "does not exist"),
            (vec![task("a", &["a"], &["x"], false)], "depends on itself"),
            (vec![task("a", &["b"], &["x"], false), task("b", &["a"], &["y"], false)], "cycle"),
            (vec![task("a", &[], &["x"], false), task("b", &["a"], &["y"], true)], "does not"),
        ];
        for (tasks, expected) in cases {
            let violations = check_graph(&tasks);
            assert!(!violations.is_empty(), "{expected}: reported");
            assert!(
                violations.iter().all(|v| v.message.contains(expected)),
                "{expected}: {violations:?}"
            );
        }
    }

    // Goal: tasks that can run concurrently must own disjoint paths; ordered or finished tasks
    // may share them.
    #[test]
    fn concurrent_tasks_own_disjoint_paths() {
        let concurrent = [
            task("a", &[], &["crates/x/src"], false),
            task("b", &[], &["crates/x/src/b.rs"], false),
        ];
        assert_eq!(rules(&check_graph(&concurrent)), ["task-owner-paths"]);
        let ordered = [
            task("a", &[], &["crates/x"], false),
            task("b", &[], &["y"], false),
            task("c", &["b", "a"], &["crates/x"], false),
        ];
        assert_eq!(check_graph(&ordered), Vec::new());
        let finished = [task("a", &[], &["crates/x"], true), task("b", &[], &["crates/x"], false)];
        assert_eq!(check_graph(&finished), Vec::new());
    }

    // Goal: plans load from disk, and incomplete plan directories and unfinished completed
    // plans are reported. Method: a fixture tree with one good plan and two broken ones.
    #[test]
    fn plans_load_from_disk() {
        let root = std::env::temp_dir().join(format!("nami-plans-{}", std::process::id()));
        let plans = root.join(super::PLANS_DIRECTORY);
        let good = r#"{"tasks": [{"id": "a", "title": "A", "depends_on": [],
            "owner_paths": ["x"], "acceptance": ["t"], "passes": false}]}"#;
        for (directory, tasks, plan) in [
            ("active/good", Some(good), Some("### a\n")),
            ("active/no-tasks", None, Some("# Plan\n")),
            (
                "completed/open",
                Some(&good.replace("\"a\"", "\"b\"").replace("\"x\"", "\"y\"")),
                Some("### b\n"),
            ),
        ] {
            let directory = plans.join(directory);
            std::fs::create_dir_all(&directory).unwrap();
            if let Some(tasks) = tasks {
                std::fs::write(directory.join("tasks.json"), tasks).unwrap();
            }
            if let Some(plan) = plan {
                std::fs::write(directory.join("plan.md"), plan).unwrap();
            }
        }
        let (tasks, violations) = super::load(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let ids: Vec<(&str, bool)> =
            tasks.iter().map(|task| (task.id.as_str(), task.active)).collect();
        assert_eq!(ids, [("a", true), ("b", false)]);
        assert_eq!(rules(&violations), ["plan-files", "plan-stage"]);
        assert_eq!(
            super::check(Path::new("/nonexistent")).unwrap(),
            Vec::new(),
            "no plans is fine"
        );
    }

    // Goal: path overlap means equality or containment by whole components.
    #[test]
    fn overlap_is_by_whole_components() {
        assert!(overlaps("a/b", "a/b"), "equal");
        assert!(overlaps("a", "a/b"), "directory contains file");
        assert!(overlaps("a/b/c", "a"), "file inside directory");
        assert!(!overlaps("a/bc", "a/b"), "a shared prefix is not containment");
        assert!(!overlaps("a", "b"), "unrelated");
    }
}
