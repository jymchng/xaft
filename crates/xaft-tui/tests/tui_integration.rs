//! Integration tests for the xaft TUI.
//!
//! Tests cover the full event pipeline:
//!   signal emission → EventBridge → TuiEvent → AppState mutations
//!
//! Uses mock signal buses and direct event injection — no real terminal required.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agtrs_runtime::signals::{
    AgentCancelled, FileEditsCommitted, ModelCallComplete, SignalBus, ToolCallComplete,
    ToolCallStarted, ToolPendingApproval,
};

use xaft_tui::approval::RiskLevel;
use xaft_tui::bridge::{EventBridge, TuiEvent};
use xaft_tui::state::{AppState, WorkflowPhase};
use xaft_tui::transcript::{LineKind, RenderMutation};

use tokio::sync::mpsc;

// ── Test helpers ──────────────────────────────────────────────────────────────

fn make_bridge() -> (
    Arc<SignalBus>,
    EventBridge,
    mpsc::UnboundedReceiver<TuiEvent>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let bus = Arc::new(SignalBus::new());
    let bridge = EventBridge::new(tx);
    (bus, bridge, rx)
}

fn make_state() -> AppState {
    AppState::new("test task")
}

async fn drain(rx: &mut mpsc::UnboundedReceiver<TuiEvent>) -> Vec<TuiEvent> {
    tokio::time::sleep(Duration::from_millis(30)).await;
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    events
}

fn commit_texts(state: &AppState) -> Vec<&str> {
    state
        .mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.as_str()),
            _ => None,
        })
        .collect()
}

fn has_stream_token(state: &AppState) -> bool {
    state
        .mutations
        .iter()
        .any(|m| matches!(m, RenderMutation::StreamToken { .. }))
}

fn has_flush_stream(state: &AppState) -> bool {
    state
        .mutations
        .iter()
        .any(|m| matches!(m, RenderMutation::FlushStream))
}

// ── 1. Bridge forwarding ─────────────────────────────────────────────────────

#[tokio::test]
async fn bridge_forwards_tool_started_from_signal() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(ToolCallStarted {
        tool_name: "read_file".into(),
        tool_use_id: "tu-1".into(),
        agent_id: None,
        input: serde_json::json!({"path": "src/main.rs"}),
        cache_hit: false,
    })
    .await;

    let events = drain(&mut rx).await;
    let tool_evt = events
        .iter()
        .find(|e| matches!(e, TuiEvent::ToolStarted { .. }));
    assert!(tool_evt.is_some(), "expected ToolStarted event");
    if let TuiEvent::ToolStarted {
        tool_name,
        tool_use_id,
        ..
    } = tool_evt.unwrap()
    {
        assert_eq!(tool_name, "read_file");
        assert_eq!(tool_use_id, "tu-1");
    }
}

#[tokio::test]
async fn bridge_forwards_tool_completed_from_signal() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(ToolCallComplete {
        tool_name: "write_file".into(),
        tool_use_id: "tu-2".into(),
        agent_id: None,
        duration_ms: 42.0,
        success: true,
        error: None,
    })
    .await;

    let events = drain(&mut rx).await;
    let completed = events
        .iter()
        .find(|e| matches!(e, TuiEvent::ToolCompleted { .. }));
    assert!(completed.is_some());
    if let TuiEvent::ToolCompleted { success, .. } = completed.unwrap() {
        assert!(success);
    }
}

#[tokio::test]
async fn bridge_forwards_llm_call_complete() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(ModelCallComplete {
        agent_name: "coder".into(),
        agent_id: None,
        model: "claude-3".into(),
        usage: agtrs_runtime::transport::TokenUsage {
            input_tokens: 100,
            output_tokens: 200,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        },
        cost_usd: 0.01,
        duration_ms: 500.0,
        total_tokens: 300,
        turns: 1,
        stop_reason: agtrs_runtime::transport::StopReason::EndTurn,
    })
    .await;

    let events = drain(&mut rx).await;
    let evt = events
        .iter()
        .find(|e| matches!(e, TuiEvent::LlmCallComplete { .. }));
    assert!(evt.is_some(), "expected LlmCallComplete");
    if let TuiEvent::LlmCallComplete {
        agent_name,
        cost_usd,
        ..
    } = evt.unwrap()
    {
        assert_eq!(agent_name, "coder");
        assert!((cost_usd - 0.01).abs() < 1e-9);
    }
}

