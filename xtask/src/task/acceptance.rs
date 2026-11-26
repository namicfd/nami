//! A task's acceptance tests, run by exact name (ADR-0011).
//!
//! An acceptance name is `<crate>::<path>`, where `<crate>` is the package name with `-`
//! written `_`. It names a test that `cargo nextest list` reports:
//!
//! - in a library, binary or proc-macro target: `<crate>::<module path>::<test>`, such as
//!   `xtask::tidy::fixtures::every_source_rule_fires_on_its_fixture`;
//! - in an integration test, bench or example target: `<crate>::<target>::<module path>::<test>`,
//!   such as `xtask::task_commands::check_pr_rejects_passes_set_outside_ci`.
//!
//! Names are looked up in the root workspace and in `lints/`, which is its own workspace on a
//! pinned nightly. A name that matches no test, or only ignored tests, fails: zero tests run is
//! never a pass.

use crate::json::{self, Value};
use crate::task::plan::Task;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The workspaces that hold tests, relative to the root, in the order they are listed.
pub const WORKSPACES: [&str; 2] = [".", "lints"];

/// Environment variables that `cargo xtask` gets from cargo and rustup, and that would make a
/// nested cargo build with the root's toolchain in `lints/`.
const INHERITED_TOOLCHAIN: [&str; 4] = ["RUSTUP_TOOLCHAIN", "RUSTC", "RUSTDOC", "CARGO"];

/// One test that nextest lists and does not ignore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Its workspace, one of [`WORKSPACES`].
    pub workspace: String,
    /// nextest's binary ID, such as `xtask::task_commands`.
    pub binary_id: String,
    /// Its name inside the binary, such as `tidy::fixtures::fires`.
    pub test: String,
    /// The acceptance name that names it.
    pub name: String,
}

/// The tests in `cargo nextest list --message-format json` output from `workspace`, without
/// the ignored ones.
///
/// # Errors
///
/// Names the field that is missing or has the wrong type.
pub fn listed_from_json(workspace: &str, value: &Value) -> Result<Vec<Listed>, String> {
    let suites = value.get("rust-suites").and_then(Value::as_object);
    let suites = suites.ok_or("nextest sent no `rust-suites` object")?;
    let mut listed = Vec::new();
    for (binary_id, suite) in suites {
        let field = |key: &str| {
            suite.get(key).and_then(Value::as_str).ok_or_else(|| {
                format!("nextest sent no string `{key}` for the binary `{binary_id}`")
            })
        };
        let package = field("package-name")?.replace('-', "_");
        let prefix = match field("kind")? {
            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" | "bin" => package,
            _ => format!("{package}::{}", field("binary-name")?),
        };
        let cases = suite.get("testcases").and_then(Value::as_object);
        let cases = cases.ok_or_else(|| format!("nextest sent no tests for `{binary_id}`"))?;
        for (test, case) in cases {
            let ignored = case.get("ignored").and_then(Value::as_bool);
            let ignored =
                ignored.ok_or_else(|| format!("nextest sent no `ignored` for `{test}`"))?;
            if !ignored {
                listed.push(Listed {
                    workspace: workspace.to_owned(),
                    binary_id: binary_id.clone(),
                    test: test.clone(),
                    name: format!("{prefix}::{test}"),
                });
            }
        }
    }
    assert!(listed.iter().all(|test| test.name.contains("::")), "every name has a crate");
    Ok(listed)
}

/// The tests that the acceptance name `name` names, or why it names none.
///
/// # Errors
///
/// When no listed test has that name, or a match holds characters that a filterset could
/// misread.
pub fn resolve<'listed>(
    name: &str,
    listed: &'listed [Listed],
) -> Result<Vec<&'listed Listed>, String> {
    let found: Vec<&Listed> = listed.iter().filter(|test| test.name == name).collect();
    if found.is_empty() {
        return Err(format!(
            "`{name}` names no test that runs: nextest lists none by that name in {}, or only \
             ignored ones (`<crate>::<module path>::<test>`, or `<crate>::<test target>::...` \
             for a file under tests/)",
            WORKSPACES.join(" and ")
        ));
    }
    if let Some(odd) = found.iter().find(|test| !is_plain(&test.binary_id) || !is_plain(&test.test))
    {
        return Err(format!("`{}` holds characters that a nextest filter could misread", odd.name));
    }
    assert!(found.iter().all(|test| test.name == name), "only exact matches");
    Ok(found)
}

