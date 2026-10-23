//! Handler for `xaft mcp` subcommands.
//!
//! Port of `agenthicc.cli.commands.mcp`. Seven subcommands only read or edit
//! the config file and never open a socket. The other three — `connect`,
//! `refresh` and `doctor` (parity rows 36, 40, 41) — do what agenthicc's
//! `_live_cli_manager` does: build a short-lived session manager
//! ([`crate::mcp_live`]) over the project config's servers, behind a network
//! guard derived from `[security]`, act, print JSON, and shut it down.
//!
//! | command | prints |
//! | --- | --- |
//! | `connect NAME` | the status record, `json.dumps(…, sort_keys=True)` |
//! | `refresh NAME` | the catalog snapshot, or `{"status": "unavailable"}` |
//! | `doctor [NAME] [--json]` | one report per server, indented (or compact with `--json`) |
//!
//! The JSON is rendered through [`crate::pyjson`], so it is byte-for-byte what
//! Python's `json.dumps` produces (`", "` / `": "` separators, sorted keys,
//! ASCII escapes).
//!
//! # The one behaviour that looks like a bug and is not
//!
//! `add`, `remove`, `get` and `auth` print `error: …` and return
//! [`ExitCode::SUCCESS`]. That is exactly what agenthicc does: every one of its
//! `mcp_*` handlers catches `McpConfigError`, prints, and `return`s without
//! raising. A caller checking the exit code sees success for a failed `add`.
//!
//! It is reproduced rather than corrected, because parity is the whole point
//! of this command family and because a script written against agenthicc's
//! behaviour should keep working. The failure is still visible in stdout, so
//! nothing is silently swallowed.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use xaft_config::McpClientConfig;
use xaft_runtime::ExitCode;

use crate::args::{
    McpAddArgs, McpDoctorArgs, McpGetArgs, McpListArgs, McpLiveArgs, McpNameArgs, McpRemoveArgs,
    McpScopeArgs, McpSubcommand,
};
use crate::error::XaftError;
use crate::mcp::{
    McpEntry, McpError, Scope, ServerSpec, add_mcp_server, config_path, read_mcp_servers,
    remove_mcp_server,
};
use crate::mcp_live::{McpSessionManager, NetworkGuard};
use crate::pyjson;

/// Execute `xaft mcp <subcommand>`, printing its output to stdout.
pub async fn handle_mcp(subcommand: &McpSubcommand) -> Result<ExitCode, XaftError> {
    let mut out = String::new();
    let code = render_mcp(subcommand, &mut out).await?;
    print!("{out}");
    Ok(code)
}

/// Render `xaft mcp <subcommand>` into `out` instead of printing it.
///
/// Every user-facing line of this command family is written here, so the exact
/// stdout can be asserted in a test without capturing the process's own stdout.
/// The wording is byte-identical to what `handle_mcp` used to print directly:
/// this is a seam for testing, not a change in behaviour.
pub async fn render_mcp(
    subcommand: &McpSubcommand,
    out: &mut String,
) -> Result<ExitCode, XaftError> {
    match subcommand {
        McpSubcommand::List(args) => list(args, out),
        McpSubcommand::Add(args) => add(args, out),
        McpSubcommand::Get(args) => get(args, out),
        McpSubcommand::Remove(args) => remove(args, out),
        McpSubcommand::Disconnect(args) => disconnect(args, out),
        McpSubcommand::Auth(args) => auth(args, out),
        McpSubcommand::Logout(args) => logout(args, out),
        McpSubcommand::Connect(args) => connect(args, out).await,
        McpSubcommand::Refresh(args) => refresh(args, out).await,
        McpSubcommand::Doctor(args) => doctor(args, out).await,
    }
}

/// agenthicc renders names with Python's `!r`, which single-quotes them. The
/// strings are user-supplied, so a name containing a quote is rejected upstream
/// by [`crate::mcp`]'s validator; the escaping here is for defence in depth,
/// not for names that can actually reach it.
fn repr(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn scope_of(args: &McpScopeArgs) -> Scope {
    Scope {
        global: args.global,
        project: args.project,
        explicit: args.config.clone(),
    }
}

/// Resolve the config path, or print agenthicc's error and stop.
fn resolve(args: &McpScopeArgs) -> Result<PathBuf, McpError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    config_path(scope_of(args), &cwd, home.as_deref())
}

