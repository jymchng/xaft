//! Handler tests for `xaft mcp`.
//!
//! `crates/xaft-cli/src/commands/mcp.rs` had **zero** test coverage: 126
//! coverable lines, none of them executed. The `mcp` family is entirely
//! config-file manipulation, so this is the cheapest real behaviour in the
//! workspace to cover — no network, no MCP server, no session manager.
//!
//! What these tests pin, beyond "the call returns":
//!
//! * The **redaction contract**. `list`, `get` and `auth` all print JSON
//!   derived from a config that can hold a bearer token and env headers.
//!   `auth_is_configured_for_a_token_env_reference` and
//!   `get_never_prints_a_token_value` exist because a leak here would put a
//!   secret on stdout.
//! * The **exit-code convention** that looks like a bug: every config failure
//!   prints `error: …` and returns `ExitCode::SUCCESS`. The module docs say
//!   this reproduces agenthicc deliberately, so
//!   `a_failed_add_reports_the_error_but_exits_success` asserts it rather than
//!   leaving it as an accident.
//! * The **exact stdout of all seven subcommands**, pinned through
//!   `render_mcp` in the "Exact stdout" section at the end. Before
//!   `render_mcp` existed, nothing in this file looked at stdout at all — which
//!   is how the bullet above could name a test that did not exist for so long
//!   without anything failing.
//! * **Round-tripping through the config file**. `add` then `list` then
//!   `remove` are driven through `handle_mcp`, and each step asserts against
//!   what was actually written to disk, so the write-back path cannot rot.
//!
//! Note on `auth`: it deliberately ignores the `--scope` flags and resolves the
//! *default* project config from the process's current directory, which is why
//! `CwdGuard` exists and why every test that touches `auth` holds the lock.
//! It is that inconsistency itself that `auth_ignores_the_scope_flags` pins.
//!
//! Note on the two config readers — **every test in this file is
//! lenient-path-only**.
//!
//! Each one drives `render_mcp`, which reaches `read_mcp_servers`: the
//! *lenient* per-stanza reader that `--config` selects, that treats a missing
//! file as an empty list, and that degrades a stanza it cannot build to
//! `McpEntry::Invalid`. None of it goes through the strict typed
//! `xaft_config::ConfigLoader`, which `src/main.rs` runs *before dispatching any
//! command* and which rejects a whole file it cannot deserialise.
//!
//! The consequence is a false green waiting to happen: every assertion below
//! stays true while the startup loader rejects the very file the test wrote, so
//! a green run here is evidence about `read_mcp_servers` and about nothing
//! else. `xaft version` exits 3 on a file whose `mcp list --config` printed
//! happily; that gap is what goals 67 and 68 fell into, and it is why the
//! loader side is guarded in `crates/xaft-config/tests/config_integration.rs`
//! through `ConfigLoader::load` with `project_dir` at a real `.xaft/xaft.toml`
//! — the shape that actually runs at CLI startup. One test in this file,
//! `the_lenient_seam_stays_green_while_the_startup_loader_rejects_the_same_file`
//! in the section at the end, pins *both* verdicts for a single file so the
//! asymmetry cannot be documented away.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use xaft_cli::args::{
    McpAddArgs, McpGetArgs, McpListArgs, McpNameArgs, McpRemoveArgs, McpScopeArgs, McpSubcommand,
};
use xaft_cli::commands::mcp::{handle_mcp, render_mcp};
use xaft_cli::mcp::read_mcp_servers;
use xaft_config::{CliOverrides, ConfigLoader};
use xaft_runtime::ExitCode;

/// Serialise every test in this file.
///
/// `auth` resolves its config from the process's current directory rather than
/// from its own arguments, so tests that exercise it must not race each other
/// over `current_dir`/`HOME`.
fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// A temp directory plus a `--config` pointing at a file inside it.
///
/// Passing an explicit path is what lets every non-`auth` subcommand be tested
/// without touching `$HOME` or the cwd at all: `config_path` returns
/// `scope.explicit` outright, before it looks at cwd or home.
///
/// That is also this file's defining limitation. `--config` reaches
/// `read_mcp_servers` — the lenient reader — and *only* that reader, so a test
/// built on `Cfg` cannot observe a defect that lives in `ConfigLoader::load`.
/// Treat every assertion made through `Cfg::scope()` as scoped to the lenient
/// path, and back any claim about the loader with a real `ConfigLoader::load`.
struct Cfg {
    dir: tempfile::TempDir,
    path: PathBuf,
}