#[tokio::test]
async fn bridge_forwards_pending_approval() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(ToolPendingApproval {
        agent_id: "agent-1".into(),
        agent_run_id: "run-1".into(),
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-3".into(),
        input: serde_json::json!({"command": "rm -rf /"}),
    })
    .await;

    let events = drain(&mut rx).await;
    let evt = events
        .iter()
        .find(|e| matches!(e, TuiEvent::ToolPendingApproval { .. }));
    assert!(evt.is_some());
    if let TuiEvent::ToolPendingApproval { tool_name, .. } = evt.unwrap() {
        assert_eq!(tool_name, "bash_exec");
    }
}

#[tokio::test]
async fn bridge_forwards_agent_cancelled() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(AgentCancelled {
        agent_name: "coder".into(),
        agent_id: "agent-1".into(),
        reason: "user requested".into(),
        turns_completed: 0,
    })
    .await;

    let events = drain(&mut rx).await;
    let evt = events
        .iter()
        .find(|e| matches!(e, TuiEvent::AgentCancelled { .. }));
    assert!(evt.is_some());
}

#[tokio::test]
async fn bridge_forwards_file_edits_committed() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    let mut diffs = std::collections::HashMap::new();
    diffs.insert("src/main.rs".to_string(), "diff content".to_string());

    bus.emit(FileEditsCommitted {
        files: vec!["src/main.rs".to_string()],
        total_lines_added: 10,
        total_lines_removed: 5,
        diffs,
    })
    .await;

    let events = drain(&mut rx).await;
    let evt = events
        .iter()
        .find(|e| matches!(e, TuiEvent::FileEditsCommitted { .. }));
    assert!(evt.is_some());
    if let TuiEvent::FileEditsCommitted {
        lines_added,
        lines_removed,
        ..
    } = evt.unwrap()
    {
        assert_eq!(*lines_added, 10);
        assert_eq!(*lines_removed, 5);
    }
}

// ── 2. State machine transitions ─────────────────────────────────────────────

#[test]
fn tool_lifecycle_produces_mutations() {
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "read_file".into(),
        tool_use_id: "tid-1".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        started_at: Instant::now(),
    });
    // Must produce a CommitLine with the tool call
    assert!(
        commit_texts(&s).iter().any(|t| t.contains("ReadFile")),
        "must commit tool call line"
    );

    s.mutations.clear();
    s.handle_event(TuiEvent::ToolCompleted {
        tool_name: "read_file".into(),
        tool_use_id: "tid-1".into(),
        duration_ms: 50.0,
        success: true,
        error: None,
    });
    assert!(
        commit_texts(&s).iter().any(|t| t.contains("✓")),
        "must commit success line"
    );
}

#[test]
fn tool_failure_produces_error_lines() {
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-f".into(),
        input: serde_json::json!({"command": "oops"}),
        started_at: Instant::now(),
    });
    s.mutations.clear();
    s.handle_event(TuiEvent::ToolCompleted {
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-f".into(),
        duration_ms: 10.0,
        success: false,
        error: Some("permission denied".into()),
    });
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("FAILED")),
        "must have FAILED line: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("permission denied")),
        "must have error detail: {texts:?}"
    );
}

#[test]
fn cost_accumulation_across_agents() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallComplete {
        agent_name: "planner".into(),
        input_tokens: 500,
        output_tokens: 200,
        cost_usd: 0.01,
        duration_ms: 300.0,
    });
    s.handle_event(TuiEvent::LlmCallComplete {
        agent_name: "coder".into(),
        input_tokens: 1000,
        output_tokens: 400,
        cost_usd: 0.02,
        duration_ms: 600.0,
    });
    assert_eq!(s.total_input_tokens, 1500);
    assert_eq!(s.total_output_tokens, 600);
    assert!((s.total_cost_usd - 0.03).abs() < 1e-9);
    assert_eq!(s.agent_costs["planner"], 0.01);
    assert_eq!(s.agent_costs["coder"], 0.02);
}

