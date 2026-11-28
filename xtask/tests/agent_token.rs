//! `agent-token.sh` signs a well-formed GitHub App JWT for the agents' app (ADR-0008) and the
//! CI app (ADR-0023), refuses an unnarrowed token and a bare JWT in CI and for the CI app, and
//! never acts as the agents' app in CI.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(command: &mut Command) -> String {
    let output = command.output().expect("command starts");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{command:?} failed: {stderr}");
    String::from_utf8(output.stdout).expect("output is UTF-8")
}

/// Splits `header.payload.signature`, base64url-decodes the parts and verifies the RS256
/// signature with openssl. Prints the header, the payload and openssl's verdict, one per line.
const VERIFY: &str = r#"
set -eu
IFS=. read -r h p s <<< "$2"
d() {
  x=$(printf %s "$1" | tr -- -_ +/)
  while [ $((${#x} % 4)) -ne 0 ]; do x="$x="; done
  printf %s "$x" | openssl base64 -d -A
}
d "$h"; echo; d "$p"; echo
d "$s" > "$1/signature"
openssl rsa -in "$1/key.pem" -pubout -out "$1/public.pem" 2> /dev/null
printf %s.%s "$h" "$p" | openssl dgst -sha256 -verify "$1/public.pem" -signature "$1/signature"
"#;

/// Stands in for curl: records the bearer and each request (URL and body), and answers the
/// installation lookup and the token request as GitHub would.
const FAKE_CURL: &str = r#"#!/usr/bin/env bash
set -eu
IFS= read -r header
printf '%s\n' "${header#Authorization: Bearer }" >> "$NAMI_FAKE_DIR/bearers"
url="${!#}"
data=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--data" ]; then data="$2"; fi
  shift
done
printf '%s %s\n' "$url" "$data" >> "$NAMI_FAKE_DIR/requests"
case "$url" in
  */repos/owner/name/installation) printf '{"id":7}' ;;
  */app/installations/7/access_tokens) printf '{"token":"ghs_fake"}' ;;
  *) exit 22 ;;
esac
"#;

/// A new temporary directory, unique per call: `cargo test` runs the tests as threads of one
/// process.
fn scratch(name: &str) -> PathBuf {
    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!("nami-{name}-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temporary directory is writable");
    directory
}

/// A throwaway RSA key at `path`.
fn generate_key(path: &Path) {
    let _ = run(Command::new("openssl").args(["genrsa", "-out"]).arg(path).arg("2048"));
}

/// The header, payload and verdict of `jwt`, verified against `directory/key.pem`.
fn verified(directory: &Path, jwt: &str) -> Vec<String> {
    let decoded =
        run(Command::new("bash").args(["-c", VERIFY, "verify"]).arg(directory).arg(jwt.trim()));
    let lines: Vec<String> = decoded.lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 3, "header, payload and verdict: {decoded}");
    lines
}

/// Runs `agent-token.sh` with `arguments` and only the given environment, in a temporary home
/// that is removed afterwards. Returns whether it succeeded, its stdout and its stderr.
fn run_script(arguments: &[&str], environment: &[(&str, &OsStr)]) -> (bool, String, String) {
    run_script_with_input(arguments, environment, "")
}

/// Like [`run_script`], with `input` on the script's stdin.
fn run_script_with_input(
    arguments: &[&str],
    environment: &[(&str, &OsStr)],
    input: &str,
) -> (bool, String, String) {
    let home = scratch("agent-token-home");
    let script = xtask::workspace_root().join("xtask/agent-token.sh");
    let mut command = Command::new(script);
    let _command = command
        .args(arguments)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (name, value) in environment {
        let _set = command.env(name, value);
    }
    let mut child = command.spawn().expect("the script starts");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    std::io::Write::write_all(&mut stdin, input.as_bytes()).expect("the input is written");
    drop(stdin);
    let output = child.wait_with_output().expect("the script ends");
    std::fs::remove_dir_all(&home).expect("temporary home is removable");
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("output is UTF-8");
    (output.status.success(), text(output.stdout), text(output.stderr))
}

