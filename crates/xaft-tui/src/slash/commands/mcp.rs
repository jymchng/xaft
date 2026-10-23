//! `/mcp` — report and manage the session's live MCP servers.
//!
//! Port of agenthicc's `_cmd_mcp` (`commands/builtins.py`), over the one
//! `McpSessionManager` the session shares with every agent turn
//! (`SessionServices::mcp_manager`):
//!
//! | input | effect |
//! | --- | --- |
//! | `/mcp`, `/mcp status`, `/mcp list` | every server's status record, `json.dumps(…, sort_keys=True, indent=2)` |
//! | `/mcp connect NAME` | start one server (including an `auto_connect = false` one) |
//! | `/mcp disconnect NAME` | close it and drop its tools |
//! | `/mcp refresh NAME` | re-fetch its catalog |
//! | `/mcp reload` | reconnect every enabled server and republish every catalog |
//! | `/mcp doctor [NAME]` | validate and live-start, then print the per-server report |
//!
//! Actions run in the background, as agenthicc's `manager.start_action`
//! does: the command answers `/mcp <action> scheduled[ for NAME].` at once,
//! and `/mcp <action> complete[ for NAME].` (or `/mcp <action> failed: …`,
//! or the doctor report) arrives through [`Notices`] when the work ends. The
//! event loop never waits on a server.
//!
//! One divergence: agenthicc's failure line names the Python exception class
//! (`KeyError: …`); xaft's is the message alone, the same choice recorded for
//! `xaft mcp connect`.

use std::sync::Arc;

use xaft_runtime::mcp::{McpSessionManager, pyjson};

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult, Notices};

/// agenthicc's usage line.
pub const USAGE: &str =
    "Usage: /mcp [status|reload|connect NAME|disconnect NAME|refresh NAME|doctor [NAME]]";

/// No manager attached: no server is configured.
pub const UNAVAILABLE: &str = "/mcp: no MCP servers configured or not available.";

/// The `/mcp` handler.
pub struct McpHandler;

impl SlashHandler for McpHandler {
    fn description(&self) -> &'static str {
        "Show MCP server status"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[status|reload|connect NAME|disconnect NAME|refresh NAME|doctor [NAME]]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let Some(manager) = ctx.services.mcp_manager.clone() else {
            return CommandResult::Lines(vec![UNAVAILABLE.into()]);
        };
        run(&manager, &ctx.args, ctx.services.notices.clone())
    }
}

/// Dispatch `/mcp <args>` against `manager`.
///
/// Status is answered now; every other action is scheduled on the current
/// Tokio runtime and reports through `notices`.
pub fn run(manager: &Arc<McpSessionManager>, args: &str, notices: Notices) -> CommandResult {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let action = parts
        .first()
        .map(|a| a.to_ascii_lowercase())
        .unwrap_or_else(|| "status".into());
    let name = parts.get(1).map(|n| n.to_string());

    if action == "status" || action == "list" {
        let status = manager.status_all();
        if status.as_object().is_none_or(|m| m.is_empty()) {
            return CommandResult::Lines(vec!["/mcp: no MCP servers configured.".into()]);
        }
        return CommandResult::Lines(
            pyjson::dumps_pretty(&status)
                .lines()
                .map(String::from)
                .collect(),
        );
    }
    if action == "reload" && name.is_some() {
        return CommandResult::Lines(vec![USAGE.into()]);
    }
    let known = ["connect", "disconnect", "refresh", "doctor", "reload"];
    if !known.contains(&action.as_str())
        || (!matches!(action.as_str(), "doctor" | "reload") && name.is_none())
    {
        return CommandResult::Lines(vec![USAGE.into()]);
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return CommandResult::Error("No async runtime available.".into());
    };
    let suffix = name
        .as_ref()
        .map(|n| format!(" for {n}"))
        .unwrap_or_default();
    let manager = Arc::clone(manager);
    let scheduled = format!("/mcp {action} scheduled{suffix}.");
    runtime.spawn(async move {
        for line in perform(&manager, &action, name.as_deref()).await {
            notices.push(line);
        }
    });
    CommandResult::Lines(vec![scheduled])
}

