//! The session manager against live stand-in servers (no network).
//!
//! * stdio: the POSIX `sh` stand-in from `xaft_mcp::testing`, launched for
//!   real, so relaunches and `notifications/tools/list_changed` are real.
//! * Streamable HTTP: the in-process stand-in bound to `127.0.0.1:0`.

#![cfg(unix)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agtrs_runtime::tool::{ErasedTool, ToolContext};
use serde_json::{Value, json};
use xaft_mcp::testing::{HttpStandIn, stdio_launches, stdio_stanza, write_stdio_server};
use xaft_mcp::{McpEntry, McpServerFailure, McpSessionManager, McpTimeouts, entries_from_config};

fn entries(stanzas: &str) -> Vec<McpEntry> {
    let config: xaft_config::XaftConfig = toml::from_str(stanzas).unwrap();
    entries_from_config(&config)
}

fn stdio_manager(dir: &Path, reconnect_attempts: u32) -> Arc<McpSessionManager> {
    let script = write_stdio_server(dir);
    McpSessionManager::with_timeouts(
        entries(&stdio_stanza("fake", &script, reconnect_attempts)),
        dir,
        None,
        McpTimeouts {
            refresh_debounce: Duration::from_millis(10),
            ..McpTimeouts::default()
        },
    )
}

fn tool(manager: &McpSessionManager, name: &str) -> Arc<ErasedTool> {
    manager
        .tools()
        .into_iter()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| {
            panic!(
                "{name} not published: {:?}",
                manager
                    .tools()
                    .iter()
                    .map(|t| t.name().to_string())
                    .collect::<Vec<_>>()
            )
        })
}

async fn call(tool: &Arc<ErasedTool>, input: Value) -> (bool, String) {
    let result = tool.call(input, &ToolContext::new("t")).await.unwrap();
    (result.is_error, result.content)
}

async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test]
async fn start_all_publishes_every_tool_under_its_provider_name() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = stdio_manager(dir.path(), 0);
    manager.start_all().await;
    let status = manager.status("fake").unwrap();
    assert_eq!(status["status"], "ready", "{status}");
    assert_eq!(status["tool_count"], 5);
    assert_eq!(status["catalog_revision"], 1);
    let names: Vec<String> = manager
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "mcp__fake__die",
            "mcp__fake__echo",
            "mcp__fake__fail",
            "mcp__fake__grow",
            "mcp__fake__json"
        ]
    );
    let echo = tool(&manager, "mcp__fake__echo");
    assert_eq!(echo.description(), "Echo text back");
    assert_eq!(echo.schema()["required"], json!(["text"]));
    assert!(
        !echo.requires_confirmation(),
        "the mode decides, not the tool"
    );
    let bindings = manager.tool_bindings();
    assert_eq!(bindings[1].canonical_name, "mcp:fake:echo");
    assert_eq!(
        manager.prompt_instructions(),
        "Instructions supplied by MCP server 'fake' (untrusted metadata; catalog revision 1):\n\
         Prefer the echo tool for greetings."
    );
    manager.shutdown().await;
    assert_eq!(manager.status("fake").unwrap()["status"], "stopped");
    assert!(manager.tools().is_empty());
}