#[test]
fn approval_auto_approved_for_low_risk() {
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolPendingApproval {
        agent_run_id: "run-1".into(),
        tool_name: "read_file".into(),
        tool_use_id: "tid-low".into(),
        input: serde_json::json!({"path": "src/main.rs"}),
        risk: RiskLevel::Low,
    });
    assert!(
        !s.approval_queue.has_pending(),
        "low risk must auto-approve"
    );
    assert_eq!(s.pending_gate_decisions.len(), 1);
    assert!(s.pending_gate_decisions[0].1, "must be approved=true");
}

#[test]
fn approval_gated_for_high_risk() {
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolPendingApproval {
        agent_run_id: "run-2".into(),
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-hi".into(),
        input: serde_json::json!({"command": "rm -rf /tmp/x"}),
        risk: RiskLevel::High,
    });
    assert!(s.approval_queue.has_pending(), "high risk must gate");
    assert_eq!(s.pending_gate_decisions.len(), 0);
    let texts = commit_texts(&s);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("⚠") && t.contains("BashExec")),
        "must emit inline approval prompt: {texts:?}"
    );
}

#[test]
fn phase_transitions_on_agent_start() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "planner".into(),
        call_index: 0,
    });
    assert_eq!(s.phase, WorkflowPhase::Planning);

    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    assert_eq!(s.phase, WorkflowPhase::Coding);

    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "qa".into(),
        call_index: 0,
    });
    assert_eq!(s.phase, WorkflowPhase::QaReview);
}

#[test]
fn task_complete_marks_done() {
    let mut s = make_state();
    s.handle_event(TuiEvent::TaskComplete {
        summary: "All done".into(),
        session: xaft_runtime::session::AgentSession::new(
            "test",
            std::path::PathBuf::from("."),
            "default".into(),
            "claude-3".into(),
        ),
    });
    assert!(s.task_done);
    assert_eq!(s.phase, WorkflowPhase::Done);
    // Ephemeral must be cleared
    assert!(
        s.mutations
            .iter()
            .any(|m| matches!(m, RenderMutation::SetEphemeral(None))),
        "must clear ephemeral on task complete"
    );
}

#[test]
fn task_complete_stores_session_for_resume() {
    // After TaskComplete, state.session must be Some so that the next task in
    // the same TUI session can pass resume_session_id and get prior context.
    let mut s = make_state();
    assert!(
        s.session.is_none(),
        "no session before first task completes"
    );

    let completed_session = xaft_runtime::session::AgentSession::new(
        "initial task",
        std::path::PathBuf::from("."),
        "default".into(),
        "claude-3".into(),
    );
    let session_id = completed_session.id.to_string();

    s.handle_event(TuiEvent::TaskComplete {
        summary: "done".into(),
        session: completed_session,
    });

    let stored = s
        .session
        .as_ref()
        .expect("session must be set after TaskComplete");
    assert_eq!(
        stored.id.to_string(),
        session_id,
        "stored session id must match"
    );
}

// ── 3. Approval flow ─────────────────────────────────────────────────────────

#[test]
fn keyboard_approve_resolves_pending() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolPendingApproval {
        agent_run_id: "run-3".into(),
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-ap".into(),
        input: serde_json::json!({"command": "rm -rf /tmp/xaft-test-dir"}),
        risk: RiskLevel::High,
    });
    assert!(s.approval_queue.has_pending());
    s.mutations.clear();

    s.handle_event(TuiEvent::Key(KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }));

    assert_eq!(s.pending_gate_decisions.len(), 1);
    assert!(s.pending_gate_decisions[0].1, "must be approved");
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("✓ Approved")),
        "must commit approved line: {texts:?}"
    );
}

