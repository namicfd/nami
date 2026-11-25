//! Rules about crate manifests: runtime dependencies (ADR-0001).
//!
//! xtask has no TOML parser (ADR-0001), so manifests are read line by line. That is enough for
//! the way manifests are written here, and where a line is ambiguous the rule reports it.

use super::Violation;
use std::fs;
use std::io;
use std::path::Path;

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

/// Checks the normal and build dependencies of one manifest. Dev-dependencies are governed by
/// the cargo-deny allow-list instead.
pub fn check_manifest(relative: &Path, text: &str, workspace_crates: &[String]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut in_runtime_section = false;
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('[') {
            in_runtime_section = is_runtime_dependency_section(line);
            continue;
        }
        let key = line.split(['=', '.']).next().unwrap_or_default().trim();
        if in_runtime_section
            && !key.is_empty()
            && !key.starts_with('#')
            && !is_allowed_runtime(key, workspace_crates)
        {
            let line_number = u32::try_from(line_index + 1).expect("manifest has < 2^32 lines");
            let message = format!(
                "runtime dependency `{key}`; the only runtime dependency allowed is `libm`. \
                 Adding a crate needs an ADR approved by a human (ADR-0001)"
            );
            violations.push(Violation::new(relative, line_number, "runtime-dependency", message));
        }
    }
    violations
}

fn is_runtime_dependency_section(header: &str) -> bool {
    let name = header.trim_start_matches('[').trim_end_matches(']');
    let is_dependencies = name == "dependencies" || name.ends_with(".dependencies");
    let is_build = name == "build-dependencies" || name.ends_with(".build-dependencies");
    is_dependencies || is_build
}

fn is_allowed_runtime(key: &str, workspace_crates: &[String]) -> bool {
    key == "libm" || workspace_crates.iter().any(|name| name == key)
}

#[cfg(test)]
mod tests {
    use super::{check_manifest, workspace_crates};
    use std::path::Path;

    fn rules(violations: &[super::Violation]) -> Vec<&'static str> {
        violations.iter().map(|violation| violation.rule).collect()
    }

    // Goal: only libm and workspace crates may be runtime dependencies; dev-dependencies are
    // exempt. A crate that merely looks like a workspace crate (`base-extra`) is rejected.
    #[test]
    fn runtime_dependency_rule_fires() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\nlibm.workspace = true\n\
                        base = { path = \"../base\" }\nserde = \"1\"\nbase-extra = \"1\"\n\n\
                        [dev-dependencies]\nproptest = \"1\"\n";
        let workspace = ["base".to_owned(), "kernels".to_owned()];
        let violations = check_manifest(Path::new("Cargo.toml"), manifest, &workspace);
        assert_eq!(rules(&violations), ["runtime-dependency", "runtime-dependency"]);
        assert_eq!(violations[0].line, 7);
        assert_eq!(violations[1].line, 8);
    }

    // Goal: build-dependencies and target-specific dependencies count as runtime.
    #[test]
    fn build_and_target_dependencies_are_runtime() {
        let manifest =
            "[build-dependencies]\ncc = \"1\"\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n";
        assert_eq!(rules(&check_manifest(Path::new("Cargo.toml"), manifest, &[])).len(), 2);
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
