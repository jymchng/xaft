//! `/mcp` reports and manages the session's live MCP servers (agenthicc's
//! `_cmd_mcp`), dispatched through `AppState` exactly as the input loop does,
//! over a manager running the stand-in stdio server (no network).

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use xaft_runtime::mcp::testing::{stdio_stanza, write_stdio_server};
use xaft_runtime::mcp::{McpSessionManager, entries_from_config};
use xaft_tui::slash::CommandResult;
use xaft_tui::slash::parser::SlashCommandParser;
use xaft_tui::state::AppState;

fn run(state: &mut AppState, input: &str) -> Vec<String> {
    let command = match SlashCommandParser::parse(input) {
        Some(Ok(c)) => c,
        other => panic!("{input:?} did not parse: {other:?}"),
    };
    let after = input.trim_start().trim_start_matches('/');
    let args = match after.find(char::is_whitespace) {
        Some(i) => after[i + 1..].trim().to_string(),
        None => String::new(),
    };
    let handler = state.slash_registry.get(&command);
    match handler.execute(state.build_command_context(args, command)) {
        CommandResult::Lines(lines) => lines,
        other => panic!("{input}: expected lines, got {other:?}"),
    }
}

/// Wait for the background action's report.
async fn notices(state: &AppState) -> Vec<String> {
    let mut out = Vec::new();
    for _ in 0..400 {
        out.extend(state.services.notices.drain());
        if !out.is_empty() {
            // Give a multi-line report (doctor) a moment to land whole.
            tokio::time::sleep(Duration::from_millis(50)).await;
            out.extend(state.services.notices.drain());
            return out;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no notice arrived");
}

fn manager(dir: &std::path::Path) -> Arc<McpSessionManager> {
    let script = write_stdio_server(dir);
    let config: xaft_config::XaftConfig =
        toml::from_str(&stdio_stanza("fake", &script, 0)).unwrap();
    McpSessionManager::new(entries_from_config(&config), dir, None)
}

#[tokio::test]
async fn without_servers_mcp_says_none_are_available() {
    let mut state = AppState::new("");
    assert_eq!(
        run(&mut state, "/mcp"),
        ["/mcp: no MCP servers configured or not available."]
    );
}

#[tokio::test]
async fn mcp_reports_and_manages_the_live_registry() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = manager(dir.path());
    manager.start_all().await;
    let mut state = AppState::new("");
    state.attach_mcp_manager(Some(Arc::clone(&manager)));

    let status = run(&mut state, "/mcp").join("\n");
    let parsed: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(parsed["fake"]["status"], "ready", "{status}");
    assert_eq!(parsed["fake"]["tool_count"], 5);
    assert!(
        status.starts_with("{\n  \"fake\": {\n    \"auth_state\""),
        "{status}"
    );

    assert_eq!(
        run(&mut state, "/mcp disconnect fake"),
        ["/mcp disconnect scheduled for fake."]
    );
    assert_eq!(
        notices(&state).await,
        ["/mcp disconnect complete for fake."]
    );
    assert_eq!(manager.status("fake").unwrap()["status"], "stopped");
    assert!(manager.tools().is_empty());

    assert_eq!(run(&mut state, "/mcp reload"), ["/mcp reload scheduled."]);
    assert_eq!(notices(&state).await, ["/mcp reload complete."]);
    assert_eq!(manager.status("fake").unwrap()["status"], "ready");

    assert_eq!(
        run(&mut state, "/mcp refresh fake"),
        ["/mcp refresh scheduled for fake."]
    );
    assert_eq!(notices(&state).await, ["/mcp refresh complete for fake."]);

    assert_eq!(
        run(&mut state, "/mcp doctor fake"),
        ["/mcp doctor scheduled for fake."]
    );
    let report = notices(&state).await.join("\n");
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report[0]["server"], "fake");
    assert_eq!(report[0]["status"]["status"], "ready");

    assert_eq!(
        run(&mut state, "/mcp connect"),
        ["Usage: /mcp [status|reload|connect NAME|disconnect NAME|refresh NAME|doctor [NAME]]"]
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_failed_start_is_reported_with_agenthiccs_line() {
    let dir = tempfile::TempDir::new().unwrap();
    let config: xaft_config::XaftConfig = toml::from_str(
        "[[mcp.client]]\nname = \"broken\"\ncommand = \"/definitely/not/a/program\"\nreconnect_attempts = 0\n",
    )
    .unwrap();
    let manager = McpSessionManager::new(entries_from_config(&config), dir.path(), None);
    let mut state = AppState::new("");
    state.attach_mcp_manager(Some(Arc::clone(&manager)));
    // What `app.rs` installs.
    let notices = state.services.notices.clone();
    manager.set_failure_listener(Arc::new(
        move |failure: &xaft_runtime::mcp::McpServerFailure| {
            notices.push(failure.transcript_line());
        },
    ));
    manager.start_all().await;
    assert_eq!(
        state.services.notices.drain(),
        ["MCP server 'broken' is unavailable; continuing without its tools."]
    );
    let status = run(&mut state, "/mcp status").join("\n");
    assert!(status.contains("\"status\": \"failed\""), "{status}");
}

#[tokio::test]
async fn tools_lists_the_session_mcp_tools_with_their_source() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = manager(dir.path());
    manager.start_all().await;
    let mut state = AppState::new("");
    state.working_dir = dir.path().to_path_buf();
    state.attach_mcp_manager(Some(Arc::clone(&manager)));
    let out = run(&mut state, "/tools");
    assert!(
        out.iter()
            .any(|l| l == "  mcp__fake__echo  [mcp]  Echo text back"),
        "{out:?}"
    );
    assert!(out.iter().any(|l| l.contains("read_file  [builtin]")));
    manager.shutdown().await;
}