impl Cfg {
    fn new() -> Cfg {
        Cfg::with_body("")
    }

    fn with_body(body: &str) -> Cfg {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("xaft.toml");
        std::fs::write(&path, body).expect("seed config");
        Cfg { dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The lenient seam every non-`auth` test in this file goes through.
    ///
    /// `config: Some(..)` is `Scope::explicit`, so `config_path` returns the
    /// path without consulting the cwd or `$HOME` — and `render_mcp` then reads
    /// it with `read_mcp_servers`, never with `ConfigLoader::load`. Nothing
    /// below the handler sees the typed config.
    fn scope(&self) -> McpScopeArgs {
        McpScopeArgs {
            global: false,
            project: false,
            config: Some(self.path.clone()),
        }
    }

    /// The raw config text, for asserting what a subcommand wrote back.
    fn raw(&self) -> String {
        std::fs::read_to_string(&self.path).expect("read back config")
    }
}

fn add_args(cfg: &Cfg, name: &str, target: &str) -> McpAddArgs {
    McpAddArgs {
        name: name.to_string(),
        target: target.to_string(),
        transport: "stdio".to_string(),
        args: vec![],
        token_env: None,
        no_auto_connect: false,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
        scope: cfg.scope(),
    }
}

async fn run(sub: McpSubcommand) -> ExitCode {
    handle_mcp(&sub)
        .await
        .expect("mcp handler must not return Err")
}

// ── list ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_on_a_missing_config_file_reports_no_servers() {
    let _lock = serial();
    // A path that does not exist at all. `read_mcp_servers` treats a missing
    // file as an empty list rather than an error, which is what makes
    // `mcp list` usable before anything has been configured.
    //
    // Built inline rather than through `Cfg` because the point is an absent
    // path. That also means it carries none of `Cfg`'s warnings by
    // inheritance, so: this is the lenient path, exactly like every other
    // test in this file. `ConfigLoader::load` treats a missing file as
    // "use the defaults" and also exits 0, so unlike the two-stanza tests
    // here nothing is at stake — but the general point stands and this test
    // proves nothing about the startup loader.
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = McpScopeArgs {
        global: false,
        project: false,
        config: Some(dir.path().join("absent.toml")),
    };
    let code = run(McpSubcommand::List(McpListArgs { json: false, scope })).await;
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn list_renders_one_line_per_valid_server() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
enabled = true

[[mcp.client]]
name = "beta"
url = "https://beta.example/mcp"
transport = "http"
enabled = false
"#,
    );
    let code = run(McpSubcommand::List(McpListArgs {
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    let names: Vec<String> = read_mcp_servers(cfg.path())
        .expect("read back")
        .iter()
        .map(|e| e.name().to_string())
        .collect();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
}

#[tokio::test]
async fn list_keeps_an_unparseable_stanza_visible_as_invalid() {
    let _lock = serial();
    // `gamma` has no `name`, so it cannot become a Valid entry — but the
    // listing must still show it rather than aborting the whole command. This
    // is the `McpEntry::Invalid` arm of `list`, and it is agenthicc's
    // "one typo must not hide the other four servers" behaviour.
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"

[[mcp.client]]
transport = "http"
"#,
    );
    let entries = read_mcp_servers(cfg.path()).expect("read back");
    assert!(
        entries
            .iter()
            .any(|e| matches!(e, xaft_cli::mcp::McpEntry::Invalid { .. })),
        "expected an Invalid entry, got {entries:?}"
    );

    let code = run(McpSubcommand::List(McpListArgs {
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn list_json_shape_carries_the_path_and_a_servers_array() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
token_env = "ALPHA_TOKEN"
"#,
    );
    let code = run(McpSubcommand::List(McpListArgs {
        json: true,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    // Assert the redacted projection directly: this is the exact value `list`
    // serialises into `{"path": …, "servers": […]}`.
    let entries = read_mcp_servers(cfg.path()).expect("read back");
    let redacted: Vec<serde_json::Value> = entries
        .iter()
        .map(xaft_cli::mcp::McpEntry::redacted)
        .collect();
    assert_eq!(redacted.len(), 1);
    assert_eq!(redacted[0]["name"], "alpha");
    assert!(
        !serde_json::to_string(&redacted)
            .unwrap()
            .contains("secret-value"),
        "the redacted projection leaked a token value"
    );
}

// ── add ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn add_writes_a_stdio_server_that_list_then_reads_back() {
    let _lock = serial();
    let cfg = Cfg::new();
    let code = run(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;
    assert_eq!(code, ExitCode::SUCCESS);

    let raw = cfg.raw();
    assert!(raw.contains("alpha"), "name not written: {raw}");
    assert!(raw.contains("alpha-server"), "command not written: {raw}");

    // The round trip that matters: the handler wrote a file the reader parses.
    let entries = read_mcp_servers(cfg.path()).expect("read back");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "alpha");
}

#[tokio::test]
async fn adding_a_duplicate_name_is_refused_rather_than_overwriting() {
    let _lock = serial();
    let cfg = Cfg::new();
    run(McpSubcommand::Add(add_args(&cfg, "alpha", "first-server"))).await;
    let code = run(McpSubcommand::Add(add_args(&cfg, "alpha", "second-server"))).await;

    // `add_mcp_server` (crates/xaft-cli/src/mcp.rs:437) returns
    // "MCP server already exists" instead of overwriting, so a re-add with a
    // typo'd target cannot silently discard a working server's command. The
    // exit code still follows the family convention and stays SUCCESS.
    assert_eq!(code, ExitCode::SUCCESS);

    let entries = read_mcp_servers(cfg.path()).expect("read back");
    assert_eq!(entries.len(), 1, "re-adding must not duplicate the entry");
    let raw = cfg.raw();
    assert!(
        raw.contains("first-server") && !raw.contains("second-server"),
        "the refused add must leave the original entry untouched: {raw}"
    );
}

#[tokio::test]
async fn add_records_a_token_env_reference_and_never_the_token_itself() {
    let _lock = serial();
    let cfg = Cfg::new();
    let mut args = add_args(&cfg, "alpha", "alpha-server");
    args.token_env = Some("ALPHA_TOKEN".to_string());
    let code = run(McpSubcommand::Add(args)).await;
    assert_eq!(code, ExitCode::SUCCESS);

    let raw = cfg.raw();
    assert!(
        raw.contains("ALPHA_TOKEN"),
        "the env reference must be persisted: {raw}"
    );
}

#[tokio::test]
async fn add_stores_a_non_stdio_target_as_a_url() {
    let _lock = serial();
    let cfg = Cfg::new();
    let mut args = add_args(&cfg, "remote", "https://remote.example/mcp");
    args.transport = "http".to_string();
    let code = run(McpSubcommand::Add(args)).await;
    assert_eq!(code, ExitCode::SUCCESS);

    let raw = cfg.raw();
    assert!(
        raw.contains("https://remote.example/mcp"),
        "an http transport must persist the target as a url: {raw}"
    );
}

#[tokio::test]
async fn add_rejects_an_invalid_name_but_exits_success() {
    let _lock = serial();
    let cfg = Cfg::new();
    // The validator requires a leading alphanumeric; `-leading-dash` fails.
    let code = run(McpSubcommand::Add(add_args(
        &cfg,
        "-not-a-valid-name",
        "alpha-server",
    )))
    .await;
    // Deliberately SUCCESS: see the module docs. agenthicc prints and returns.
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        !cfg.raw().contains("not-a-valid-name"),
        "a rejected server must not be written: {}",
        cfg.raw()
    );
}

#[tokio::test]
async fn add_rejects_an_empty_target() {
    let _lock = serial();
    let cfg = Cfg::new();
    let code = run(McpSubcommand::Add(add_args(&cfg, "alpha", "   "))).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(read_mcp_servers(cfg.path()).unwrap().len(), 0);
}

#[tokio::test]
async fn add_rejects_an_unsupported_transport() {
    let _lock = serial();
    let cfg = Cfg::new();
    let mut args = add_args(&cfg, "alpha", "alpha-server");
    args.transport = "carrier-pigeon".to_string();
    let code = run(McpSubcommand::Add(args)).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(read_mcp_servers(cfg.path()).unwrap().len(), 0);
}

#[tokio::test]
async fn add_honours_no_auto_connect() {
    let _lock = serial();
    let cfg = Cfg::new();
    let mut args = add_args(&cfg, "alpha", "alpha-server");
    args.no_auto_connect = true;
    run(McpSubcommand::Add(args)).await;
    let raw = cfg.raw();
    assert!(
        raw.contains("auto_connect = false"),
        "--no-auto-connect was not persisted: {raw}"
    );
}

// ── get ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_finds_a_configured_server() {
    let _lock = serial();
    let cfg = Cfg::new();
    run(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;

    let code = run(McpSubcommand::Get(McpGetArgs {
        name: "alpha".into(),
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn get_reports_a_missing_server_without_failing() {
    let _lock = serial();
    let cfg = Cfg::new();
    let code = run(McpSubcommand::Get(McpGetArgs {
        name: "absent".into(),
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    // Same convention as the rest of the family: report, do not fail.
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn get_never_prints_a_token_value() {
    let _lock = serial();
    // A config that carries a literal token alongside the env reference.
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
token = "super-secret-literal"
token_env = "ALPHA_TOKEN"
"#,
    );
    let code = run(McpSubcommand::Get(McpGetArgs {
        name: "alpha".into(),
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    // `get` prints `entry.redacted()`; assert the redaction actually redacts
    // rather than trusting the name of the function.
    let entries = read_mcp_servers(cfg.path()).expect("read back");
    let rendered = serde_json::to_string(&entries[0].redacted()).expect("serialise");
    assert!(
        !rendered.contains("super-secret-literal"),
        "a literal token reached the redacted projection: {rendered}"
    );
}

// ── remove ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn remove_deletes_a_configured_server() {
    let _lock = serial();
    let cfg = Cfg::new();
    run(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;
    run(McpSubcommand::Add(add_args(&cfg, "beta", "beta-server"))).await;
    assert_eq!(read_mcp_servers(cfg.path()).unwrap().len(), 2);

    let code = run(McpSubcommand::Remove(McpRemoveArgs {
        name: "alpha".into(),
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    let entries = read_mcp_servers(cfg.path()).expect("read back");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), "beta", "remove deleted the wrong entry");
}

#[tokio::test]
async fn remove_leaves_other_keys_in_the_file_intact() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
# a comment the user wrote
[core]
log_level = "debug"

[[mcp.client]]
name = "alpha"
command = "alpha-server"
"#,
    );
    run(McpSubcommand::Remove(McpRemoveArgs {
        name: "alpha".into(),
        scope: cfg.scope(),
    }))
    .await;

    let raw = cfg.raw();
    assert!(
        raw.contains("log_level"),
        "an unrelated key was destroyed by write-back: {raw}"
    );
}

#[tokio::test]
async fn removing_an_absent_server_is_reported_not_fatal() {
    let _lock = serial();
    let cfg = Cfg::new();
    let code = run(McpSubcommand::Remove(McpRemoveArgs {
        name: "absent".into(),
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

// ── disconnect / logout ───────────────────────────────────────────────────

#[tokio::test]
async fn disconnect_is_explained_rather_than_performed() {
    let _lock = serial();
    let code = run(McpSubcommand::Disconnect(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn logout_states_that_environment_backed_credentials_are_untouched() {
    let _lock = serial();
    let code = run(McpSubcommand::Logout(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

// ── auth ──────────────────────────────────────────────────────────────────

/// Move the process into `dir` for the guard's lifetime.
///
/// `auth` ignores its own scope flags and resolves the default project config
/// from the cwd, so there is no way to point it at a file except by changing
/// where the process thinks it is. Serially, behind the shared lock.
struct CwdGuard {
    previous: PathBuf,
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
}

impl CwdGuard {
    fn new() -> CwdGuard {
        let lock = serial();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::current_dir().expect("cwd");
        // SAFETY: `lock` is held for this guard's lifetime and released only
        // after `Drop` puts the cwd back.
        unsafe { std::env::set_current_dir(dir.path()) };
        CwdGuard {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        // SAFETY: the lock is still held by this guard.
        unsafe { std::env::set_current_dir(&self.previous) };
    }
}

#[tokio::test]
async fn auth_reports_not_configured_when_the_server_has_no_credentials() {
    let _cwd = CwdGuard::new();
    std::fs::write(_cwd._dir.path().join(".xaft").join("xaft.toml"), "").ok();

    let code = run(McpSubcommand::Auth(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn auth_reports_not_configured_for_an_unknown_server() {
    let _cwd = CwdGuard::new();
    let code = run(McpSubcommand::Auth(McpNameArgs {
        name: "absent".into(),
    }))
    .await;
    // "not found or invalid" is printed, and — per the family convention — the
    // exit code stays SUCCESS.
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn auth_is_configured_for_a_token_env_reference() {
    let _cwd = CwdGuard::new();
    let nested = _cwd._dir.path().join(".xaft");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(
        nested.join("xaft.toml"),
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
token_env = "ALPHA_TOKEN"
"#,
    )
    .expect("seed");

    let code = run(McpSubcommand::Auth(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn auth_ignores_the_scope_flags_because_it_takes_none() {
    // `McpNameArgs` has only a `name`, so `auth` structurally cannot honour
    // `--global`/`--config`. It always resolves the default project config.
    // This test documents that asymmetry rather than asserting a wish: it runs
    // `auth` with a config that exists ONLY as an explicit path elsewhere and
    // confirms the server is nonetheless reported as not found.
    let _lock = serial();
    let cfg = Cfg::new();
    run(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;
    assert_eq!(read_mcp_servers(cfg.path()).unwrap().len(), 1);

    let code = run(McpSubcommand::Auth(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
}

// ── Exact stdout ────────────────────────────────────────────────────────────
//
// Everything above this line asserts that a call *returns*; nothing asserted
// what it *said*. That is why the module docs could name
// `a_failed_add_reports_the_error_but_exits_success` and get away with it: the
// suite never looked at stdout, so a test that did not exist would not fail.
//
// `render_mcp` is the seam that makes stdout assertable without capturing the
// process's own stdout. The helper below runs a subcommand and returns the
// exact text, and every test here pins BOTH that text and the exit code.

/// Run a subcommand through `render_mcp`, returning `(exit code, exact stdout)`.
async fn capture(sub: McpSubcommand) -> (ExitCode, String) {
    let mut out = String::new();
    let code = render_mcp(&sub, &mut out)
        .await
        .expect("mcp render must not return Err");
    (code, out)
}

/// The exact redacted JSON body of a minimal stdio server named `alpha`.
const ALPHA_REDACTED_COMPACT: &str = concat!(
    r#"{"args":[],"auto_connect":true,"command":"alpha-server","enabled":true,"#,
    r#""env_headers":{},"name":"alpha","reconnect_attempts":3,"#,
    r#""reconnect_delay_seconds":1.0,"transport":"stdio","url":"<stdio>"}"#,
);

#[tokio::test]
async fn list_stdout_on_an_empty_config_is_the_exact_empty_state_line() {
    let _lock = serial();
    let cfg = Cfg::new();
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!("No MCP servers configured in {}\n", cfg.path().display())
    );
}

#[tokio::test]
async fn list_stdout_prints_one_exact_line_per_server() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"

[[mcp.client]]
name = "beta"
transport = "HTTP+SSE"
url = "https://mcp.example.com/sse"
enabled = false
"#,
    );
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    // `alpha` omits `transport` and is reported as `stdio` — the serde default,
    // not a guess. `beta` shows the transport normalised and `enabled=false`
    // honoured, so both halves of the format string are pinned.
    assert_eq!(
        out,
        "alpha: transport=stdio enabled=true\nbeta: transport=http+sse enabled=false\n"
    );
}

/// Row 32's stated contract, end to end on the `list` output: a stanza that
/// cannot be built becomes a visible `invalid` entry instead of aborting the
/// whole listing.
///
/// `alpha` is ordinary and `nameless` has no `name`, so the two lines below are
/// what a user with one broken stanza actually sees. Before this pair of fixes
/// the process never got here: `src/main.rs` loads config before dispatch, so
/// the same file exited 3 with `missing field 'name'` and printed nothing at
/// all. The loader side is guarded separately in
/// `crates/xaft-config/tests/config_integration.rs`
/// (`mcp_client_name_defaults_to_empty_via_project_config_discovery`), because
/// this file's `--config` seam deliberately bypasses it.
#[tokio::test]
async fn list_stdout_reports_a_nameless_stanza_as_invalid_and_keeps_the_valid_one() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"

[[mcp.client]]
transport = "http"
"#,
    );
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "one bad stanza must not fail the command"
    );
    assert_eq!(
        out,
        "alpha: transport=stdio enabled=true\n<invalid>: transport=unknown enabled=true\n"
    );
}

/// The same two stanzas through `--json`, which is the machine-readable form
/// row 32 names: `{"name": ..., "status": "invalid"}` for the broken one.
#[tokio::test]
async fn list_json_stdout_marks_the_nameless_stanza_invalid() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"

[[mcp.client]]
transport = "http"
"#,
    );
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: true,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    let payload: serde_json::Value =
        serde_json::from_str(out.trim()).expect("stdout is one JSON object");
    assert_eq!(payload["path"], cfg.path().display().to_string());
    let servers = payload["servers"].as_array().expect("servers array");
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0]["name"], "alpha");
    assert_eq!(servers[1]["name"], "<invalid>");
    assert_eq!(servers[1]["status"], "invalid");
    assert_eq!(
        servers[1].as_object().expect("object").len(),
        2,
        "an invalid entry carries exactly name and status"
    );
}

#[tokio::test]
async fn list_json_stdout_is_one_compact_object_naming_the_path() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
"#,
    );
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: true,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!(
            r#"{{"path":"{}","servers":[{ALPHA_REDACTED_COMPACT}]}}"#,
            cfg.path().display()
        ) + "\n"
    );
}

#[tokio::test]
async fn add_stdout_names_the_server_the_scope_and_the_path() {
    let _lock = serial();
    let cfg = Cfg::new();
    let (code, out) = capture(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;
    assert_eq!(code, ExitCode::SUCCESS);
    // Names are rendered with agenthicc's `!r` single quotes.
    assert_eq!(
        out,
        format!(
            "Added MCP server 'alpha' (project) to {}\n",
            cfg.path().display()
        )
    );
}

/// The exit-code convention that reads like a bug: a rejected `add` prints
/// `error: …` and still exits `SUCCESS`. The module docs claim this test
/// exists; before this one it did not, so the claim was unbacked.
#[tokio::test]
async fn a_failed_add_reports_the_error_but_exits_success() {
    let _lock = serial();
    let cfg = Cfg::new();
    // An empty name fails `ServerSpec::validated()` before any write.
    let (code, out) = capture(McpSubcommand::Add(add_args(&cfg, "", "alpha-server"))).await;
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "agenthic exits 0 on a config error"
    );
    assert_eq!(
        out,
        "error: server name must start with a letter or number and contain only \
         letters, numbers, '.', '_' or '-'\n"
    );
    // Nothing was written, so the claim is not merely cosmetic.
    assert_eq!(cfg.raw(), "");

    // Adding the same name twice is the other soft failure: the second add is
    // refused rather than duplicating the stanza, and again exits SUCCESS.
    capture(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;
    let (code, out) = capture(McpSubcommand::Add(add_args(&cfg, "alpha", "other-server"))).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "error: MCP server already exists: alpha\n");
}

#[tokio::test]
async fn get_stdout_is_indented_json_with_no_token() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
token = "sk-literal-secret-value"
"#,
    );
    let (code, out) = capture(McpSubcommand::Get(McpGetArgs {
        name: "alpha".into(),
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        concat!(
            "{\n",
            "  \"args\": [],\n",
            "  \"auto_connect\": true,\n",
            "  \"command\": \"alpha-server\",\n",
            "  \"enabled\": true,\n",
            "  \"env_headers\": {},\n",
            "  \"name\": \"alpha\",\n",
            "  \"reconnect_attempts\": 3,\n",
            "  \"reconnect_delay_seconds\": 1.0,\n",
            "  \"transport\": \"stdio\",\n",
            "  \"url\": \"<stdio>\"\n",
            "}\n",
        )
    );
    assert!(!out.contains("sk-literal-secret-value"), "token leaked");
}

#[tokio::test]
async fn get_json_stdout_is_the_same_document_compacted() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
"#,
    );
    let (code, out) = capture(McpSubcommand::Get(McpGetArgs {
        name: "alpha".into(),
        json: true,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, format!("{ALPHA_REDACTED_COMPACT}\n"));
}

#[tokio::test]
async fn get_stdout_for_a_missing_server_is_the_exact_error_line() {
    let _lock = serial();
    let cfg = Cfg::new();
    let (code, out) = capture(McpSubcommand::Get(McpGetArgs {
        name: "ghost".into(),
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "error: MCP server not found: ghost\n");
}

#[tokio::test]
async fn remove_stdout_confirms_the_removal_by_name_and_path() {
    let _lock = serial();
    let cfg = Cfg::new();
    capture(McpSubcommand::Add(add_args(&cfg, "alpha", "alpha-server"))).await;
    let (code, out) = capture(McpSubcommand::Remove(McpRemoveArgs {
        name: "alpha".into(),
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!("Removed MCP server 'alpha' from {}\n", cfg.path().display())
    );
    assert!(read_mcp_servers(cfg.path()).unwrap().is_empty());
}

#[tokio::test]
async fn disconnect_stdout_is_the_exact_session_scoped_notice() {
    let _lock = serial();
    let (code, out) = capture(McpSubcommand::Disconnect(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "MCP server 'alpha' is session-scoped; run /mcp disconnect alpha inside the active xaft session.\n"
    );
}

#[tokio::test]
async fn logout_stdout_is_the_exact_no_persisted_credential_notice() {
    let _lock = serial();
    let (code, out) = capture(McpSubcommand::Logout(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "No persisted MCP credential was removed for 'alpha'; environment-backed credentials remain unchanged.\n"
    );
}

#[tokio::test]
async fn auth_stdout_for_an_unknown_server_names_the_resolved_config() {
    let cwd = CwdGuard::new();
    let resolved = cwd._dir.path().join(".xaft/xaft.toml");
    let (code, out) = capture(McpSubcommand::Auth(McpNameArgs {
        name: "ghost".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!(
            "error: MCP server not found or invalid: ghost (no such server in {})\n",
            resolved.display()
        )
    );
}

#[tokio::test]
async fn auth_stdout_is_the_exact_status_object_when_configured() {
    let cwd = CwdGuard::new();
    std::fs::create_dir_all(cwd._dir.path().join(".xaft")).unwrap();
    // `has_auth` counts a server as configured when it has at least one
    // `env_headers` entry, or a `token` that is a `${ENV_VAR}` *reference*.
    std::fs::write(
        cwd._dir.path().join(".xaft").join("xaft.toml"),
        concat!(
            "[[mcp.client]]\n",
            "name = \"alpha\"\n",
            "command = \"alpha-server\"\n",
            "env_headers = { Authorization = \"ALPHA_TOKEN\" }\n",
        ),
    )
    .unwrap();

    let (code, out) = capture(McpSubcommand::Auth(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "{\"server\":\"alpha\",\"status\":\"configured\"}\n");
}

#[tokio::test]
async fn auth_stdout_reports_not_configured_for_a_server_with_no_credentials() {
    let cwd = CwdGuard::new();
    std::fs::create_dir_all(cwd._dir.path().join(".xaft")).unwrap();
    std::fs::write(
        cwd._dir.path().join(".xaft").join("xaft.toml"),
        "[[mcp.client]]\nname = \"alpha\"\ncommand = \"alpha-server\"\n",
    )
    .unwrap();

    let (code, out) = capture(McpSubcommand::Auth(McpNameArgs {
        name: "alpha".into(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "{\"server\":\"alpha\",\"status\":\"not_configured\"}\n"
    );
}

/// An `[[mcp.client]]` stanza missing `transport` is a *valid* server that
/// defaults to `stdio`, so it is listed by name rather than degraded to
/// `McpEntry::Invalid`. This pins the observable consequence of the serde
/// default at the CLI layer: before it, the lenient reader could not parse the
/// stanza and printed `transport=unknown` / `status: invalid` instead.
#[tokio::test]
async fn a_stanza_omitting_transport_is_listed_as_stdio_not_as_invalid() {
    let _lock = serial();
    let cfg = Cfg::with_body(
        r#"
[[mcp.client]]
name = "alpha"
command = "alpha-server"
"#,
    );
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: false,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "alpha: transport=stdio enabled=true\n");
    assert!(!out.contains("unknown"), "degraded to Invalid: {out}");

    // The JSON form agrees: a real redacted server, not a status stub.
    let (_code, json) = capture(McpSubcommand::List(McpListArgs {
        json: true,
        scope: cfg.scope(),
    }))
    .await;
    assert_eq!(
        json,
        format!(
            r#"{{"path":"{}","servers":[{ALPHA_REDACTED_COMPACT}]}}"#,
            cfg.path().display()
        ) + "\n"
    );
}
// ── One file, two readers ──────────────────────────────────────────────

/// The false green, made executable: the same bytes get opposite verdicts
/// from the two readers, and only this file's seam is exercised by the rest of
/// the suite.
///
/// `beta`'s `enabled` is a string where the typed config wants a bool. That is
/// deliberately *not* one of the omissions goals 67 and 68 gave serde defaults
/// to (`transport`, then `name`) — those two are why a partially-specified
/// stanza now starts the process instead of bricking it. A type mismatch is
/// still a whole-file rejection on the strict side, and this test keeps that
/// visible rather than letting it be forgotten: if `McpClientConfig` is ever
/// loosened far enough to accept it, this assertion fails and says so.
///
/// The lenient reader never deserialises the stanza as a struct it must
/// commit to, so it degrades `beta` alone to `McpEntry::Invalid` and still
/// prints `alpha` beside it — exit 0, the same output every other test in this
/// file would see. In a real process neither line is ever printed, because
/// `main` runs `ConfigLoader::load` first and exits 3.
#[tokio::test]
async fn the_lenient_seam_stays_green_while_the_startup_loader_rejects_the_same_file() {
    let _lock = serial();
    let dir = tempfile::tempdir().expect("tempdir");

    // The layout `ConfigLoader` discovers at startup, not an explicit
    // `--config`: `.xaft/xaft.toml` found by walking up from `project_dir`.
    let project = dir.path().join("project");
    let project_config = project.join(".xaft/xaft.toml");
    std::fs::create_dir_all(project_config.parent().unwrap()).unwrap();
    std::fs::write(
        &project_config,
        "[[mcp.client]]\nname = \"alpha\"\ncommand = \"alpha-server\"\n\n\
         [[mcp.client]]\nname = \"beta\"\ncommand = \"beta-server\"\nenabled = \"yes\"\n",
    )
    .unwrap();

    // (a) The startup path. `project_dir` is set explicitly so the walk cannot
    // pick up a second config from the real cwd, which is what makes this
    // assertion about *this* stanza rather than about the environment.
    let err = ConfigLoader::load(&CliOverrides {
        project_dir: Some(project.clone()),
        ..Default::default()
    })
    .expect_err("the typed loader rejects the file, so no command can start");
    assert!(
        err.to_string().contains("boolean"),
        "the rejection must come from this stanza, not from something ambient: {err}"
    );

    // (b) The lenient path, which is what every other test here does.
    let (code, out) = capture(McpSubcommand::List(McpListArgs {
        json: false,
        scope: McpScopeArgs {
            global: false,
            project: false,
            config: Some(project_config),
        },
    }))
    .await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out, "alpha: transport=stdio enabled=true\nbeta: transport=unknown enabled=true\n",
        "the lenient reader degrades one stanza and keeps the other"
    );
}
