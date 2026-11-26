//! What the head's tools printed on the base's cases: plain data, written by the job that runs
//! the head's code and judged by a job that runs none (ADR-0022, §4).
//!
//! Every run keeps the program's exit code and its whole output, and the judge parses them with
//! the base's code. The head's code can lie in what it prints; the threat model covers that.
//! What the judge cannot read is no verdict, never a pass.

use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// The version of this format. A judge refuses any other.
const FORMAT: &str = "2";

/// One run of a head tool on one base case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The base case: a fixture or UI case path, `lints/ui`, or a workspace for mutants.
    pub subject: String,
    /// Where the case was planted in the head tree, if it was.
    pub placement: String,
    /// What ran: `file`, `tree`, `library`, `dylint` or `mutants`.
    pub mode: String,
    /// The exit code, or `None` if the program was killed by a signal.
    pub exit: Option<i32>,
    /// Everything it printed on stdout.
    pub stdout: String,
    /// Everything it printed on stderr.
    pub stderr: String,
    /// Files it left, by name; `None` for a file that was not there.
    pub files: BTreeMap<String, Option<String>>,
}

/// Everything one observation of the head recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observations {
    /// True if the observer found no change to the tooling and ran nothing.
    pub skipped: bool,
    /// The seed of the placements.
    pub seed: u64,
    /// The fingerprint of the base corpus the observer planted.
    pub corpus: String,
    /// The fingerprint of the head tree the observer copied ([`super::tree::fingerprint`]).
    pub head: String,
    /// Every run, in the order they ran.
    pub runs: Vec<Run>,
}

impl Observations {
    /// The runs of one mode for one subject.
    pub fn runs_of<'a>(&'a self, subject: &'a str, mode: &'a str) -> impl Iterator<Item = &'a Run> {
        self.runs.iter().filter(move |run| run.subject == subject && run.mode == mode)
    }

    /// The observations as JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let runs: Vec<String> = self.runs.iter().map(run_json).collect();
        format!(
            "{{\"format\":{},\"skipped\":{},\"seed\":{},\"corpus\":{},\"head\":{},\
             \"runs\":[\n{}\n]}}\n",
            json::quote(FORMAT),
            self.skipped,
            json::quote(&self.seed.to_string()),
            json::quote(&self.corpus),
            json::quote(&self.head),
            runs.join(",\n")
        )
    }

    /// Reads observations written by [`Observations::to_json`].
    ///
    /// # Errors
    ///
    /// Returns what is wrong: any missing field or field of the wrong type.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value = json::parse(text).map_err(|error| {
            format!("the observations are not JSON: line {}: {}", error.line, error.message)
        })?;
        if text_of(&value, "format")? != FORMAT {
            return Err(format!("the observations are not in format {FORMAT}"));
        }
        let skipped = value
            .get("skipped")
            .and_then(Value::as_bool)
            .ok_or("the observations have no `skipped`")?;
        let seed = text_of(&value, "seed")?
            .parse::<u64>()
            .map_err(|_| "the observations' seed is not a number")?;
        let runs =
            value.get("runs").and_then(Value::as_array).ok_or("the observations have no `runs`")?;
        let runs = runs.iter().map(run_from).collect::<Result<Vec<_>, _>>()?;
        let (corpus, head) = (text_of(&value, "corpus")?, text_of(&value, "head")?);
        Ok(Self { skipped, seed, corpus, head, runs })
    }

    /// Reads observations from a file.
    ///
    /// # Errors
    ///
    /// Returns what is wrong if the file is missing or unreadable.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("cannot read the observations {}: {error}", path.display()))?;
        Self::from_json(&text)
    }
}

fn run_json(run: &Run) -> String {
    let exit = run.exit.map_or_else(|| "null".to_owned(), |code| code.to_string());
    let files: Vec<String> = run
        .files
        .iter()
        .map(|(name, text)| {
            format!(
                "{}:{}",
                json::quote(name),
                text.as_deref().map_or_else(|| "null".to_owned(), json::quote)
            )
        })
        .collect();
    format!(
        "{{\"subject\":{},\"placement\":{},\"mode\":{},\"exit\":{exit},\
         \"stdout\":{},\"stderr\":{},\"files\":{{{}}}}}",
        json::quote(&run.subject),
        json::quote(&run.placement),
        json::quote(&run.mode),
        json::quote(&run.stdout),
        json::quote(&run.stderr),
        files.join(",")
    )
}

fn text_of(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("an observation has no text `{key}`"))
}

