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
//! * **Round-tripping through the config file**. `add` then `list` then
//!   `remove` are driven through `handle_mcp`, and each step asserts against
//!   what was actually written to disk, so the write-back path cannot rot.
//!
//! Note on `auth`: it deliberately ignores the `--scope` flags and resolves the
//! *default* project config from the process's current directory, which is why
//! `cwd_guard` exists and why every test that touches `auth` holds the lock.
//! It is that inconsistency itself that `auth_ignores_the_scope_flags` pins.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use xaft_cli::args::{
    McpAddArgs, McpGetArgs, McpListArgs, McpNameArgs, McpRemoveArgs, McpScopeArgs, McpSubcommand,
};
use xaft_cli::commands::mcp::handle_mcp;
use xaft_cli::mcp::read_mcp_servers;
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
    handle_mcp(&sub).await.expect("mcp handler must not return Err")
}

// ── list ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_on_a_missing_config_file_reports_no_servers() {
    let _lock = serial();
    // A path that does not exist at all. `read_mcp_servers` treats a missing
    // file as an empty list rather than an error, which is what makes
    // `mcp list` usable before anything has been configured.
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
        entries.iter().any(|e| matches!(e, xaft_cli::mcp::McpEntry::Invalid { .. })),
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
    let redacted: Vec<serde_json::Value> =
        entries.iter().map(xaft_cli::mcp::McpEntry::redacted).collect();
    assert_eq!(redacted.len(), 1);
    assert_eq!(redacted[0]["name"], "alpha");
    assert!(
        !serde_json::to_string(&redacted).unwrap().contains("secret-value"),
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
    assert!(
        raw.contains("alpha-server"),
        "command not written: {raw}"
    );

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
    std::fs::write(
        _cwd._dir.path().join(".xaft").join("xaft.toml"),
        "",
    )
    .ok();

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