/// True for the characters of Rust paths and nextest binary IDs: nothing a filterset reads as
/// an operator.
fn is_plain(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_:-/".contains(&byte))
}

/// The nextest filterset that selects exactly `tests`, each by its binary and its full name.
#[must_use]
pub fn filterset(tests: &[&Listed]) -> String {
    assert!(!tests.is_empty(), "an empty filterset would select nothing");
    let terms: Vec<String> = tests
        .iter()
        .map(|test| format!("(binary_id(={}) & test(={}))", test.binary_id, test.test))
        .collect();
    let expression = terms.join(" | ");
    assert!(expression.contains("binary_id(="), "every term is exact");
    expression
}

/// What setting `passes` needs from a test runner.
pub trait Acceptance {
    /// For each task, in order: `Ok(())` if every one of its acceptance tests ran and passed,
    /// or why not.
    ///
    /// # Errors
    ///
    /// When the tests cannot be listed at all.
    fn check(&self, tasks: &[&Task]) -> Result<Vec<Result<(), String>>, String>;
}

/// [`Acceptance`] through `cargo nextest` in the checkout at `root`.
#[derive(Debug, Clone)]
pub struct Nextest {
    root: PathBuf,
}

impl Nextest {
    /// Runs nextest in the workspaces of the checkout at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    fn cargo(&self, workspace: &str) -> Command {
        let mut command = Command::new("cargo");
        let _ = command.current_dir(self.root.join(workspace));
        for variable in INHERITED_TOOLCHAIN {
            let _ = command.env_remove(variable);
        }
        command
    }

    /// Every test in `workspace` that nextest lists and does not ignore, or nothing if the
    /// workspace does not exist.
    fn list_in(&self, workspace: &str) -> Result<Vec<Listed>, String> {
        if !self.root.join(workspace).join("Cargo.toml").is_file() {
            return Ok(Vec::new());
        }
        let mut command = self.cargo(workspace);
        let arguments = ["nextest", "list", "--workspace", "--locked", "--message-format"];
        let _ = command.args(arguments).arg("json").stderr(Stdio::inherit());
        let output = command
            .output()
            .map_err(|error| format!("cannot run cargo nextest: {error}; install it"))?;
        if !output.status.success() {
            return Err(format!("`cargo nextest list` failed in `{workspace}`"));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| "cargo nextest printed invalid UTF-8".to_owned())?;
        let value = json::parse(&text)
            .map_err(|error| format!("cargo nextest printed invalid JSON: {}", error.message))?;
        let listed = listed_from_json(workspace, &value)?;
        assert!(listed.iter().all(|test| test.workspace == workspace), "listed where it ran");
        Ok(listed)
    }

    /// The tests that `tasks` may name. The root workspace comes first; `lints/` builds the
    /// dylint library on its own nightly, so it is listed only when the root lacks a name.
    fn list(&self, tasks: &[&Task]) -> Result<Vec<Listed>, String> {
        let [first, rest @ ..] = WORKSPACES;
        let mut listed = self.list_in(first)?;
        let lacks = |known: &[Listed]| {
            let mut names = tasks.iter().flat_map(|task| &task.acceptance);
            names.any(|name| !known.iter().any(|test| test.name == *name))
        };
        for workspace in rest {
            if lacks(&listed) {
                listed.extend(self.list_in(workspace)?);
            }
        }
        assert!(listed.iter().all(|test| !test.name.is_empty()), "every test has a name");
        Ok(listed)
    }

    /// Runs `task`'s acceptance tests: each name must name a test, and every test must pass.
    fn run(&self, task: &Task, listed: &[Listed]) -> Result<(), String> {
        let mut chosen = Vec::new();
        for name in &task.acceptance {
            chosen.extend(resolve(name, listed)?);
        }
        assert!(chosen.len() >= task.acceptance.len(), "each name names at least one test");
        for workspace in WORKSPACES {
            let here: Vec<&Listed> =
                chosen.iter().copied().filter(|test| test.workspace == workspace).collect();
            if here.is_empty() {
                continue;
            }
            let mut command = self.cargo(workspace);
            let arguments = ["nextest", "run", "--workspace", "--locked", "--no-tests=fail", "-E"];
            let _ = command.args(arguments).arg(filterset(&here));
            let _ = command.stdout(Stdio::from(std::io::stderr())).stderr(Stdio::inherit());
            let status = command
                .status()
                .map_err(|error| format!("cannot run cargo nextest: {error}; install it"))?;
            if !status.success() {
                return Err(format!(
                    "the acceptance tests of `{}` failed in `{workspace}`; nextest's output is \
                     above",
                    task.id
                ));
            }
        }
        Ok(())
    }
}

