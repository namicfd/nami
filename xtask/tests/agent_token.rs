//! `agent-token.sh` signs a well-formed GitHub App JWT (ADR-0008).

use std::path::Path;
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
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-token.sh");
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
