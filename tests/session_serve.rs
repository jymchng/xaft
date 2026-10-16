//! End-to-end tests for `xaft session serve` (parity matrix row 13).
//!
//! `serve` never returns on its own — there is no `--once` — so it cannot be
//! tested by asserting on a process exit. These tests spawn the real binary,
//! which is the only way to prove the two things the in-crate tests cannot:
//!
//! * the readiness line reaches a **pipe**. `crates/xaft-cli`'s tests call
//!   `start_server` directly, so they prove the *document* is right; a
//!   block-buffered `println!` would still never arrive at a supervisor, and
//!   only a spawned child reading its own stdout proves otherwise.
//! * the socket the readiness line names is the socket that answers. The
//!   in-crate test builds its own URL from `local_addr()`; this one uses the
//!   string the process actually printed.
//!
//! Nothing here is mocked. The child is given a throwaway `HOME` and a scratch
//! data directory so it cannot read or write an operator's configuration or
//! sessions. `XAFT_CORE__DATA_DIR` is the documented override from
//! `ConfigLoader`, not a test-only hook.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long to wait for a readiness line or an HTTP response before failing.
///
/// Generous on purpose: the child boots the whole runtime, and this test must
/// fail loudly rather than flakily on a loaded CI box.
const PATIENCE: Duration = Duration::from_secs(60);

/// A scratch directory the child is pointed at for every path it resolves.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("xaft-session-serve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A `serve` child that is killed and reaped even when an assertion fails.
///
/// Without this, a failing assertion would leave a listening process behind and
/// the next run would be debugging a stray port instead of the failure.
struct Serving {
    child: Child,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `xaft session serve --port 0` against `scratch`, with `env` applied.
///
/// `XAFT_SESSION_AUTH_TOKEN` is removed from the inherited environment before
/// anything is added: the auth token has to be exactly what this test says it
/// is, not whatever the operator's shell happened to export.
fn serve(scratch: &Scratch, env: &[(&str, &str)]) -> Serving {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xaft"));
    command
        .args(["session", "serve", "--port", "0"])
        .current_dir(&scratch.dir)
        .env("HOME", &scratch.dir)
        .env("XDG_CONFIG_HOME", scratch.dir.join("config"))
        .env("XDG_DATA_HOME", scratch.dir.join("data"))
        .env("XAFT_CORE__DATA_DIR", scratch.dir.join("xaft-data"))
        .env_remove("XAFT_SESSION_AUTH_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let child = command.spawn().expect("spawn `xaft session serve`");
    Serving { child }
}

/// The `url` out of the one readiness line `serve` prints, once it arrives.
///
/// The line is read from a *pipe*, and that is the assertion: a readiness line
/// the process buffered rather than flushed would never arrive, and this would
/// time out. The document's exact shape — `status`, `url` and agenthicc's
/// `schedule_service` — is pinned by `readiness_line_is_the_three_key_document`
/// in `xaft-cli`; this only has to find the URL in it.
fn await_ready(child: &mut Child) -> String {
    let stdout = child.stdout.take().expect("`serve` stdout is piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let deadline = Instant::now() + PATIENCE;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(line) = rx.recv_timeout(left) else {
            break;
        };
        if line.contains("\"status\":\"ready\"") {
            return url_in(&line).unwrap_or_else(|| panic!("readiness line has no url: {line}"));
        }
    }
    panic!("`xaft session serve` printed no readiness line within {PATIENCE:?}");
}

/// The value of the `"url"` field of a readiness line, without a JSON parser.
///
/// The document's shape is pinned elsewhere, so a `serde_json` dependency on the
/// root package would buy nothing here.
fn url_in(line: &str) -> Option<String> {
    let rest = line.split("\"url\":\"").nth(1)?;
    Some(rest.split('"').next()?.to_string())
}

/// One raw `GET` against `url`, returning the whole response.
///
/// Hand-written for the same reason the transport's own tests are: there is no
/// HTTP client in the dependency graph, and a test is not a reason to add one.
fn get(url: &str, path: &str, bearer: Option<&str>) -> String {
    let authority = url.strip_prefix("http://").expect("readiness url is http");
    let auth = bearer
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\n{auth}Connection: close\r\n\r\n");

    let mut stream = TcpStream::connect(authority).expect("connect to the served port");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    String::from_utf8_lossy(&response).into_owned()
}

#[test]
fn serve_prints_a_flushed_readiness_line_and_answers_on_the_port_it_names() {
    let scratch = Scratch::new("ready");
    let mut serving = serve(&scratch, &[]);

    let url = await_ready(&mut serving.child);
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the default bind must be loopback: {url}"
    );
    assert!(
        !url.ends_with(":0"),
        "the readiness line must name the bound port, not the requested one: {url}"
    );

    let response = get(&url, "/health", None);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "health was {response}"
    );
    assert!(
        response.contains("xaft-session"),
        "health must identify the service: {response}"
    );
}

#[test]
fn serve_requires_the_token_it_was_given_on_every_route() {
    let scratch = Scratch::new("token");
    let mut serving = serve(&scratch, &[("XAFT_SESSION_AUTH_TOKEN", "e2e-secret")]);

    let url = await_ready(&mut serving.child);

    let refused = get(&url, "/health", None);
    assert!(
        refused.starts_with("HTTP/1.1 401"),
        "unauthenticated /health was {refused}"
    );
    assert!(
        !refused.contains("e2e-secret"),
        "the refusal must not echo the token: {refused}"
    );

    let allowed = get(&url, "/health", Some("e2e-secret"));
    assert!(
        allowed.starts_with("HTTP/1.1 200 OK"),
        "authenticated /health was {allowed}"
    );
    assert!(
        !allowed.contains("e2e-secret"),
        "a served payload must never carry the token: {allowed}"
    );
}

#[test]
fn serve_refuses_a_non_loopback_host_without_a_token_before_it_binds() {
    let scratch = Scratch::new("refused");
    let mut command = Command::new(env!("CARGO_BIN_EXE_xaft"));
    command
        .args(["session", "serve", "--host", "0.0.0.0"])
        .current_dir(&scratch.dir)
        .env("HOME", &scratch.dir)
        .env("XDG_CONFIG_HOME", scratch.dir.join("config"))
        .env("XAFT_CORE__DATA_DIR", scratch.dir.join("xaft-data"))
        .env_remove("XAFT_SESSION_AUTH_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = command
        .output()
        .expect("run `xaft session serve --host 0.0.0.0`");

    assert_eq!(
        output.status.code(),
        Some(2),
        "a refused bind must be a usage error; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--auth-token"),
        "the refusal must name the fix: {stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("\"status\":\"ready\""),
        "a refused bind must not also claim to be ready"
    );
}