#[test]
fn keyboard_reject_resolves_pending() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolPendingApproval {
        agent_run_id: "run-4".into(),
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-rej".into(),
        input: serde_json::json!({"command": "rm -rf /tmp/xaft-test-dir-2"}),
        risk: RiskLevel::High,
    });
    s.mutations.clear();

    s.handle_event(TuiEvent::Key(KeyEvent {
        code: KeyCode::Char('r'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }));

    assert_eq!(s.pending_gate_decisions.len(), 1);
    assert!(!s.pending_gate_decisions[0].1, "must be rejected");
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("✗ Rejected")),
        "must commit rejected line: {texts:?}"
    );
}

// ── 4. Streaming lifecycle ────────────────────────────────────────────────────

#[test]
fn stream_token_opens_stream() {
    let mut s = make_state();
    s.handle_event(TuiEvent::StreamToken {
        agent_name: "coder".into(),
        token: "hello ".into(),
    });
    assert!(s.stream_active);
    assert!(has_stream_token(&s));
}

#[test]
fn agent_output_flushes_stream_then_commits() {
    let mut s = make_state();
    s.handle_event(TuiEvent::StreamToken {
        agent_name: "coder".into(),
        token: "partial".into(),
    });
    s.mutations.clear();
    s.stream_active = true;

    s.handle_event(TuiEvent::AgentOutput {
        agent_name: "coder".into(),
        content: "Full authoritative response\nSecond line".into(),
    });

    // FlushStream must come first
    assert!(
        matches!(s.mutations.first(), Some(RenderMutation::FlushStream)),
        "first mutation must be FlushStream"
    );
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("Full authoritative")),
        "must commit agent output: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("Second line")),
        "must commit second line: {texts:?}"
    );
    assert!(
        !s.stream_active,
        "stream must be inactive after AgentOutput"
    );
}

#[test]
fn tool_started_flushes_stream() {
    let mut s = make_state();
    s.stream_active = true;
    s.mutations.push(RenderMutation::StreamToken {
        fragment: "pending".into(),
        style: xaft_tui::transcript::LineStyle::Dim,
    });
    s.mutations.clear();

    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "read_file".into(),
        tool_use_id: "tid-x".into(),
        input: serde_json::json!({"path": "foo.rs"}),
        started_at: Instant::now(),
    });

    assert!(
        has_flush_stream(&s),
        "ToolStarted must flush stream: {:?}",
        s.mutations
    );
}

// ── 5. Agent tracking ─────────────────────────────────────────────────────────

#[test]
fn agent_tracker_records_llm_start() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    assert!(s.agent_tracker.nodes.contains_key("coder"));
    assert_eq!(
        s.agent_tracker.nodes["coder"].status,
        xaft_tui::agent_tracker::AgentStatus::Thinking
    );
}

#[test]
fn agent_tracker_records_tool_start_and_complete() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "read_file".into(),
        tool_use_id: "t-1".into(),
        input: serde_json::json!({"path": "src/lib.rs"}),
        started_at: Instant::now(),
    });
    assert_eq!(
        s.agent_tracker.nodes["coder"].status,
        xaft_tui::agent_tracker::AgentStatus::ToolCalling
    );

    s.handle_event(TuiEvent::ToolCompleted {
        tool_name: "read_file".into(),
        tool_use_id: "t-1".into(),
        duration_ms: 12.0,
        success: true,
        error: None,
    });
    assert_eq!(
        s.agent_tracker.nodes["coder"].status,
        xaft_tui::agent_tracker::AgentStatus::Thinking
    );
    assert_eq!(s.agent_tracker.nodes["coder"].tool_calls_completed, 1);
}

#[test]
fn agent_tracker_reset_on_new_task() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    assert!(!s.agent_tracker.nodes.is_empty());
    s.reset_for_new_task();
    assert!(s.agent_tracker.nodes.is_empty());
}

#[test]
fn multiple_agents_tracked_independently() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "planner".into(),
        call_index: 0,
    });
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    assert!(s.agent_tracker.nodes.contains_key("planner"));
    assert!(s.agent_tracker.nodes.contains_key("coder"));
}

// ── 6. Per-agent cost breakdown ───────────────────────────────────────────────