/// Run one action to completion and return the lines it reports.
pub async fn perform(manager: &McpSessionManager, action: &str, name: Option<&str>) -> Vec<String> {
    let suffix = name.map(|n| format!(" for {n}")).unwrap_or_default();
    let outcome: Result<(), String> = match (action, name) {
        ("connect", Some(name)) => manager.connect_server(name).await,
        ("disconnect", Some(name)) => manager.disconnect_server(name).await,
        ("refresh", Some(name)) => {
            if manager.contains(name) {
                manager.refresh_server(name).await;
                Ok(())
            } else {
                Err(format!("No MCP server registered with name '{name}'"))
            }
        }
        ("reload", _) => {
            manager.reload_all().await;
            Ok(())
        }
        ("doctor", name) => {
            return match manager.doctor(name).await {
                Ok(report) => pyjson::dumps_pretty(&serde_json::Value::Array(report))
                    .lines()
                    .map(String::from)
                    .collect(),
                Err(error) => vec![format!("/mcp doctor failed: {error}")],
            };
        }
        _ => return vec![USAGE.into()],
    };
    match outcome {
        Ok(()) => vec![format!("/mcp {action} complete{suffix}.")],
        Err(error) => vec![format!("/mcp {action} failed: {error}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use xaft_runtime::mcp::McpEntry;

    fn lines(result: CommandResult) -> Vec<String> {
        match result {
            CommandResult::Lines(lines) => lines,
            other => panic!("expected lines, got {other:?}"),
        }
    }

    fn manager(stanzas: &str) -> Arc<McpSessionManager> {
        let config: xaft_config::XaftConfig = toml::from_str(stanzas).unwrap();
        McpSessionManager::new(
            xaft_runtime::mcp::entries_from_config(&config),
            Path::new("/"),
            None,
        )
    }

    #[tokio::test]
    async fn status_prints_every_server_as_sorted_indented_json() {
        let m = manager(
            "[[mcp.client]]\nname = \"zeta\"\ncommand = \"srv\"\n\n[[mcp.client]]\nname = \"alpha\"\ncommand = \"srv\"\nenabled = false\n",
        );
        for args in ["", "status", "LIST"] {
            let out = lines(run(&m, args, Notices::default()));
            assert_eq!(out[0], "{", "{args}: {out:?}");
            assert_eq!(out[1], "  \"alpha\": {", "sorted by name, indent=2");
            assert!(
                out.iter().any(|l| l == "    \"status\": \"disabled\","),
                "{out:?}"
            );
            assert!(out.iter().any(|l| l == "    \"status\": \"configured\","));
        }
    }

    #[tokio::test]
    async fn an_empty_registry_says_so() {
        let m = McpSessionManager::new(Vec::<McpEntry>::new(), Path::new("/"), None);
        assert_eq!(
            lines(run(&m, "status", Notices::default())),
            ["/mcp: no MCP servers configured."]
        );
    }

    #[tokio::test]
    async fn malformed_forms_print_agenthiccs_usage() {
        let m = manager("[[mcp.client]]\nname = \"a\"\ncommand = \"srv\"\n");
        for args in ["connect", "refresh", "disconnect", "bogus", "reload a"] {
            assert_eq!(lines(run(&m, args, Notices::default())), [USAGE], "{args}");
        }
    }

    #[tokio::test]
    async fn an_action_is_scheduled_and_reports_through_notices() {
        let m = manager(
            "[[mcp.client]]\nname = \"a\"\ncommand = \"/definitely/not/a/program\"\nreconnect_attempts = 0\n",
        );
        let notices = Notices::default();
        assert_eq!(
            lines(run(&m, "connect a", notices.clone())),
            ["/mcp connect scheduled for a."]
        );
        let mut reported = Vec::new();
        for _ in 0..200 {
            reported.extend(notices.drain());
            if !reported.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(reported, ["/mcp connect complete for a."]);
        assert_eq!(m.status("a").unwrap()["status"], "failed");
    }

    #[tokio::test]
    async fn unknown_names_fail_with_the_managers_message() {
        let m = manager("[[mcp.client]]\nname = \"a\"\ncommand = \"srv\"\n");
        assert_eq!(
            perform(&m, "connect", Some("ghost")).await,
            ["/mcp connect failed: No MCP server registered with name 'ghost'"]
        );
        assert_eq!(
            perform(&m, "refresh", Some("ghost")).await,
            ["/mcp refresh failed: No MCP server registered with name 'ghost'"]
        );
        assert_eq!(
            perform(&m, "doctor", Some("ghost")).await,
            ["/mcp doctor failed: No MCP server registered with name 'ghost'"]
        );
    }
}