#[tokio::test]
async fn calls_return_agenthiccs_extracted_content_and_errors() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = stdio_manager(dir.path(), 0);
    manager.start_all().await;
    assert_eq!(
        call(&tool(&manager, "mcp__fake__echo"), json!({"text": "hello"})).await,
        (false, "echo: hello".into())
    );
    assert_eq!(
        call(&tool(&manager, "mcp__fake__json"), json!({})).await,
        (false, "{\"answer\":42}".into()),
        "JSON text is decoded, then rendered compactly"
    );
    assert_eq!(
        call(&tool(&manager, "mcp__fake__fail"), json!({})).await,
        (
            true,
            "MCP server 'fake' returned error for 'fail': something broke".into()
        )
    );
    let (is_error, content) = call(&tool(&manager, "mcp__fake__echo"), json!("text")).await;
    assert!(
        is_error && content.contains("takes a JSON object"),
        "{content}"
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_list_changed_notification_republishes_the_catalog() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = stdio_manager(dir.path(), 0);
    manager.start_all().await;
    let echo_before = tool(&manager, "mcp__fake__echo");
    assert_eq!(
        call(&tool(&manager, "mcp__fake__grow"), json!({})).await,
        (false, "grown".into())
    );
    assert!(
        eventually(|| manager
            .tools()
            .iter()
            .any(|t| t.name() == "mcp__fake__extra"))
        .await,
        "the refresh published the new tool"
    );
    let status = manager.status("fake").unwrap();
    assert_eq!(status["tool_count"], 6);
    assert_eq!(status["catalog_revision"], 2);
    // A binding from the replaced catalog refuses rather than guessing.
    assert_eq!(
        call(&echo_before, json!({"text": "x"})).await,
        (
            true,
            "MCP tool 'mcp:fake:echo' belongs to a stale catalog revision".into()
        )
    );
    assert_eq!(
        call(&tool(&manager, "mcp__fake__echo"), json!({"text": "again"})).await,
        (false, "echo: again".into())
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_dropped_server_is_relaunched_within_its_reconnect_budget() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = stdio_manager(dir.path(), 1);
    let failures: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&failures);
    manager.set_failure_listener(Arc::new(move |f: &McpServerFailure| {
        sink.lock().unwrap().push(f.transcript_line());
    }));
    manager.start_all().await;
    assert_eq!(stdio_launches(dir.path()), 1);

    // The server dies mid-call: that call fails…
    let (is_error, content) = call(&tool(&manager, "mcp__fake__die"), json!({})).await;
    assert!(is_error, "{content}");
    assert!(
        content.starts_with("MCP call fake/die failed:"),
        "{content}"
    );
    // …and the next one relaunches it (budget: 1).
    assert_eq!(
        call(&tool(&manager, "mcp__fake__echo"), json!({"text": "back"})).await,
        (false, "echo: back".into())
    );
    assert_eq!(stdio_launches(dir.path()), 2);
    assert_eq!(manager.status("fake").unwrap()["status"], "ready");

    // A second death spends the budget: the server is failed, not retried.
    let _ = call(&tool(&manager, "mcp__fake__die"), json!({})).await;
    assert_eq!(
        call(&tool(&manager, "mcp__fake__echo"), json!({"text": "x"})).await,
        (true, "Server 'fake' is not connected".into())
    );
    let status = manager.status("fake").unwrap();
    assert_eq!(status["status"], "failed");
    assert_eq!(status["tool_count"], 0);
    assert_eq!(
        status["last_error"],
        "MCP server 'fake' disconnected and its 1 reconnect attempt(s) are spent"
    );
    assert!(manager.tools().is_empty(), "its tools left the catalog");
    assert_eq!(stdio_launches(dir.path()), 2);
    assert_eq!(
        *failures.lock().unwrap(),
        ["MCP server 'fake' is unavailable; continuing without its tools."]
    );

    // `/mcp connect` (or reload) brings it back with a fresh budget.
    manager.connect_server("fake").await.unwrap();
    assert_eq!(manager.status("fake").unwrap()["status"], "ready");
    assert_eq!(
        call(&tool(&manager, "mcp__fake__echo"), json!({"text": "again"})).await,
        (false, "echo: again".into())
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn disconnect_reload_and_refresh_manage_one_server() {
    let dir = tempfile::TempDir::new().unwrap();
    let script = write_stdio_server(dir.path());
    let manager = McpSessionManager::new(
        entries(&format!(
            "{}\nauto_connect = false\n",
            stdio_stanza("manual", &script, 0)
        )),
        dir.path(),
        None,
    );
    manager.start_all().await;
    assert_eq!(
        manager.status("manual").unwrap()["status"],
        "configured",
        "auto_connect = false is not started at session start"
    );
    let first = manager
        .refresh_server("manual")
        .await
        .expect("refresh starts it");
    assert_eq!(first.revision, 1);
    assert_eq!(manager.status("manual").unwrap()["status"], "ready");
    let same = manager.refresh_server("manual").await.unwrap();
    assert_eq!(same.revision, 1, "an unchanged catalog keeps its revision");
    assert_eq!(same.catalog_hash, first.catalog_hash);

    manager.disconnect_server("manual").await.unwrap();
    let status = manager.status("manual").unwrap();
    assert_eq!(status["status"], "stopped");
    assert_eq!(status["tool_count"], 0);
    assert!(manager.tools().is_empty());
    assert!(manager.disconnect_server("ghost").await.is_err());

    manager.reload_all().await;
    assert_eq!(
        manager.status("manual").unwrap()["status"],
        "ready",
        "reload includes auto_connect = false servers"
    );
    assert_eq!(stdio_launches(dir.path()), 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn the_http_transport_carries_session_calls() {
    let server = HttpStandIn::start().await;
    let dir = tempfile::TempDir::new().unwrap();
    let manager = McpSessionManager::new(entries(&server.stanza("web")), dir.path(), None);
    manager.start_all().await;
    assert_eq!(manager.status("web").unwrap()["status"], "ready");
    assert_eq!(
        manager.status("web").unwrap()["transport"],
        "streamable_http"
    );
    assert_eq!(
        call(
            &tool(&manager, "mcp__web__echo"),
            json!({"text": "over http"})
        )
        .await,
        (false, "echo: over http".into())
    );
    assert_eq!(
        server.calls(),
        [("echo".to_string(), json!({"text": "over http"}))]
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn the_network_guard_applies_to_session_servers() {
    let server = HttpStandIn::start().await;
    let dir = tempfile::TempDir::new().unwrap();
    let manager = McpSessionManager::new(
        entries(&server.stanza("web")),
        dir.path(),
        Some(xaft_mcp::NetworkGuard::from_config(
            &xaft_config::XaftConfig::default(),
        )),
    );
    manager.start_all().await;
    let status = manager.status("web").unwrap();
    assert_eq!(status["status"], "failed");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .ends_with("Outbound request to '127.0.0.1' is not on the network allow-list."),
        "{status}"
    );
    assert!(server.calls().is_empty(), "nothing was dialled");
}

#[tokio::test]
async fn provider_names_never_collide_across_servers() {
    let dir = tempfile::TempDir::new().unwrap();
    let script = write_stdio_server(dir.path());
    // `a.b` and `a_b` sanitise to the same provider prefix.
    let manager = McpSessionManager::new(
        entries(&format!(
            "{}\n{}",
            stdio_stanza("a.b", &script, 0),
            stdio_stanza("a_b", &script, 0)
        )),
        dir.path(),
        None,
    );
    manager.start_all().await;
    let names: Vec<String> = manager
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(names.len(), 10, "{names:?}");
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), 10, "every provider name is unique: {names:?}");
    assert!(
        names
            .iter()
            .all(|n| n.len() <= 64 && n.starts_with("mcp__a_b__"))
    );
    manager.shutdown().await;
}
