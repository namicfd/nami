//! `agent-token.sh` signs a well-formed GitHub App JWT for the agents' app (ADR-0008) and the
//! CI app (ADR-0023), and refuses an unnarrowed token in CI.

use std::ffi::OsStr;
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

// Goal: `agent-token.sh jwt` prints a JWT whose header and payload are what GitHub expects
// (RS256, the client ID as issuer, a 10-minute window) and whose signature verifies against the
// key. Method: sign with a throwaway key, then decode and verify independently with openssl.
#[test]
fn jwt_is_well_formed_and_verifies() {
    let directory = std::env::temp_dir().join(format!("nami-agent-token-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temporary directory is writable");
    let key = directory.join("key.pem");
    let _ = run(Command::new("openssl").args(["genrsa", "-out"]).arg(&key).arg("2048"));
    let script = xtask::workspace_root().join("xtask/agent-token.sh");
    let jwt = run(Command::new(script)
        .arg("jwt")
        .env("NAMI_AGENT_KEY", &key)
        .env("NAMI_AGENT_CLIENT_ID", "Iv-test-client"));
    let decoded =
        run(Command::new("bash").args(["-c", VERIFY, "verify"]).arg(&directory).arg(&jwt));
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");

    let lines: Vec<&str> = decoded.lines().collect();
    assert_eq!(lines.len(), 3, "header, payload and verdict: {decoded}");
    assert_eq!(lines[0], r#"{"alg":"RS256","typ":"JWT"}"#);
    assert_eq!(lines[2], "Verified OK", "the signature must verify against the key");
    let numbers: Vec<i64> =
        lines[1].split([':', ',']).filter_map(|field| field.parse().ok()).collect();
    assert_eq!(numbers.len(), 2, "payload has iat and exp: {}", lines[1]);
    assert_eq!(numbers[1] - numbers[0], 600, "GitHub allows at most 10 minutes");
    assert!(lines[1].ends_with(r#""iss":"Iv-test-client"}"#), "issuer is the client ID");
}

/// Runs `agent-token.sh` with `arguments` and only the given environment, in a temporary home
/// that is removed afterwards. Returns whether it succeeded, its stdout and its stderr.
fn run_script(arguments: &[&str], environment: &[(&str, &OsStr)]) -> (bool, String, String) {
    // Unique per call, since `cargo test` runs the tests as threads of one process.
    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let home =
        std::env::temp_dir().join(format!("nami-agent-token-home-{}-{call}", std::process::id()));
    std::fs::create_dir_all(&home).expect("temporary directory is writable");
    let script = xtask::workspace_root().join("xtask/agent-token.sh");
    let mut command = Command::new(script);
    let _command = command
        .args(arguments)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home);
    for (name, value) in environment {
        let _set = command.env(name, value);
    }
    let output = command.output().expect("the script starts");
    std::fs::remove_dir_all(&home).expect("temporary home is removable");
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("output is UTF-8");
    (output.status.success(), text(output.stdout), text(output.stderr))
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

// Goal: a token request without permissions, or with `{}`, is refused in CI, which GitHub
// Actions marks with `CI` or `GITHUB_ACTIONS` set to `true`, and for the CI app outside CI
// (ADR-0023, rule 2). A narrowed request is printed as outside CI, and another value of the
// markers is not CI.
#[test]
fn ci_and_the_ci_app_must_narrow_the_token() {
    let narrowed = "{\"repositories\":[\"name\"],\"permissions\":{\"issues\":\"write\"}}\n";
    let repository = ("NAMI_AGENT_REPO", OsStr::new("owner/name"));
    let ci_app = vec![
        ("NAMI_AGENT_APP", OsStr::new("ci")),
        ("NAMI_CI_KEY", OsStr::new("/the/key/is/not/read")),
        ("NAMI_CI_CLIENT_ID", OsStr::new("Iv-ci")),
    ];
    let markers =
        [vec![("CI", OsStr::new("true"))], vec![("GITHUB_ACTIONS", OsStr::new("true"))], ci_app];
    for marker in markers {
        let name = marker[0].0;
        for (permissions, expected) in [
            (None, "must narrow the token"),
            (Some(""), "must narrow the token"),
            (Some("{}"), "narrows nothing"),
        ] {
            let mut environment = marker.clone();
            environment.push(repository);
            environment
                .extend(permissions.map(|value| ("NAMI_AGENT_PERMISSIONS", OsStr::new(value))));
            let (ok, body, error) = run_script(&["request"], &environment);
            assert!(!ok && body.is_empty(), "{name}, {permissions:?}: refused: {body}");
            assert!(error.contains(expected), "{name}, {permissions:?}: {error}");
        }
        let mut environment = marker.clone();
        environment.push(repository);
        environment.push(("NAMI_AGENT_PERMISSIONS", OsStr::new(r#"{"issues":"write"}"#)));
        let (ok, body, error) = run_script(&["request"], &environment);
        assert!(ok && error.is_empty(), "{name}: a narrowed token is asked for: {error}");
        assert_eq!(body, narrowed, "{name}");
    }
    for marker in ["CI", "GITHUB_ACTIONS"] {
        let not_ci = [repository, (marker, OsStr::new("1"))];
        assert!(run_script(&["request"], &not_ci).0, "{marker}=1 is not GitHub Actions");
    }
}

// Goal: `NAMI_AGENT_APP=ci` signs with the CI app's key and client ID, refuses `env`, and
// fails without its own key or client ID even when the agents' key and client ID are valid:
// it never falls back to them. An unknown app fails. Method: one throwaway key for each app;
// the JWT is decoded and verified independently with openssl against the CI app's key.
#[test]
fn the_ci_app_is_selected_and_kept_apart() {
    let directory = std::env::temp_dir().join(format!("nami-ci-token-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temporary directory is writable");
    let key = directory.join("key.pem");
    let agent_key = directory.join("agent.pem");
    for path in [&key, &agent_key] {
        let _ = run(Command::new("openssl").args(["genrsa", "-out"]).arg(path).arg("2048"));
    }
    let nowhere = directory.join("no-ci-key.pem");
    let app = ("NAMI_AGENT_APP", OsStr::new("ci"));
    let ci_key = ("NAMI_CI_KEY", key.as_os_str());
    let ci_client = ("NAMI_CI_CLIENT_ID", OsStr::new("Iv-ci"));
    let agent = ("NAMI_AGENT_KEY", agent_key.as_os_str());
    let agent_client = ("NAMI_AGENT_CLIENT_ID", OsStr::new("Iv-agent"));
    let complete = [app, ci_key, ci_client, agent, agent_client];
    let (signed, jwt, signing) = run_script(&["jwt"], &complete);
    assert!(signed, "the CI app's key signs: {signing}");
    let decoded =
        run(Command::new("bash").args(["-c", VERIFY, "verify"]).arg(&directory).arg(jwt.trim()));
    let lines: Vec<&str> = decoded.lines().collect();
    assert_eq!(lines.len(), 3, "header, payload and verdict: {decoded}");
    assert!(lines[1].ends_with(r#""iss":"Iv-ci"}"#), "the CI app's client ID: {}", lines[1]);
    assert_eq!(lines[2], "Verified OK", "signed with the CI app's key");
    let (exported, _, refusal) = run_script(&["env"], &complete);
    assert!(!exported && refusal.contains("for the agents' app only"), "{refusal}");
    let empty_key = ("NAMI_CI_KEY", OsStr::new(""));
    let missing_key = ("NAMI_CI_KEY", nowhere.as_os_str());
    let cases: [(Vec<(&str, &OsStr)>, &str); 5] = [
        (vec![app, ci_client, agent, agent_client], "NAMI_CI_KEY is not set"),
        (vec![app, empty_key, ci_client, agent, agent_client], "NAMI_CI_KEY is not set"),
        (vec![app, ci_key, agent, agent_client], "NAMI_CI_CLIENT_ID is not set"),
        (vec![app, missing_key, ci_client, agent, agent_client], "the ci app's private key"),
        (vec![("NAMI_AGENT_APP", OsStr::new("other"))], "unknown app 'other'"),
    ];
    for (environment, expected) in cases {
        let (ok, body, error) = run_script(&["jwt"], &environment);
        assert!(!ok && body.is_empty() && error.contains(expected), "{expected}: {error}");
    }
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");
}