impl Acceptance for Nextest {
    fn check(&self, tasks: &[&Task]) -> Result<Vec<Result<(), String>>, String> {
        if tasks.is_empty() {
            return Ok(Vec::new());
        }
        let listed = self.list(tasks)?;
        let verdicts: Vec<Result<(), String>> =
            tasks.iter().map(|task| self.run(task, &listed)).collect();
        assert_eq!(verdicts.len(), tasks.len(), "one verdict per task");
        Ok(verdicts)
    }
}

#[cfg(test)]
mod tests {
    use super::{Listed, filterset, listed_from_json, resolve};
    use crate::json;

    /// nextest's list output, cut down: a library, an integration test and a binary.
    const LIST: &str = r#"{"rust-suites": {
      "alloc-guard::counting": {"package-name": "alloc-guard", "binary-name": "counting",
        "kind": "test", "testcases": {"allocation_free_work_passes": {"ignored": false}}},
      "xtask": {"package-name": "xtask", "binary-name": "xtask", "kind": "lib",
        "testcases": {"tidy::fixtures::fires": {"ignored": false},
                      "tidy::slow": {"ignored": true}}},
      "xtask::bin/xtask": {"package-name": "xtask", "binary-name": "xtask", "kind": "bin",
        "testcases": {}}
    }}"#;

    fn listed() -> Vec<Listed> {
        let value = json::parse(LIST).expect("the fixture is JSON");
        listed_from_json(".", &value).expect("the fixture is nextest's shape")
    }

    // Goal: library tests are named after the crate, integration tests after the crate and
    // their target, and ignored tests are left out.
    #[test]
    fn listed_tests_get_their_acceptance_names() {
        let names: Vec<String> = listed().into_iter().map(|test| test.name).collect();
        assert_eq!(
            names,
            ["alloc_guard::counting::allocation_free_work_passes", "xtask::tidy::fixtures::fires"]
        );
        let broken = json::parse(r#"{"rust-suites": {"x": {"kind": "lib"}}}"#).expect("JSON");
        let error = listed_from_json(".", &broken).expect_err("no package name");
        assert!(error.contains("package-name") && error.contains("`x`"), "{error}");
        assert!(listed_from_json(".", &json::parse("{}").expect("JSON")).is_err(), "no suites");
    }

    // Goal: a name resolves only to the test with exactly that name; a missing, ignored,
    // partial or longer name resolves to nothing, so it can never count as passing.
    #[test]
    fn names_resolve_exactly() {
        let listed = listed();
        let found = resolve("xtask::tidy::fixtures::fires", &listed).expect("it exists");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].binary_id, "xtask");
        assert_eq!(found[0].test, "tidy::fixtures::fires");
        for missing in [
            "xtask::tidy::slow",
            "xtask::tidy::fixtures",
            "xtask::tidy::fixtures::fire",
            "xtask::tidy::fixtures::fires::x",
            "ci::gate::determinism_rerun",
        ] {
            let error = resolve(missing, &listed).expect_err("no such test");
            assert!(error.contains("names no test that runs"), "{missing}: {error}");
        }
        let odd =
            Listed { test: "a | all()".to_owned(), name: "x::a".to_owned(), ..listed[1].clone() };
        let error = resolve("x::a", std::slice::from_ref(&odd)).expect_err("operators are refused");
        assert!(error.contains("misread"), "{error}");
    }

    // Goal: the filterset names each test by binary and full name, so nextest runs nothing
    // else.
    #[test]
    fn filterset_is_exact() {
        let listed = listed();
        let all: Vec<&Listed> = listed.iter().collect();
        assert_eq!(
            filterset(&all),
            "(binary_id(=alloc-guard::counting) & test(=allocation_free_work_passes)) | \
             (binary_id(=xtask) & test(=tidy::fixtures::fires))"
        );
    }
}
