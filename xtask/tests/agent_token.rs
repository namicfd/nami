//! `agent-token.sh` signs a well-formed GitHub App JWT (ADR-0008).

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
fn run_script(
    arguments: &[&str],
    environment: &[(&str, &std::path::Path)],
) -> (bool, String, String) {
    let home = std::env::temp_dir().join(format!("nami-agent-token-home-{}", std::process::id()));
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
    let output = command.output().expect("command starts");
    std::fs::remove_dir_all(&home).expect("temporary home is removable");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.success(), stdout, String::from_utf8_lossy(&output.stderr).into_owned())
}

// Goal: `NAMI_AGENT_APP=reviewer` signs with the reviewer's key and client ID, never with the
// agents' key or client ID, refuses `env` and `git-credential`, and fails without its client
// ID; an unknown app fails. Method: one throwaway key for the reviewer, the agents' key path
// pointing nowhere and the agents' client ID set to another value; the JWT is decoded and
// verified independently with openssl.
#[test]
fn the_reviewer_app_is_selected_and_kept_apart() {
    let directory =
        std::env::temp_dir().join(format!("nami-reviewer-token-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temporary directory is writable");
    let key = directory.join("key.pem");
    let _ = run(Command::new("openssl").args(["genrsa", "-out"]).arg(&key).arg("2048"));
    let reviewer = std::path::Path::new("reviewer");
    let client = std::path::Path::new("Iv-reviewer");
    let agent_client = std::path::Path::new("Iv-agent");
    let nowhere = directory.join("no-agent-key.pem");
    let base = [
        ("NAMI_AGENT_APP", reviewer),
        ("NAMI_REVIEWER_KEY", key.as_path()),
        ("NAMI_AGENT_KEY", nowhere.as_path()),
        ("NAMI_AGENT_CLIENT_ID", agent_client),
    ];
    let mut with_client = base.to_vec();
    with_client.push(("NAMI_REVIEWER_CLIENT_ID", client));
    let (signed, jwt, signing) = run_script(&["jwt"], &with_client);
    assert!(signed, "the reviewer's key signs: {signing}");
    let decoded =
        run(Command::new("bash").args(["-c", VERIFY, "verify"]).arg(&directory).arg(jwt.trim()));
    let lines: Vec<&str> = decoded.lines().collect();
    assert_eq!(lines.len(), 3, "header, payload and verdict: {decoded}");
    assert!(
        lines[1].ends_with(r#""iss":"Iv-reviewer"}"#),
        "the reviewer's client ID: {}",
        lines[1]
    );
    assert_eq!(lines[2], "Verified OK", "signed with the reviewer's key");
    let (no_client, _, no_client_error) = run_script(&["jwt"], &base);
    let unset = no_client_error.contains("NAMI_REVIEWER_CLIENT_ID is not set");
    assert!(!no_client && unset, "{no_client_error}");
    for command in ["env", "git-credential"] {
        let (refused, _, refusal) = run_script(&[command], &with_client);
        assert!(!refused && refusal.contains("does not commit or push"), "{command}: {refusal}");
    }
    let missing = [
        ("NAMI_AGENT_APP", reviewer),
        ("NAMI_REVIEWER_CLIENT_ID", client),
        ("NAMI_AGENT_KEY", key.as_path()),
    ];
    let (fell_back, _, fallback) = run_script(&["jwt"], &missing);
    let refused_agent_key = !fell_back && fallback.contains("reviewer app's private key");
    assert!(refused_agent_key, "never the agents' key: {fallback}");
    let (unknown, _, unknown_error) =
        run_script(&["jwt"], &[("NAMI_AGENT_APP", std::path::Path::new("other"))]);
    assert!(!unknown && unknown_error.contains("unknown app 'other'"), "{unknown_error}");
    std::fs::remove_dir_all(&directory).expect("temporary directory is removable");
}