#[test]
fn top_agents_by_cost_sorted() {
    let mut s = make_state();
    s.handle_event(TuiEvent::LlmCallComplete {
        agent_name: "qa".into(),
        input_tokens: 100,
        output_tokens: 50,
        cost_usd: 0.005,
        duration_ms: 100.0,
    });
    s.handle_event(TuiEvent::LlmCallComplete {
        agent_name: "coder".into(),
        input_tokens: 500,
        output_tokens: 200,
        cost_usd: 0.02,
        duration_ms: 500.0,
    });
    let top = s.top_agents_by_cost();
    assert_eq!(top[0].0, "coder", "highest cost agent must be first");
    assert_eq!(top[1].0, "qa");
}

// ── 7. Multi-signal workflow ──────────────────────────────────────────────────

#[test]
fn full_coder_to_qa_workflow_produces_mutations() {
    let mut s = make_state();

    // Planner starts
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "planner".into(),
        call_index: 0,
    });
    // Planner streams tokens
    for t in ["I'll", " plan", " this"] {
        s.handle_event(TuiEvent::StreamToken {
            agent_name: "planner".into(),
            token: t.into(),
        });
    }
    // Planner produces output
    s.handle_event(TuiEvent::AgentOutput {
        agent_name: "planner".into(),
        content: "Here is my plan".into(),
    });

    // Handoff to coder
    s.handle_event(TuiEvent::AgentHandoff {
        from_agent: "planner".into(),
        to_agent: "coder".into(),
        summary: "implement the plan".into(),
    });

    // Coder starts and calls a tool
    s.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "edit_file".into(),
        tool_use_id: "t-edit".into(),
        input: serde_json::json!({
            "path": "src/main.rs",
            "old_content": "fn old() {}\n",
            "new_content": "fn new() {}\n"
        }),
        started_at: Instant::now(),
    });
    s.handle_event(TuiEvent::ToolCompleted {
        tool_name: "edit_file".into(),
        tool_use_id: "t-edit".into(),
        duration_ms: 30.0,
        success: true,
        error: None,
    });

    // QA approval
    s.handle_event(TuiEvent::TaskComplete {
        summary: "approved".into(),
        session: xaft_runtime::session::AgentSession::new(
            "test",
            std::path::PathBuf::from("."),
            "default".into(),
            "claude-3".into(),
        ),
    });

    // Verify key mutations produced
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("planner")),
        "planner marker: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("Here is my plan")),
        "plan output: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("EditFile")),
        "edit tool: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("⎿")),
        "diff summary: {texts:?}"
    );
    assert!(s.task_done);
}

// ── 8. Theme consistency ──────────────────────────────────────────────────────

#[test]
fn dark_theme_has_distinct_error_and_success() {
    let theme = xaft_tui::Theme::dark();
    assert_ne!(
        theme.success, theme.error,
        "success and error colors must be distinct"
    );
    assert_ne!(theme.fg, theme.dim, "fg and dim must be distinct");
}

// ── 9. Concurrent signal emission ────────────────────────────────────────────

#[tokio::test]
async fn concurrent_tool_signals_received() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    let bus_clone = Arc::clone(&bus);
    let futs: Vec<_> = (0..5)
        .map(|i| {
            let b = Arc::clone(&bus_clone);
            tokio::spawn(async move {
                b.emit(ToolCallStarted {
                    tool_name: "read_file".into(),
                    tool_use_id: format!("tu-{i}"),
                    agent_id: None,
                    input: serde_json::json!({}),
                    cache_hit: false,
                })
                .await;
            })
        })
        .collect();

    for f in futs {
        f.await.unwrap();
    }

    let events = drain(&mut rx).await;
    let tool_starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, TuiEvent::ToolStarted { .. }))
        .collect();
    assert_eq!(tool_starts.len(), 5, "all 5 tool signals must arrive");
}

// ── 10. Inline diff tests ─────────────────────────────────────────────────────