/// Installs the fake curl in `directory/bin`, and returns a `PATH` that finds it first. The
/// fake records into `directory`, which `NAMI_FAKE_DIR` must name.
fn fake_github(directory: &Path) -> std::ffi::OsString {
    let fakes = directory.join("bin");
    std::fs::create_dir_all(&fakes).expect("fake directory is writable");
    std::fs::write(fakes.join("curl"), FAKE_CURL).expect("the fake curl is written");
    let executable = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(fakes.join("curl"), executable).expect("the fake is executable");
    let mut path = fakes.into_os_string();
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());
    path
}

/// The requests that the fake curl recorded in `directory`, one per line, or none.
fn recorded_requests(directory: &Path) -> String {
    std::fs::read_to_string(directory.join("requests")).unwrap_or_default()
}

// Goal: `agent-token.sh jwt` prints a JWT whose header and payload are what GitHub expects
// (RS256, the client ID as issuer, a 10-minute window) and whose signature verifies against the
// key. Method: sign with a throwaway key, then decode and verify independently with openssl.
#[test]
fn jwt_is_well_formed_and_verifies() {
    let directory = scratch("agent-token");
    let key = directory.join("key.pem");
    generate_key(&key);
    let environment =
        [("NAMI_AGENT_KEY", key.as_os_str()), ("NAMI_AGENT_CLIENT_ID", OsStr::new("Iv-test"))];
    let (ok, jwt, error) = run_script(&["jwt"], &environment);
    assert!(ok, "the agents' app signs outside CI: {error}");
    let lines = verified(&directory, &jwt);
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");

    assert_eq!(lines[0], r#"{"alg":"RS256","typ":"JWT"}"#);
    assert_eq!(lines[2], "Verified OK", "the signature must verify against the key");
    let numbers: Vec<i64> =
        lines[1].split([':', ',']).filter_map(|field| field.parse().ok()).collect();
    assert_eq!(numbers.len(), 2, "payload has iat and exp: {}", lines[1]);
    assert_eq!(numbers[1] - numbers[0], 600, "GitHub allows at most 10 minutes");
    assert!(lines[1].ends_with(r#""iss":"Iv-test"}"#), "issuer is the client ID");
}

/// Runs `agent-token.sh request` for the repository `owner/name`, outside CI, with
/// `NAMI_AGENT_PERMISSIONS` set to `permissions`, or unset for `None`.
fn request(permissions: Option<&str>) -> (bool, String, String) {
    let mut environment = vec![("NAMI_AGENT_REPO", OsStr::new("owner/name"))];
    environment.extend(permissions.map(|value| ("NAMI_AGENT_PERMISSIONS", OsStr::new(value))));
    run_script(&["request"], &environment)
}

// Goal: without `NAMI_AGENT_PERMISSIONS`, the token request names only the repository, so the
// token has all of the app's permissions, as before.
#[test]
fn the_token_request_names_only_the_repository_by_default() {
    let expected = "{\"repositories\":[\"name\"]}\n".to_owned();
    assert_eq!(request(None), (true, expected.clone(), String::new()));
    assert_eq!(request(Some("")).1, expected, "empty means unset");
}

// Goal: `NAMI_AGENT_PERMISSIONS` narrows the token: the request carries the object as given.
#[test]
fn permissions_narrow_the_token_request() {
    let (ok, body, _) = request(Some(r#"{"issues": "write", "actions": "read"}"#));
    assert!(ok, "a JSON object is accepted");
    let permissions = "{\"issues\":\"write\",\"actions\":\"read\"}";
    assert_eq!(body, format!("{{\"repositories\":[\"name\"],\"permissions\":{permissions}}}\n"));
}

// Goal: a value that is not a JSON object stops the script with a message that shows the form,
// and no request is printed.
#[test]
fn bad_permissions_are_an_error() {
    for bad in ["issues=write", "[\"issues\"]", "\"write\"", "{\"issues\":", "null", " "] {
        let (ok, body, error) = request(Some(bad));
        assert!(!ok, "{bad} is refused");
        assert!(body.is_empty(), "{bad}: nothing is printed: {body}");
        assert!(
            error.contains("NAMI_AGENT_PERMISSIONS must be a JSON object, such as"),
            "{bad}: {error}"
        );
    }
}

// Goal: `{}` narrows nothing, so it is refused everywhere, with a message that says so.
#[test]
fn an_empty_permission_object_is_an_error() {
    for empty in ["{}", " { } "] {
        let (ok, body, error) = request(Some(empty));
        assert!(!ok && body.is_empty(), "{empty} is refused: {body}");
        assert!(error.contains("NAMI_AGENT_PERMISSIONS is {}, which narrows nothing"), "{error}");
    }
}

/// The CI app's settings, with a key path that the commands under test never read.
fn ci_app() -> Vec<(&'static str, &'static OsStr)> {
    vec![
        ("NAMI_AGENT_APP", OsStr::new("ci")),
        ("NAMI_CI_KEY", OsStr::new("/the/key/is/not/read")),
        ("NAMI_CI_CLIENT_ID", OsStr::new("Iv-ci")),
    ]
}

// Goal: in CI, which GitHub Actions marks with `CI` or `GITHUB_ACTIONS` set to `true`, the
// agents' app never acts, whether `NAMI_AGENT_APP` is unset or `agent`: its key stays on the
// agents' machines (ADR-0023). Every command is refused before a key is read. Another value
// of the markers is not CI.
#[test]
fn the_agents_app_never_acts_in_ci() {
    let repository = ("NAMI_AGENT_REPO", OsStr::new("owner/name"));
    for marker in ["CI", "GITHUB_ACTIONS"] {
        for app in [None, Some("agent")] {
            for command in ["token", "env", "git-credential", "jwt", "request"] {
                let mut environment = vec![repository, (marker, OsStr::new("true"))];
                environment.extend(app.map(|app| ("NAMI_AGENT_APP", OsStr::new(app))));
                let (ok, body, error) = run_script(&[command], &environment);
                assert!(!ok && body.is_empty(), "{marker}, {app:?}, {command}: {body}");
                assert!(error.contains("the agents' app never acts in CI"), "{error}");
            }
        }
        let not_ci = [repository, (marker, OsStr::new("1"))];
        assert!(run_script(&["request"], &not_ci).0, "{marker}=1 is not GitHub Actions");
    }
}

// Goal: a token request without permissions, or with `{}`, is refused for the CI app, in CI
// and outside it (ADR-0023, rule 2). A narrowed request is printed.
#[test]
fn the_ci_app_must_narrow_the_token() {
    let narrowed = "{\"repositories\":[\"name\"],\"permissions\":{\"issues\":\"write\"}}\n";
    let repository = ("NAMI_AGENT_REPO", OsStr::new("owner/name"));
    for marker in [None, Some("CI"), Some("GITHUB_ACTIONS")] {
        let mut base = ci_app();
        base.push(repository);
        base.extend(marker.map(|marker| (marker, OsStr::new("true"))));
        for (permissions, expected) in [
            (None, "must narrow the token"),
            (Some(""), "must narrow the token"),
            (Some("{}"), "narrows nothing"),
        ] {
            let mut environment = base.clone();
            environment
                .extend(permissions.map(|value| ("NAMI_AGENT_PERMISSIONS", OsStr::new(value))));
            let (ok, body, error) = run_script(&["request"], &environment);
            assert!(!ok && body.is_empty(), "{marker:?}, {permissions:?}: refused: {body}");
            assert!(error.contains(expected), "{marker:?}, {permissions:?}: {error}");
        }
        let mut environment = base;
        environment.push(("NAMI_AGENT_PERMISSIONS", OsStr::new(r#"{"issues":"write"}"#)));
        let (ok, body, error) = run_script(&["request"], &environment);
        assert!(ok && error.is_empty(), "{marker:?}: a narrowed token is asked for: {error}");
        assert_eq!(body, narrowed, "{marker:?}");
    }
}

// Goal: `jwt` is refused in CI and for the CI app, because a JWT mints tokens with every
// permission of the app (ADR-0023, rule 2). Nothing is printed and no key is read.
#[test]
fn jwt_is_refused_in_ci_and_for_the_ci_app() {
    for marker in [None, Some("CI"), Some("GITHUB_ACTIONS")] {
        let mut environment = ci_app();
        environment.extend(marker.map(|marker| (marker, OsStr::new("true"))));
        let (ok, body, error) = run_script(&["jwt"], &environment);
        assert!(!ok && body.is_empty(), "{marker:?}: refused: {body}");
        assert!(error.contains("'jwt' is refused in CI and for the CI app"), "{error}");
    }
}

// Goal: `NAMI_AGENT_APP=ci` mints its token with a JWT signed by the CI app's key, with the CI
// app's client ID as issuer, while the agents' key and client ID are valid too, and asks for
// the narrowed permissions only. Method: curl is replaced by a fake that records the bearer
// and the requests; the bearer is decoded and verified independently with openssl.
#[test]
fn the_ci_app_signs_with_its_own_key() {
    let directory = scratch("ci-token");
    let key = directory.join("key.pem");
    let agent_key = directory.join("agent.pem");
    generate_key(&key);
    generate_key(&agent_key);
    let path = fake_github(&directory);
    let environment = [
        ("NAMI_AGENT_APP", OsStr::new("ci")),
        ("NAMI_CI_KEY", key.as_os_str()),
        ("NAMI_CI_CLIENT_ID", OsStr::new("Iv-ci")),
        ("NAMI_AGENT_KEY", agent_key.as_os_str()),
        ("NAMI_AGENT_CLIENT_ID", OsStr::new("Iv-agent")),
        ("NAMI_AGENT_REPO", OsStr::new("owner/name")),
        ("NAMI_AGENT_PERMISSIONS", OsStr::new(r#"{"issues":"write"}"#)),
        ("GITHUB_ACTIONS", OsStr::new("true")),
        ("NAMI_FAKE_DIR", directory.as_os_str()),
        ("PATH", path.as_os_str()),
    ];
    let (ok, token, error) = run_script(&["token"], &environment);
    assert!(ok && error.is_empty(), "a narrowed token is minted: {error}");
    assert_eq!(token, "ghs_fake\n");
    let bearers = std::fs::read_to_string(directory.join("bearers")).expect("bearers recorded");
    let bearers: Vec<&str> = bearers.lines().collect();
    assert_eq!(bearers.len(), 2, "one JWT for each call: {bearers:?}");
    assert_eq!(bearers[0], bearers[1], "the same JWT for both calls");
    let lines = verified(&directory, bearers[0]);
    assert!(lines[1].ends_with(r#""iss":"Iv-ci"}"#), "the CI app's client ID: {}", lines[1]);
    assert_eq!(lines[2], "Verified OK", "signed with the CI app's key");
    let requests = std::fs::read_to_string(directory.join("requests")).expect("requests");
    let expected = "https://api.github.com/repos/owner/name/installation \n\
                    https://api.github.com/app/installations/7/access_tokens \
                    {\"repositories\":[\"name\"],\"permissions\":{\"issues\":\"write\"}}\n";
    assert_eq!(requests, expected);
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");
}

// Goal: `NAMI_AGENT_APP=ci` refuses `env` and `git-credential`, and fails without its own key
// or client ID even when the agents' key and client ID are valid: it never falls back to
// them. An unknown app fails.
#[test]
fn the_ci_app_never_falls_back_to_the_agents_key() {
    let directory = scratch("ci-fallback");
    let agent_key = directory.join("agent.pem");
    generate_key(&agent_key);
    let nowhere = directory.join("no-ci-key.pem");
    let app = ("NAMI_AGENT_APP", OsStr::new("ci"));
    let ci_key = ("NAMI_CI_KEY", agent_key.as_os_str());
    let ci_client = ("NAMI_CI_CLIENT_ID", OsStr::new("Iv-ci"));
    let agent = ("NAMI_AGENT_KEY", agent_key.as_os_str());
    let agent_client = ("NAMI_AGENT_CLIENT_ID", OsStr::new("Iv-agent"));
    let narrowed = ("NAMI_AGENT_PERMISSIONS", OsStr::new(r#"{"issues":"write"}"#));
    for command in [&["env"][..], &["git-credential", "get"]] {
        let (exported, out, refusal) = run_script(command, &[app, ci_key, ci_client, narrowed]);
        let refused = !exported && out.is_empty() && refusal.contains("for the agents' app only");
        assert!(refused, "{command:?}: {refusal}");
    }
    let empty_key = ("NAMI_CI_KEY", OsStr::new(""));
    let missing_key = ("NAMI_CI_KEY", nowhere.as_os_str());
    let cases: [(Vec<(&str, &OsStr)>, &str); 5] = [
        (vec![app, ci_client, agent, agent_client, narrowed], "NAMI_CI_KEY is not set"),
        (vec![app, empty_key, ci_client, agent, agent_client, narrowed], "NAMI_CI_KEY is not set"),
        (vec![app, ci_key, agent, agent_client, narrowed], "NAMI_CI_CLIENT_ID is not set"),
        (
            vec![app, missing_key, ci_client, agent, agent_client, narrowed],
            "cannot read the ci app's private key",
        ),
        (vec![("NAMI_AGENT_APP", OsStr::new("other"))], "unknown app 'other'"),
    ];
    for (environment, expected) in cases {
        let (ok, body, error) = run_script(&["token"], &environment);
        assert!(!ok && body.is_empty() && error.contains(expected), "{expected}: {error}");
    }
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");
}

/// What git sends a credential helper before it talks to GitHub over HTTPS.
const CREDENTIAL_REQUEST: &str = "protocol=https\nhost=github.com\n\n";

// Goal: `git-credential get` and `env` never hand out a token that a refusal stopped. They
// mint inside a command substitution, where bash does not keep `set -e`, so a refusal there
// must still end the script (review of #64). Each case exits non-zero, prints no credential or
// token, and the fake GitHub records no token request. Method: a valid key and a fake curl
// that would mint; a narrowed control run shows that it would.
#[test]
fn git_credential_and_env_never_skip_a_refusal() {
    let directory = scratch("credential-refusals");
    let key = directory.join("key.pem");
    generate_key(&key);
    let path = fake_github(&directory);
    let common = [
        ("NAMI_AGENT_REPO", OsStr::new("owner/name")),
        ("NAMI_FAKE_DIR", directory.as_os_str()),
        ("PATH", path.as_os_str()),
        ("NAMI_AGENT_KEY", key.as_os_str()),
        ("NAMI_CI_KEY", key.as_os_str()),
        ("NAMI_CI_CLIENT_ID", OsStr::new("Iv-ci")),
    ];
    let ci = ("NAMI_AGENT_APP", OsStr::new("ci"));
    let in_actions = ("GITHUB_ACTIONS", OsStr::new("true"));
    let in_ci = ("CI", OsStr::new("true"));
    let unnarrowed = [None, Some(""), Some("{}")];
    for permissions in unnarrowed {
        assert_refused(&directory, &common, &[ci], permissions);
        assert_refused(&directory, &common, &[ci, in_actions], permissions);
        assert_refused(&directory, &common, &[in_ci], permissions);
    }
    for permissions in [Some("{}"), Some("issues=write")] {
        assert_refused(&directory, &common, &[], permissions);
    }
    let mut environment = common.to_vec();
    environment.push(("NAMI_AGENT_PERMISSIONS", OsStr::new(r#"{"issues":"write"}"#)));
    let (minted, credential, refusal) =
        run_script_with_input(&["git-credential", "get"], &environment, CREDENTIAL_REQUEST);
    assert!(minted && refusal.is_empty(), "the narrowed control mints: {refusal}");
    assert_eq!(credential, "username=x-access-token\npassword=ghs_fake\n");
    let (exported, exports, failure) = run_script(&["env"], &environment);
    assert!(exported && exports.starts_with("export GH_TOKEN='ghs_fake'\n"), "{failure}");
    assert_eq!(recorded_requests(&directory).matches("access_tokens").count(), 2, "two mints");
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");
}

/// Asserts that `git-credential get` and `env`, with `common` and `setup` in the environment
/// and `NAMI_AGENT_PERMISSIONS` set to `permissions` (unset for `None`), exit non-zero, print
/// no credential or token, and leave no token request in the fake GitHub in `directory`.
fn assert_refused(
    directory: &Path,
    common: &[(&str, &OsStr)],
    setup: &[(&str, &OsStr)],
    permissions: Option<&str>,
) {
    for command in [&["git-credential", "get"][..], &["env"]] {
        let mut environment = common.to_vec();
        environment.extend_from_slice(setup);
        environment.extend(permissions.map(|value| ("NAMI_AGENT_PERMISSIONS", OsStr::new(value))));
        let (ok, out, error) = run_script_with_input(command, &environment, CREDENTIAL_REQUEST);
        let case = format!("{setup:?}, {permissions:?}, {command:?}");
        assert!(!ok && error.starts_with("agent-token: "), "{case}: refused: {error}");
        for leak in ["password=", "GH_TOKEN", "ghs_fake"] {
            assert!(!out.contains(leak), "{case}: no {leak} in {out}");
        }
        let requests = recorded_requests(directory);
        assert!(!requests.contains("access_tokens"), "{case}: no token minted: {requests}");
    }
}
