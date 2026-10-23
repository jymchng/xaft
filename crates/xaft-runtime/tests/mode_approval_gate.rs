//! Safe mode's approval requirement, end to end through `XaftRuntime::run`
//! (parity §3 row 1).
//!
//! agenthicc's Safe makes every side-effecting tool wait for an explicit
//! approval, enforced by the runtime rather than requested of the model. These
//! drive a real run — a scripted coder calls `write_file`, which does not
//! require confirmation on its own — with and without
//! `RunRequest::mode_approval_filter`, and observe what the approval gate saw
//! and whether the file was written.

use std::sync::{Arc, Mutex};

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use async_trait::async_trait;
use tempfile::TempDir;
use xaft_config::XaftConfig;
use xaft_runtime::agent_registry::WorkflowConfig;
use xaft_runtime::dispatch::{ModeToolFilter, RunRequest, RuntimeDispatch};
use xaft_runtime::runtime::XaftRuntime;

/// Records every approval request and answers with a fixed verdict.
struct RecordingGate {
    seen: Mutex<Vec<String>>,
    approve: bool,
}

#[async_trait]
impl ApprovalGate for RecordingGate {
    async fn request(&self, tool_name: &str, _id: &str, _input: &serde_json::Value) -> bool {
        self.seen.lock().unwrap().push(tool_name.to_string());
        self.approve
    }
}

async fn scripted_coder_writing(path: &str) -> Arc<dyn LlmProvider> {
    let transport = Arc::new(MockTransport::new());
    transport
        .queue_tool_call(
            "write_file",
            serde_json::json!({"path": path, "content": "hello"}),
        )
        .await;
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "qa", "reason": "done"}),
        )
        .await;
    transport.queue_text("Handing off to QA.").await;
    for _ in 0..4 {
        transport.queue_text("APPROVED").await;
    }
    Arc::new(MockLlmProvider::new(transport))
}

fn request(dir: &TempDir, approval: Option<ModeToolFilter>) -> RunRequest {
    RunRequest {
        task: "write a file".into(),
        config: XaftConfig::default(),
        working_dir: dir.path().to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::Dynamic {
            initial_agent: "coder".into(),
            max_handoffs: 4,
            agent_subset: None,
        },
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
        mode_approval_filter: approval,
    }
}

#[tokio::test]
async fn a_mode_approval_filter_routes_a_write_through_the_gate() {
    let dir = TempDir::new().unwrap();
    let gate = Arc::new(RecordingGate {
        seen: Mutex::new(Vec::new()),
        approve: false,
    });
    let runtime = XaftRuntime::for_testing(
        XaftConfig::default(),
        Some(scripted_coder_writing("out.txt").await),
    )
    .with_approval_gate(Arc::clone(&gate) as Arc<dyn ApprovalGate>);

    // Safe's shape: everything but reads needs approval.
    let safe: ModeToolFilter = Arc::new(|name: &str| name != "read_file");
    let _ = runtime.run(request(&dir, Some(safe))).await;

    assert!(
        gate.seen.lock().unwrap().iter().any(|t| t == "write_file"),
        "the write must have been put to the gate: {:?}",
        gate.seen.lock().unwrap()
    );
    assert!(
        !dir.path().join("out.txt").exists(),
        "a rejected write must not have run"
    );
}

#[tokio::test]
async fn without_the_filter_the_same_write_runs_unasked() {
    let dir = TempDir::new().unwrap();
    let gate = Arc::new(RecordingGate {
        seen: Mutex::new(Vec::new()),
        approve: false,
    });
    let runtime = XaftRuntime::for_testing(
        XaftConfig::default(),
        Some(scripted_coder_writing("out.txt").await),
    )
    .with_approval_gate(Arc::clone(&gate) as Arc<dyn ApprovalGate>);

    let _ = runtime.run(request(&dir, None)).await;

    assert!(
        !gate.seen.lock().unwrap().iter().any(|t| t == "write_file"),
        "write_file does not ask on its own, so the gate never saw it"
    );
    assert!(
        dir.path().join("out.txt").exists(),
        "the control run wrote the file — so the first test's absence is the gate's doing"
    );
}