fn list(args: &McpListArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e, out),
    };
    let entries = match read_mcp_servers(&path) {
        Ok(entries) => entries,
        Err(e) => return soft(e, out),
    };

    if args.json {
        let servers: Vec<serde_json::Value> = entries.iter().map(McpEntry::redacted).collect();
        let payload = serde_json::json!({ "path": path.display().to_string(), "servers": servers });
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string(&payload).map_err(json_err)?
        );
        return Ok(ExitCode::SUCCESS);
    }

    if entries.is_empty() {
        let _ = writeln!(out, "No MCP servers configured in {}", path.display());
        return Ok(ExitCode::SUCCESS);
    }

    for entry in &entries {
        match entry {
            McpEntry::Valid(c) => {
                let _ = writeln!(
                    out,
                    "{}: transport={} enabled={}",
                    c.name,
                    c.normalized_transport(),
                    c.enabled
                );
            }
            // agenthicc prints the same shape for an unparseable stanza, so a
            // `--json` consumer and a human see the same `status` field.
            McpEntry::Invalid { name } => {
                let _ = writeln!(out, "{name}: transport=unknown enabled=true");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn add(args: &McpAddArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e, out),
    };
    let spec = match (ServerSpec {
        name: args.name.clone(),
        transport: args.transport.clone(),
        target: args.target.clone(),
        args: args.args.clone(),
        token_env: args.token_env.clone(),
        auto_connect: !args.no_auto_connect,
        reconnect_attempts: args.reconnect_attempts,
        reconnect_delay_seconds: args.reconnect_delay_seconds,
    })
    .validated()
    {
        Ok(spec) => spec,
        Err(e) => return soft(e, out),
    };

    let scope = if args.scope.global {
        "global"
    } else {
        "project"
    };
    match add_mcp_server(&path, &spec) {
        Ok(name) => {
            let _ = writeln!(
                out,
                "Added MCP server {} ({scope}) to {}",
                repr(&name),
                path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => soft(e, out),
    }
}

fn get(args: &McpGetArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e, out),
    };
    let entries = match read_mcp_servers(&path) {
        Ok(entries) => entries,
        Err(e) => return soft(e, out),
    };

    let Some(entry) = entries.iter().find(|e| e.name() == args.name) else {
        let _ = writeln!(out, "error: MCP server not found: {}", args.name);
        return Ok(ExitCode::SUCCESS);
    };

    let payload = entry.redacted();
    let rendered = if args.json {
        serde_json::to_string(&payload)
    } else {
        serde_json::to_string_pretty(&payload)
    }
    .map_err(json_err)?;
    let _ = writeln!(out, "{rendered}");
    Ok(ExitCode::SUCCESS)
}

fn remove(args: &McpRemoveArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e, out),
    };
    match remove_mcp_server(&path, &args.name) {
        Ok(()) => {
            let _ = writeln!(
                out,
                "Removed MCP server {} from {}",
                repr(&args.name),
                path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => soft(e, out),
    }
}

fn disconnect(args: &McpNameArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    // agenthicc's whole implementation is this one print.
    let _ = writeln!(
        out,
        "MCP server {} is session-scoped; run /mcp disconnect {} inside the active xaft session.",
        repr(&args.name),
        args.name
    );
    Ok(ExitCode::SUCCESS)
}

fn logout(args: &McpNameArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    // agenthicc's whole implementation is this one print. The reason it says
    // "no persisted credential" rather than "done" is that credentials here are
    // environment-backed by design, and the CLI refuses to mutate the
    // environment a credential is read from.
    let _ = writeln!(
        out,
        "No persisted MCP credential was removed for {}; environment-backed credentials remain unchanged.",
        repr(&args.name)
    );
    Ok(ExitCode::SUCCESS)
}

fn auth(args: &McpNameArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = match config_path(Scope::default(), &cwd, home.as_deref()) {
        Ok(path) => path,
        Err(e) => return soft(e, out),
    };

    let entries = match read_mcp_servers(&path) {
        Ok(entries) => entries,
        Err(e) => return soft(e, out),
    };

    let Some(McpEntry::Valid(config)) = entries.iter().find(|e| e.name() == args.name) else {
        // agenthicc reports the exception text from a failed `next()`. This
        // says the same thing in words a Rust user can act on.
        let _ = writeln!(
            out,
            "error: MCP server not found or invalid: {} (no such server in {})",
            args.name,
            path.display()
        );
        return Ok(ExitCode::SUCCESS);
    };

    let status = if McpClientConfig::has_auth(config) {
        "configured"
    } else {
        "not_configured"
    };
    // Compact and key-sorted, matching agenthicc's `json.dumps` here.
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string(&serde_json::json!({ "server": args.name, "status": status }))
            .map_err(json_err)?
    );
    Ok(ExitCode::SUCCESS)
}