#[test]
fn edit_file_diff_produces_add_and_remove_lines() {
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "edit_file".into(),
        tool_use_id: "t-diff".into(),
        input: serde_json::json!({
            "path": "src/main.py",
            "old_content": "import random\nprint(random.choice([1,2,3]))\n",
            "new_content": "import secrets\nprint(secrets.choice([1,2,3]))\n"
        }),
        started_at: Instant::now(),
    });
    s.mutations.clear();
    s.handle_event(TuiEvent::ToolCompleted {
        tool_name: "edit_file".into(),
        tool_use_id: "t-diff".into(),
        duration_ms: 8.0,
        success: true,
        error: None,
    });

    assert!(
        !s.pending_file_inputs.contains_key("t-diff"),
        "input consumed"
    );
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("⎿")),
        "must have ⎿ summary: {texts:?}"
    );

    let add_lines: Vec<_> = s
        .mutations
        .iter()
        .filter(|m| matches!(m, RenderMutation::CommitLine(l) if l.kind == LineKind::DiffAdd))
        .collect();
    let rem_lines: Vec<_> = s
        .mutations
        .iter()
        .filter(|m| matches!(m, RenderMutation::CommitLine(l) if l.kind == LineKind::DiffRemove))
        .collect();
    assert!(!add_lines.is_empty(), "must have DiffAdd lines");
    assert!(!rem_lines.is_empty(), "must have DiffRemove lines");
}

#[test]
fn write_file_produces_summary_line() {
    let mut s = make_state();
    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "write_file".into(),
        tool_use_id: "t-wf".into(),
        input: serde_json::json!({
            "path": "src/new.py",
            "content": "line1\nline2\nline3\n"
        }),
        started_at: Instant::now(),
    });
    s.mutations.clear();
    s.handle_event(TuiEvent::ToolCompleted {
        tool_name: "write_file".into(),
        tool_use_id: "t-wf".into(),
        duration_ms: 5.0,
        success: true,
        error: None,
    });

    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("⎿") && t.contains("3")),
        "must have ⎿ Added 3 lines: {texts:?}"
    );
}

// ── 11. Resize handling ───────────────────────────────────────────────────────

#[test]
fn resize_produces_resize_mutation_and_updates_size() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Resize(100, 30));
    assert_eq!(s.terminal_size, (100, 30));
    assert!(
        s.mutations.iter().any(|m| matches!(
            m,
            RenderMutation::Resize {
                cols: 100,
                rows: 30
            }
        )),
        "must produce Resize mutation"
    );
}

// ── 12. Exit behavior ─────────────────────────────────────────────────────────

#[test]
fn ctrl_c_double_press_sets_should_quit() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = make_state();
    let key = KeyEvent {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(key.clone()));
    assert!(s.cancel_requested);
    assert!(!s.should_quit);

    s.handle_event(TuiEvent::Key(key));
    assert!(s.should_quit, "second Ctrl+C must set should_quit");
}

#[test]
fn runtime_error_sets_error_phase_and_commits_line() {
    let mut s = make_state();
    s.handle_event(TuiEvent::RuntimeError("connection refused".into()));
    assert_eq!(s.phase, WorkflowPhase::Error);
    assert!(s.error_message.is_some());
    let texts = commit_texts(&s);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("✗") && t.contains("connection refused"))
    );
}

// ── 13. Agent handoff ─────────────────────────────────────────────────────────

#[test]
fn agent_handoff_commits_transition_line() {
    let mut s = make_state();
    s.handle_event(TuiEvent::AgentHandoff {
        from_agent: "planner".into(),
        to_agent: "coder".into(),
        summary: "start coding".into(),
    });
    let texts = commit_texts(&s);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("planner") && t.contains("coder")),
        "handoff must commit transition line: {texts:?}"
    );
}

// ── 14. Format helpers ────────────────────────────────────────────────────────

#[test]
fn format_elapsed_helpers() {
    use xaft_tui::state::{format_elapsed, format_tokens_compact};
    assert_eq!(format_elapsed(std::time::Duration::from_secs(45)), "45s");
    assert_eq!(format_elapsed(std::time::Duration::from_secs(125)), "2m 5s");
    assert_eq!(format_tokens_compact(8_600), "8.6k");
    assert_eq!(format_tokens_compact(1_200_000), "1.2M");
    assert_eq!(format_tokens_compact(42), "42");
}