fn run_from(value: &Value) -> Result<Run, String> {
    let exit = match value.get("exit") {
        Some(Value::Null) => None,
        Some(Value::Number(digits)) => {
            Some(digits.parse::<i32>().map_err(|_| "an exit code is not a number")?)
        }
        _ => return Err("a run has no `exit`".to_owned()),
    };
    let mut files = BTreeMap::new();
    let members = value.get("files").and_then(Value::as_object).ok_or("a run has no `files`")?;
    for (name, content) in members {
        let content = match content {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
                return Err(format!("the file `{name}` of a run is not text"));
            }
        };
        let _replaced = files.insert(name.clone(), content);
    }
    Ok(Run {
        subject: text_of(value, "subject")?,
        placement: text_of(value, "placement")?,
        mode: text_of(value, "mode")?,
        exit,
        stdout: text_of(value, "stdout")?,
        stderr: text_of(value, "stderr")?,
        files,
    })
}

/// A fingerprint of the base corpus: the path and content of every tidy fixture and lint UI
/// file of the base, in order. The judge compares it with the one the observer planted.
///
/// # Errors
///
/// Returns what failed if the corpus cannot be read.
pub fn corpus_fingerprint(base: &Path) -> Result<(String, u32), String> {
    let mut hash = crate::dylint::FNV_OFFSET;
    let mut files: u32 = 0;
    for directory in [super::corpus::FIXTURE_DIRECTORY, super::lint::UI_DIRECTORY] {
        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(base.join(directory))
            .map_err(|error| format!("cannot read the base's {directory}: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("cannot read the base's {directory}: {error}"))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            let relative = format!("{directory}/{name}");
            let bytes = fs::read(base.join(&relative))
                .map_err(|error| format!("cannot read {relative}: {error}"))?;
            hash = crate::dylint::fnv1a(hash, relative.as_bytes());
            hash = crate::dylint::fnv1a(hash, &[0]);
            hash = crate::dylint::fnv1a(hash, &bytes);
            files += 1;
        }
    }
    Ok((format!("{hash:016x}"), files))
}

#[cfg(test)]
mod tests {
    use super::{Observations, Run, corpus_fingerprint};
    use std::collections::BTreeMap;

    fn run(exit: Option<i32>) -> Run {
        Run {
            subject: "xtask/fixtures/tidy/a.fires.fixture".to_owned(),
            placement: "crates/a/src/r1.rs".to_owned(),
            mode: "file".to_owned(),
            exit,
            stdout: "line \"one\"\n".to_owned(),
            stderr: "a.rs:1: error[a]: b\n".to_owned(),
            files: BTreeMap::from([
                ("missed.txt".to_owned(), None),
                ("outcomes.json".to_owned(), Some("{}".to_owned())),
            ]),
        }
    }

    // Goal: observations survive the trip through JSON exactly, and anything missing or of the
    // wrong type is an error.
    #[test]
    fn observations_round_trip() {
        let observations = Observations {
            skipped: false,
            seed: u64::MAX,
            corpus: "ab".to_owned(),
            head: "cd".to_owned(),
            runs: vec![run(Some(2)), run(None)],
        };
        assert_eq!(Observations::from_json(&observations.to_json()), Ok(observations.clone()));
        assert_eq!(observations.runs_of("xtask/fixtures/tidy/a.fires.fixture", "file").count(), 2);
        assert!(Observations::from_json("{}").is_err());
        assert!(
            Observations::from_json(
                &observations.to_json().replace("\"format\":\"2\"", "\"format\":\"1\"")
            )
            .is_err()
        );
        let broken = observations.to_json().replace("\"exit\":2", "\"exit\":\"2\"");
        assert!(Observations::from_json(&broken).unwrap_err().contains("exit"));
        assert!(Observations::read(std::path::Path::new("/no/such/file")).is_err());
    }

    // Goal: the fingerprint changes when any corpus file changes, and counts the files.
    #[test]
    fn the_corpus_has_a_fingerprint() {
        let scratch = crate::tidy::scratch::Scratch::new("ratchet-fingerprint");
        scratch.write("xtask/fixtures/tidy/a.fires.fixture", "a");
        scratch.write("lints/ui/main.rs", "fn main() {}");
        let (first, count) = corpus_fingerprint(scratch.root()).unwrap();
        assert_eq!(count, 2);
        scratch.write("lints/ui/main.rs", "fn main() { }");
        assert_ne!(corpus_fingerprint(scratch.root()).unwrap().0, first);
        let empty = crate::tidy::scratch::Scratch::new("ratchet-fingerprint-none");
        assert!(corpus_fingerprint(empty.root()).is_err());
    }
}