/// Build the short-lived manager agenthicc's `_live_cli_manager` builds.
///
/// The servers come from the project config (or `--config`), read through the
/// same lenient reader as `mcp list`, so a stanza `list` shows is a stanza
/// these commands can act on. The network guard comes from the effective
/// `[security]` section; if the config cannot be loaded the guard falls back
/// to the defaults, which allow no remote host — the safe direction.
fn live_manager(config: Option<&PathBuf>) -> Result<Arc<McpSessionManager>, McpError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let scope = Scope {
        explicit: config.cloned(),
        ..Scope::default()
    };
    let path = config_path(scope, &cwd, home.as_deref())?;
    let entries = read_mcp_servers(&path)?;
    let effective = xaft_config::ConfigLoader::load(&xaft_config::CliOverrides {
        config_file: config.cloned(),
        ..Default::default()
    })
    .unwrap_or_default();
    Ok(McpSessionManager::new(
        entries,
        &cwd,
        Some(NetworkGuard::from_config(&effective)),
    ))
}

async fn connect(args: &McpLiveArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let manager = match live_manager(args.config.as_ref()) {
        Ok(manager) => manager,
        Err(e) => {
            let _ = writeln!(
                out,
                "error: MCP connect failed for {}: {e}",
                repr(&args.name)
            );
            return Ok(ExitCode::SUCCESS);
        }
    };
    let outcome = match manager.connect_server(&args.name).await {
        Ok(()) => manager.status(&args.name),
        Err(e) => Err(e),
    };
    manager.shutdown().await;
    match outcome {
        Ok(status) => {
            let _ = writeln!(out, "{}", pyjson::dumps(&status));
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "error: MCP connect failed for {}: {e}",
                repr(&args.name)
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn refresh(args: &McpLiveArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let manager = match live_manager(args.config.as_ref()) {
        Ok(manager) => manager,
        Err(e) => {
            let _ = writeln!(
                out,
                "error: MCP refresh failed for {}: {e}",
                repr(&args.name)
            );
            return Ok(ExitCode::SUCCESS);
        }
    };
    let snapshot = manager.refresh_server(&args.name).await;
    manager.shutdown().await;
    let payload = match snapshot {
        Some(snapshot) => snapshot.to_json(),
        None => serde_json::json!({ "status": "unavailable" }),
    };
    let _ = writeln!(out, "{}", pyjson::dumps(&payload));
    Ok(ExitCode::SUCCESS)
}

async fn doctor(args: &McpDoctorArgs, out: &mut String) -> Result<ExitCode, XaftError> {
    let name = args.name.as_deref().filter(|n| !n.is_empty());
    let result = match live_manager(args.config.as_ref()) {
        Ok(manager) => {
            let report = manager.doctor(name).await;
            manager.shutdown().await;
            report.map(serde_json::Value::Array)
        }
        Err(e) => Err(e.to_string()),
    };
    // agenthicc reports a doctor that could not run at all as one
    // `{"status": "invalid", "error": …}` document, never as a crash.
    let payload =
        result.unwrap_or_else(|error| serde_json::json!({ "status": "invalid", "error": error }));
    let rendered = if args.json {
        pyjson::dumps(&payload)
    } else {
        pyjson::dumps_pretty(&payload)
    };
    let _ = writeln!(out, "{rendered}");
    Ok(ExitCode::SUCCESS)
}

/// Report a config failure the way agenthicc does: on stdout, exit 0.
///
/// See the module docs for why the exit code is deliberately not an error.
fn soft(error: McpError, out: &mut String) -> Result<ExitCode, XaftError> {
    let _ = writeln!(out, "error: {error}");
    Ok(ExitCode::SUCCESS)
}

fn json_err(e: serde_json::Error) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}
