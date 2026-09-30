//! Handler for `xaft mcp` subcommands.
//!
//! Port of `agenthicc.cli.commands.mcp`. Seven subcommands, all of which only
//! read or edit the config file: none of them opens a socket, and none needs an
//! MCP subsystem to exist. `connect`, `refresh` and `doctor` are deliberately
//! absent — they need a live session manager.
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

use std::path::PathBuf;

use xaft_config::McpClientConfig;
use xaft_runtime::ExitCode;

use crate::args::{
    McpAddArgs, McpGetArgs, McpListArgs, McpNameArgs, McpRemoveArgs, McpScopeArgs, McpSubcommand,
};
use crate::error::XaftError;
use crate::mcp::{
    McpEntry, McpError, Scope, ServerSpec, add_mcp_server, config_path, read_mcp_servers,
    remove_mcp_server,
};

/// Execute `xaft mcp <subcommand>`.
pub async fn handle_mcp(subcommand: &McpSubcommand) -> Result<ExitCode, XaftError> {
    match subcommand {
        McpSubcommand::List(args) => list(args),
        McpSubcommand::Add(args) => add(args),
        McpSubcommand::Get(args) => get(args),
        McpSubcommand::Remove(args) => remove(args),
        McpSubcommand::Disconnect(args) => disconnect(args),
        McpSubcommand::Auth(args) => auth(args),
        McpSubcommand::Logout(args) => logout(args),
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

fn list(args: &McpListArgs) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e),
    };
    let entries = match read_mcp_servers(&path) {
        Ok(entries) => entries,
        Err(e) => return soft(e),
    };

    if args.json {
        let servers: Vec<serde_json::Value> = entries.iter().map(McpEntry::redacted).collect();
        let payload = serde_json::json!({ "path": path.display().to_string(), "servers": servers });
        println!("{}", serde_json::to_string(&payload).map_err(json_err)?);
        return Ok(ExitCode::SUCCESS);
    }

    if entries.is_empty() {
        println!("No MCP servers configured in {}", path.display());
        return Ok(ExitCode::SUCCESS);
    }

    for entry in &entries {
        match entry {
            McpEntry::Valid(c) => println!(
                "{}: transport={} enabled={}",
                c.name,
                c.normalized_transport(),
                c.enabled
            ),
            // agenthicc prints the same shape for an unparseable stanza, so a
            // `--json` consumer and a human see the same `status` field.
            McpEntry::Invalid { name } => println!("{name}: transport=unknown enabled=true"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn add(args: &McpAddArgs) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e),
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
        Err(e) => return soft(e),
    };

    let scope = if args.scope.global {
        "global"
    } else {
        "project"
    };
    match add_mcp_server(&path, &spec) {
        Ok(name) => {
            println!(
                "Added MCP server {} ({scope}) to {}",
                repr(&name),
                path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => soft(e),
    }
}

fn get(args: &McpGetArgs) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e),
    };
    let entries = match read_mcp_servers(&path) {
        Ok(entries) => entries,
        Err(e) => return soft(e),
    };

    let Some(entry) = entries.iter().find(|e| e.name() == args.name) else {
        println!("error: MCP server not found: {}", args.name);
        return Ok(ExitCode::SUCCESS);
    };

    let payload = entry.redacted();
    let rendered = if args.json {
        serde_json::to_string(&payload)
    } else {
        serde_json::to_string_pretty(&payload)
    }
    .map_err(json_err)?;
    println!("{rendered}");
    Ok(ExitCode::SUCCESS)
}

fn remove(args: &McpRemoveArgs) -> Result<ExitCode, XaftError> {
    let path = match resolve(&args.scope) {
        Ok(path) => path,
        Err(e) => return soft(e),
    };
    match remove_mcp_server(&path, &args.name) {
        Ok(()) => {
            println!(
                "Removed MCP server {} from {}",
                repr(&args.name),
                path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => soft(e),
    }
}

fn disconnect(args: &McpNameArgs) -> Result<ExitCode, XaftError> {
    // agenthicc's whole implementation is this one print.
    println!(
        "MCP server {} is session-scoped; run /mcp disconnect {} inside the active xaft session.",
        repr(&args.name),
        args.name
    );
    Ok(ExitCode::SUCCESS)
}

fn logout(args: &McpNameArgs) -> Result<ExitCode, XaftError> {
    // agenthicc's whole implementation is this one print. The reason it says
    // "no persisted credential" rather than "done" is that credentials here are
    // environment-backed by design, and the CLI refuses to mutate the
    // environment a credential is read from.
    println!(
        "No persisted MCP credential was removed for {}; environment-backed credentials remain unchanged.",
        repr(&args.name)
    );
    Ok(ExitCode::SUCCESS)
}

fn auth(args: &McpNameArgs) -> Result<ExitCode, XaftError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = match config_path(Scope::default(), &cwd, home.as_deref()) {
        Ok(path) => path,
        Err(e) => return soft(e),
    };

    let entries = match read_mcp_servers(&path) {
        Ok(entries) => entries,
        Err(e) => return soft(e),
    };

    let Some(McpEntry::Valid(config)) = entries.iter().find(|e| e.name() == args.name) else {
        // agenthicc reports the exception text from a failed `next()`. This
        // says the same thing in words a Rust user can act on.
        println!(
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
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({ "server": args.name, "status": status }))
            .map_err(json_err)?
    );
    Ok(ExitCode::SUCCESS)
}

/// Report a config failure the way agenthicc does: on stdout, exit 0.
///
/// See the module docs for why the exit code is deliberately not an error.
fn soft(error: McpError) -> Result<ExitCode, XaftError> {
    println!("error: {error}");
    Ok(ExitCode::SUCCESS)
}

fn json_err(e: serde_json::Error) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}
