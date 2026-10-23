//! A phased workflow's phases get the session's MCP tools and
//! `spawn_subagents` — agenthicc's agent-turn runner gives every workflow
//! phase both — filtered by the phase's own capabilities and mode.

#![cfg(unix)]

mod support;

use std::sync::Arc;

use agtrs_runtime::llm::LlmProvider;
use serde_json::json;
use support::{ScriptedLlm, last_tool_result, text, tool};
use tempfile::TempDir;
use xaft_config::XaftConfig;
use xaft_config::types::WorkflowDefinition;
use xaft_runtime::agent_registry::WorkflowConfig;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::mcp::testing::write_stdio_server;
use xaft_runtime::runtime::XaftRuntime;

fn phased(definition: &str) -> WorkflowConfig {
    WorkflowConfig::Phased {
        name: "session_tools".into(),
        definition: Box::new(toml::from_str::<WorkflowDefinition>(definition).unwrap()),
        resume_run_id: None,
    }
}

fn run_request(dir: &TempDir, config: XaftConfig, workflow: WorkflowConfig) -> RunRequest {
    RunRequest {
        task: "use the session tools".into(),
        config,
        working_dir: dir.path().to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow,
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
        mode_approval_filter: None,
    }
}

fn config_with_server(dir: &TempDir) -> XaftConfig {
    let script = write_stdio_server(dir.path());
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

#[tokio::test]
async fn a_generic_phase_gets_mcp_tools_and_spawn_subagents() {
    let dir = TempDir::new().unwrap();
    let config = config_with_server(&dir);
    let llm = ScriptedLlm::new()
        .parent([
            tool("mcp__fake__echo", json!({"text": "from a phase"})),
            tool(
                "spawn_subagents",
                json!({"tasks": [{"type": "explorer", "task": "Look around", "context": "ROUTE-E"}]}),
            ),
            text("Phase done."),
        ])
        .route("ROUTE-E", [text("Nothing unusual.")]);
    let runtime = XaftRuntime::for_testing(
        config.clone(),
        Some(Arc::clone(&llm) as Arc<dyn LlmProvider>),
    );
    let workflow = phased("mode = \"phased\"\n[[phases]]\nname = \"work\"\nmax_turns = 6\n");
    runtime
        .run(run_request(&dir, config, workflow))
        .await
        .unwrap();

    let phase = llm.requests_for("");
    assert!(phase[0].offers("mcp__fake__echo"), "{:?}", phase[0].tools);
    assert!(phase[0].offers("spawn_subagents"));
    assert!(phase[1].transcript.contains("echo: from a phase"));
    let spawned = {
        let messages: Vec<agtrs_runtime::transport::Message> =
            serde_json::from_str(&phase[2].transcript).unwrap();
        last_tool_result(&messages)
    };
    assert_eq!(spawned["ok"], true, "{spawned}");
    assert!(
        spawned["results"]
            .as_str()
            .unwrap()
            .ends_with("Nothing unusual.")
    );
}

#[tokio::test]
async fn a_read_only_phase_sees_neither_the_mcp_tools_nor_spawn_subagents() {
    let dir = TempDir::new().unwrap();
    let config = config_with_server(&dir);
    let llm = ScriptedLlm::new().parent([text("Looked.")]);
    let runtime = XaftRuntime::for_testing(
        config.clone(),
        Some(Arc::clone(&llm) as Arc<dyn LlmProvider>),
    );
    let workflow = phased(
        "mode = \"phased\"\n[[phases]]\nname = \"look\"\nmode_override = \"plan\"\nmax_turns = 3\n",
    );
    runtime
        .run(run_request(&dir, config, workflow))
        .await
        .unwrap();
    let first = &llm.requests()[0];
    assert!(
        !first.tools.iter().any(|t| t.starts_with("mcp__")),
        "{:?}",
        first.tools
    );
    assert!(!first.offers("spawn_subagents"));
}

#[tokio::test]
async fn a_phase_allowing_network_gets_mcp_tools_and_a_read_role_does_not() {
    let dir = TempDir::new().unwrap();
    let config = config_with_server(&dir);
    let llm = ScriptedLlm::new().parent([text("first"), text("second")]);
    let runtime = XaftRuntime::for_testing(
        config.clone(),
        Some(Arc::clone(&llm) as Arc<dyn LlmProvider>),
    );
    let workflow = phased(
        r#"mode = "phased"
[[phases]]
name = "fetch"
allowed_capabilities = ["read", "network"]
max_turns = 3
next = "review"

[[phases]]
name = "review"
agent_type = "reviewer"
max_turns = 3
"#,
    );
    runtime
        .run(run_request(&dir, config, workflow))
        .await
        .unwrap();
    let requests = llm.requests();
    assert!(
        requests[0].offers("mcp__fake__echo"),
        "{:?}",
        requests[0].tools
    );
    assert!(
        !requests[1].tools.iter().any(|t| t.starts_with("mcp__")),
        "a reviewer's read-only capabilities exclude network: {:?}",
        requests[1].tools
    );
}
