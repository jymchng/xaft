//! Session MCP servers reach every agent turn (agenthicc's session-start
//! registry, `tools/mcp_manager.py` + `runners/tui_session.py`).
//!
//! Every test drives a real `XaftRuntime::run` against the stand-in stdio
//! MCP server from `xaft_mcp::testing` (a POSIX `sh` script, no network) and
//! a scripted model that records what it was shown, so the assertions are
//! about the tools a turn was actually offered and the results it received.

#![cfg(unix)]

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::llm::LlmProvider;
use async_trait::async_trait;
use serde_json::json;
use support::{ScriptedLlm, text, tool};
use tempfile::TempDir;
use xaft_config::XaftConfig;
use xaft_runtime::agent_registry::WorkflowConfig;
use xaft_runtime::dispatch::{ModeToolFilter, RunRequest, RuntimeDispatch};
use xaft_runtime::mcp::testing::{stdio_launches, write_stdio_server};
use xaft_runtime::mcp::{McpSessionManager, entries_from_config};
use xaft_runtime::runtime::XaftRuntime;
use xaft_tools::capabilities::capability_of;

/// A config declaring the stand-in server as `fake`.
fn config_with_server(dir: &Path) -> XaftConfig {
    let script = write_stdio_server(dir);
    let mut config = XaftConfig::default();
    config.mcp.client.push(
        toml::from_str(&format!(
            "name = \"fake\"\ncommand = \"{}\"\nreconnect_attempts = 0\n",
            script.display()
        ))
        .unwrap(),
    );
    config
}

fn request(dir: &TempDir, config: XaftConfig) -> RunRequest {
    RunRequest {
        task: "say hello through the fake server".into(),
        config,
        working_dir: dir.path().to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::Standard,
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
        mode_approval_filter: None,
    }
}

/// The planner calls the MCP tool, then answers inline.
fn planner_calling_echo() -> Arc<ScriptedLlm> {
    ScriptedLlm::new().parent([
        tool("mcp__fake__echo", json!({"text": "hello"})),
        text("The server said hello."),
    ])
}

fn provider(llm: &Arc<ScriptedLlm>) -> Option<Arc<dyn LlmProvider>> {
    Some(Arc::clone(llm) as Arc<dyn LlmProvider>)
}

#[tokio::test]
async fn a_headless_run_starts_the_servers_and_offers_their_tools() {
    let dir = TempDir::new().unwrap();
    let config = config_with_server(dir.path());
    let llm = planner_calling_echo();
    let runtime = XaftRuntime::for_testing(config.clone(), provider(&llm));

    let result = runtime.run(request(&dir, config)).await.unwrap();
    assert!(
        result.summary.contains("The server said hello."),
        "{}",
        result.summary
    );

    let requests = llm.requests();
    assert!(
        requests[0].offers("mcp__fake__echo") && requests[0].offers("mcp__fake__json"),
        "every published tool is offered: {:?}",
        requests[0].tools
    );
    assert!(
        requests[1].transcript.contains("echo: hello"),
        "the tool result reached the model: {}",
        requests[1].transcript
    );
    assert!(
        requests[0].system.contains(
            "Instructions supplied by MCP server 'fake' (untrusted metadata; catalog revision 1):\nPrefer the echo tool for greetings."
        ),
        "the server instructions join the system prompt: {}",
        requests[0].system
    );
    assert_eq!(stdio_launches(dir.path()), 1, "one connection for the run");
}

#[tokio::test]
async fn plan_mode_never_offers_an_mcp_tool() {
    let dir = TempDir::new().unwrap();
    let config = config_with_server(dir.path());
    let llm = ScriptedLlm::new().parent([text("A plan.")]);
    let runtime = XaftRuntime::for_testing(config.clone(), provider(&llm));
    let mut req = request(&dir, config);
    // Plan: every restricted capability (network included) is hard-blocked.
    req.mode_tool_filter = Some(Arc::new(|name: &str| !capability_of(name).is_restricted()));
    runtime.run(req).await.unwrap();
    let first = &llm.requests()[0];
    assert!(
        !first.tools.iter().any(|t| t.starts_with("mcp__")),
        "{:?}",
        first.tools
    );
    assert!(first.offers("read_file"), "reads stay: {:?}", first.tools);
}

