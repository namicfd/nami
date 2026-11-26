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

/// Runs `agent-token.sh request` for the repository `owner/name`, with `NAMI_AGENT_PERMISSIONS`
/// set to `permissions`, or unset for `None`. Returns its status, stdout and stderr.
fn request(permissions: Option<&str>) -> (bool, String, String) {
    let script = xtask::workspace_root().join("xtask/agent-token.sh");
    let mut command = Command::new(script);
    let _ = command.arg("request").env("NAMI_AGENT_REPO", "owner/name");
    let _ = match permissions {
        Some(permissions) => command.env("NAMI_AGENT_PERMISSIONS", permissions),
        None => command.env_remove("NAMI_AGENT_PERMISSIONS"),
    };
    let output = command.output().expect("the script starts");
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("output is UTF-8");
    (output.status.success(), text(output.stdout), text(output.stderr))
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
    for bad in ["issues=write", "[\"issues\"]", "\"write\"", "{\"issues\":", "null"] {
        let (ok, body, error) = request(Some(bad));
        assert!(!ok, "{bad} is refused");
        assert!(body.is_empty(), "{bad}: nothing is printed: {body}");
        assert!(
            error.contains("NAMI_AGENT_PERMISSIONS must be a JSON object, such as"),
            "{bad}: {error}"
        );
    }
}