/// Records every approval request and answers with a fixed verdict.
struct RecordingGate {
    seen: Mutex<Vec<String>>,
    approve: bool,
}

#[async_trait]
impl ApprovalGate for RecordingGate {
    async fn request(&self, tool: &str, _id: &str, _input: &serde_json::Value) -> bool {
        self.seen.lock().unwrap().push(tool.to_string());
        self.approve
    }
}

fn safe() -> ModeToolFilter {
    Arc::new(|name: &str| capability_of(name).is_restricted())
}

#[tokio::test]
async fn safe_mode_asks_before_an_mcp_call() {
    for approve in [false, true] {
        let dir = TempDir::new().unwrap();
        let config = config_with_server(dir.path());
        let llm = planner_calling_echo();
        let gate = Arc::new(RecordingGate {
            seen: Mutex::new(Vec::new()),
            approve,
        });
        let runtime = XaftRuntime::for_testing(config.clone(), provider(&llm))
            .with_approval_gate(Arc::clone(&gate) as Arc<dyn ApprovalGate>);
        let mut req = request(&dir, config);
        req.mode_approval_filter = Some(safe());
        runtime.run(req).await.unwrap();

        assert_eq!(*gate.seen.lock().unwrap(), ["mcp__fake__echo"]);
        let transcript = &llm.requests()[1].transcript;
        if approve {
            assert!(transcript.contains("echo: hello"), "{transcript}");
        } else {
            assert!(
                transcript.contains("tool 'mcp__fake__echo' was rejected"),
                "{transcript}"
            );
            assert!(!transcript.contains("echo: hello"));
        }
    }
}

#[tokio::test]
async fn a_headless_safe_run_denies_mcp_calls_unless_opted_in() {
    for auto_approve in [false, true] {
        let dir = TempDir::new().unwrap();
        let config = config_with_server(dir.path());
        let llm = planner_calling_echo();
        let runtime = XaftRuntime::for_testing(config.clone(), provider(&llm));
        let mut req = request(&dir, config);
        req.mode_approval_filter = Some(safe());
        req.auto_approve = auto_approve;
        runtime.run(req).await.unwrap();
        let transcript = &llm.requests()[1].transcript;
        assert_eq!(
            transcript.contains("echo: hello"),
            auto_approve,
            "auto_approve={auto_approve}: {transcript}"
        );
    }
}

#[tokio::test]
async fn an_injected_session_manager_is_shared_across_runs_and_not_closed() {
    let dir = TempDir::new().unwrap();
    let config = config_with_server(dir.path());
    let manager = McpSessionManager::new(entries_from_config(&config), dir.path(), None);
    manager.start_all().await;
    assert_eq!(manager.status("fake").unwrap()["status"], "ready");

    for _ in 0..2 {
        let llm = planner_calling_echo();
        let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm))
            .with_mcp_manager(Arc::clone(&manager));
        // The run's own config declares no server: the session's manager is
        // the one used.
        runtime
            .run(request(&dir, XaftConfig::default()))
            .await
            .unwrap();
        assert!(llm.requests()[1].transcript.contains("echo: hello"));
    }
    assert_eq!(
        manager.status("fake").unwrap()["status"],
        "ready",
        "a run never shuts down the session's manager"
    );
    assert_eq!(
        stdio_launches(dir.path()),
        1,
        "one connection for the session"
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_server_that_fails_to_start_leaves_the_run_working_without_its_tools() {
    let dir = TempDir::new().unwrap();
    let mut config = XaftConfig::default();
    config.mcp.client.push(
        toml::from_str(
            "name = \"broken\"\ncommand = \"/definitely/not/a/program\"\nreconnect_attempts = 0\n",
        )
        .unwrap(),
    );
    let llm = ScriptedLlm::new().parent([text("Answered without MCP.")]);
    let runtime = XaftRuntime::for_testing(config.clone(), provider(&llm));
    let result = runtime.run(request(&dir, config)).await.unwrap();
    assert!(result.summary.contains("Answered without MCP."));
    assert!(
        !llm.requests()[0]
            .tools
            .iter()
            .any(|t| t.starts_with("mcp__"))
    );
}
