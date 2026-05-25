//! Integration tests for the xaft TUI.
//!
//! Tests cover the full event pipeline:
//!   signal emission → EventBridge → TuiEvent → AppState mutations → widget rendering
//!
//! Uses mock signal buses and direct event injection — no real terminal required.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use agtrs_runtime::signals::{
    AgentCancelled, AgentRunComplete, FileEditsCommitted, ModelCallComplete, SignalBus,
    ToolCallComplete, ToolCallStarted, ToolPendingApproval,
};
use agtrs_runtime::transport::{StopReason, TokenUsage};

use xaft_tui::bridge::{EventBridge, TuiEvent};
use xaft_tui::state::{AppState, FocusedPanel, ToolEntryState, WorkflowPhase};
use xaft_tui::widgets::diff::DiffWidget;

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

fn make_state(task: &str) -> AppState {
    AppState::new(task)
}

async fn drain(rx: &mut mpsc::UnboundedReceiver<TuiEvent>) -> Vec<TuiEvent> {
    tokio::time::sleep(Duration::from_millis(30)).await;
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    events
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
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TuiEvent::ToolCompleted { success: true, .. }))
    );
}

#[tokio::test]
async fn bridge_forwards_llm_complete_from_signal() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(ModelCallComplete {
        model: "claude-3-5-sonnet".into(),
        agent_id: None,
        agent_name: "coder".into(),
        usage: TokenUsage::new(500, 1000),
        duration_ms: 1200.0,
        stop_reason: StopReason::EndTurn,
        cost_usd: 0.003,
        total_tokens: 1500,
        turns: 1,
    })
    .await;

    let events = drain(&mut rx).await;
    let found = events.iter().any(|e| {
        if let TuiEvent::LlmCallComplete {
            cost_usd,
            agent_name,
            ..
        } = e
        {
            (*cost_usd - 0.003).abs() < 1e-9 && agent_name == "coder"
        } else {
            false
        }
    });
    assert!(found, "expected LlmCallComplete with cost 0.003");
}

#[tokio::test]
async fn bridge_forwards_pending_approval_from_signal() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(ToolPendingApproval {
        agent_id: "agent-1".into(),
        agent_run_id: "run-1".into(),
        tool_name: "bash_exec".into(),
        tool_use_id: "tu-danger".into(),
        input: serde_json::json!({"command": "rm -rf /"}),
    })
    .await;

    let events = drain(&mut rx).await;
    let found = events.iter().find(|e| {
        matches!(e, TuiEvent::ToolPendingApproval { tool_name, .. } if tool_name == "bash_exec")
    });
    assert!(
        found.is_some(),
        "expected ToolPendingApproval for bash_exec"
    );
}

#[tokio::test]
async fn bridge_forwards_file_edits_committed() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    let mut diffs = HashMap::new();
    diffs.insert("src/main.rs".into(), "+fn new() {}".into());

    bus.emit(FileEditsCommitted {
        files: vec!["src/main.rs".into()],
        total_lines_added: 5,
        total_lines_removed: 2,
        diffs,
    })
    .await;

    let events = drain(&mut rx).await;
    let found = events.iter().find(|e| {
        if let TuiEvent::FileEditsCommitted {
            files, lines_added, ..
        } = e
        {
            files.contains(&"src/main.rs".to_string()) && *lines_added == 5
        } else {
            false
        }
    });
    assert!(found.is_some(), "expected FileEditsCommitted event");
}

#[tokio::test]
async fn bridge_forwards_agent_cancelled() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    bus.emit(AgentCancelled {
        agent_id: "a1".into(),
        agent_name: "coder".into(),
        reason: "user interrupt".into(),
        turns_completed: 3,
    })
    .await;

    let events = drain(&mut rx).await;
    let found = events.iter().any(
        |e| matches!(e, TuiEvent::AgentCancelled { reason, .. } if reason == "user interrupt"),
    );
    assert!(found);
}

// ── 2. State machine transitions ─────────────────────────────────────────────

#[tokio::test]
async fn state_tool_lifecycle_start_complete() {
    let mut state = make_state("task");

    // Tool starts → active_tool set
    state.handle_event(TuiEvent::ToolStarted {
        tool_name: "read_file".into(),
        tool_use_id: "tid-1".into(),
        input: serde_json::json!({"path": "a.rs"}),
        started_at: std::time::Instant::now(),
    });
    assert!(state.active_tool.is_some());
    assert_eq!(state.tool_log.len(), 1);
    assert_eq!(state.tool_log[0].state, ToolEntryState::Running);

    // Tool completes → active_tool cleared, entry updated
    state.handle_event(TuiEvent::ToolCompleted {
        tool_name: "read_file".into(),
        tool_use_id: "tid-1".into(),
        duration_ms: 15.0,
        success: true,
        error: None,
    });
    assert!(state.active_tool.is_none());
    assert_eq!(state.tool_log[0].state, ToolEntryState::Done);
    assert_eq!(state.tool_log[0].duration_ms, Some(15.0));
}

#[tokio::test]
async fn state_tool_failure_adds_error_output() {
    let mut state = make_state("task");

    state.handle_event(TuiEvent::ToolStarted {
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-fail".into(),
        input: serde_json::json!({}),
        started_at: std::time::Instant::now(),
    });
    state.handle_event(TuiEvent::ToolCompleted {
        tool_name: "bash_exec".into(),
        tool_use_id: "tid-fail".into(),
        duration_ms: 5.0,
        success: false,
        error: Some("exit code 1".into()),
    });

    assert_eq!(state.tool_log[0].state, ToolEntryState::Failed);
    // Error should appear in output
    let has_error_line = state
        .output_lines
        .iter()
        .any(|l| l.text.contains("exit code 1"));
    assert!(has_error_line);
}

#[tokio::test]
async fn state_cost_accumulates_across_multiple_llm_calls() {
    let mut state = make_state("task");

    for i in 0..5 {
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: if i % 2 == 0 {
                "coder".into()
            } else {
                "qa".into()
            },
            input_tokens: 100,
            output_tokens: 200,
            cost_usd: 0.002,
            duration_ms: 500.0,
        });
    }

    assert_eq!(state.total_llm_calls, 5);
    assert!((state.total_cost_usd - 0.010).abs() < 1e-9);
    assert_eq!(state.total_tokens(), 1500);
    // Per-agent breakdown
    assert!(state.agent_costs.contains_key("coder"));
    assert!(state.agent_costs.contains_key("qa"));
    let coder_cost = state.agent_costs["coder"];
    let qa_cost = state.agent_costs["qa"];
    assert!((coder_cost + qa_cost - 0.010).abs() < 1e-9);
}

#[tokio::test]
async fn state_pending_approval_captures_focus() {
    let mut state = make_state("task");
    assert_eq!(state.focused_panel, FocusedPanel::Conversation);

    state.handle_event(TuiEvent::ToolPendingApproval {
        agent_run_id: "run-1".into(),
        tool_name: "bash_exec".into(),
        tool_use_id: "tu-1".into(),
        input: serde_json::json!({"command": "ls"}),
    });

    assert_eq!(state.focused_panel, FocusedPanel::Approval);
    assert!(state.pending_approval.is_some());
}

#[tokio::test]
async fn state_file_edits_committed_updates_diff_state() {
    let mut state = make_state("task");

    let mut diffs = HashMap::new();
    diffs.insert("src/lib.rs".into(), "+fn bar() {}".into());
    diffs.insert("src/main.rs".into(), "+use lib::bar;".into());

    state.handle_event(TuiEvent::FileEditsCommitted {
        files: vec!["src/lib.rs".into(), "src/main.rs".into()],
        lines_added: 10,
        lines_removed: 3,
        diffs,
    });

    assert_eq!(state.diff.diffs.len(), 2);
    assert_eq!(state.diff.total_added, 10);
    assert_eq!(state.diff.total_removed, 3);
    assert!(DiffWidget::has_diffs(&state.diff));

    let summary = state.edits_summary().unwrap();
    assert!(summary.contains("2 file"));
}

#[tokio::test]
async fn state_phase_transitions_with_agent_name() {
    let mut state = make_state("task");
    assert_eq!(state.phase, WorkflowPhase::Planning);

    state.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "coder".into(),
        call_index: 0,
    });
    assert_eq!(state.phase, WorkflowPhase::Coding);

    state.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "qa".into(),
        call_index: 1,
    });
    assert_eq!(state.phase, WorkflowPhase::QaReview);

    state.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "fixer".into(),
        call_index: 2,
    });
    assert_eq!(state.phase, WorkflowPhase::Fixing);
}

#[tokio::test]
async fn state_task_complete_sets_done() {
    let mut state = make_state("task");
    state.handle_event(TuiEvent::TaskComplete {
        summary: "replaced random.choice with secrets.choice".into(),
    });
    assert_eq!(state.phase, WorkflowPhase::Done);
    assert!(state.task_done);
    assert_eq!(
        state.final_summary,
        "replaced random.choice with secrets.choice"
    );
}

#[tokio::test]
async fn state_agent_cancelled_sets_error_phase() {
    let mut state = make_state("task");
    state.handle_event(TuiEvent::AgentCancelled {
        agent_name: "coder".into(),
        reason: "timeout".into(),
    });
    assert_eq!(state.phase, WorkflowPhase::Error);
}

// ── 3. Approval flow ─────────────────────────────────────────────────────────

#[tokio::test]
async fn approval_gate_responds_approve() {
    use agtrs_runtime::approval::ApprovalGate;
    use xaft_tui::approval_gate::TuiApprovalGate;

    let signals = Arc::new(SignalBus::new());
    let gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));
    let gate_for_response = Arc::clone(&gate);

    let input = serde_json::json!({"command": "ls"});
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        gate_for_response.respond("tu-approve", true).await;
    });

    let result = gate.request("bash_exec", "tu-approve", &input).await;
    assert!(result, "user approved: result should be true");
}

#[tokio::test]
async fn approval_gate_responds_reject() {
    use agtrs_runtime::approval::ApprovalGate;
    use xaft_tui::approval_gate::TuiApprovalGate;

    let signals = Arc::new(SignalBus::new());
    let gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));
    let gate_for_response = Arc::clone(&gate);

    let input = serde_json::json!({"command": "rm -rf /"});
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        gate_for_response.respond("tu-reject", false).await;
    });

    let result = gate.request("bash_exec", "tu-reject", &input).await;
    assert!(!result, "user rejected: result should be false");
}

#[tokio::test]
async fn approval_gate_cancel_all_rejects() {
    use xaft_tui::approval_gate::TuiApprovalGate;

    use agtrs_runtime::approval::ApprovalGate;

    let signals = Arc::new(SignalBus::new());
    let gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));

    // Spawn two pending requests
    let gate1 = Arc::clone(&gate);
    let input1 = serde_json::json!({});
    let h1 = tokio::spawn(async move { gate1.request("tool_a", "tu-1", &input1).await });
    let gate2 = Arc::clone(&gate);
    let input2 = serde_json::json!({});
    let h2 = tokio::spawn(async move { gate2.request("tool_b", "tu-2", &input2).await });

    // Give requests time to register
    tokio::time::sleep(Duration::from_millis(20)).await;
    gate.cancel_all().await;

    // Both should have been rejected
    let v1 = h1.await.unwrap_or(true);
    let v2 = h2.await.unwrap_or(true);
    assert!(!v1, "cancel_all should reject pending approval 1");
    assert!(!v2, "cancel_all should reject pending approval 2");
}

#[tokio::test]
async fn approval_gate_emits_pending_signal_on_request() {
    use agtrs_runtime::approval::ApprovalGate;
    use xaft_tui::approval_gate::TuiApprovalGate;

    let signals = Arc::new(SignalBus::new());
    let mut rx = signals.subscribe::<ToolPendingApproval>().await;
    let gate = Arc::new(TuiApprovalGate::new(Arc::clone(&signals)));
    let gate_clone = Arc::clone(&gate);

    let input = serde_json::json!({"path": "/etc"});
    // Respond immediately so request() doesn't block
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        gate_clone.respond("tu-signal", false).await;
    });

    gate.request("list_files", "tu-signal", &input).await;

    let signal = rx.try_recv().ok();
    assert!(
        signal.is_some(),
        "approval request should emit ToolPendingApproval signal"
    );
    assert_eq!(signal.unwrap().tool_name, "list_files");
}

// ── 4. Output buffer management ───────────────────────────────────────────────

#[tokio::test]
async fn output_buffer_bounded_by_max() {
    let mut state = make_state("task");
    // Flood the output buffer
    for i in 0..2100 {
        state.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: format!("line {i}"),
        });
    }
    assert!(
        state.output_lines.len() <= xaft_tui::state::MAX_OUTPUT_LINES,
        "output buffer must be clamped to MAX_OUTPUT_LINES"
    );
}

#[tokio::test]
async fn visible_output_respects_height() {
    let mut state = make_state("task");
    for i in 0..50 {
        state.handle_event(TuiEvent::AgentOutput {
            agent_name: "x".into(),
            content: format!("line {i}"),
        });
    }
    let visible = state.visible_output(10);
    assert_eq!(visible.len(), 10);
    // Auto-scroll: last visible is newest line
    assert_eq!(visible.last().unwrap().text, "line 49");
}

#[tokio::test]
async fn scroll_up_disables_auto_scroll() {
    let mut state = make_state("task");
    for i in 0..20 {
        state.handle_event(TuiEvent::AgentOutput {
            agent_name: "x".into(),
            content: format!("msg {i}"),
        });
    }
    assert!(state.output_auto_scroll);

    // Scroll up
    state.handle_event(TuiEvent::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Up,
        crossterm::event::KeyModifiers::NONE,
    )));
    assert!(!state.output_auto_scroll);

    // Jump to end re-enables auto scroll
    state.handle_event(TuiEvent::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::End,
        crossterm::event::KeyModifiers::NONE,
    )));
    assert!(state.output_auto_scroll);
}

// ── 5. Per-agent cost breakdown ───────────────────────────────────────────────

#[tokio::test]
async fn per_agent_cost_breakdown_sorted_by_cost() {
    let mut state = make_state("task");

    // Coder: 3 calls at $0.005 each = $0.015
    for _ in 0..3 {
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 200,
            output_tokens: 400,
            cost_usd: 0.005,
            duration_ms: 800.0,
        });
    }
    // QA: 1 call at $0.002 = $0.002
    state.handle_event(TuiEvent::LlmCallComplete {
        agent_name: "qa".into(),
        input_tokens: 100,
        output_tokens: 200,
        cost_usd: 0.002,
        duration_ms: 400.0,
    });

    let breakdown = state.top_agents_by_cost();
    assert!(!breakdown.is_empty());
    // Coder should be first (highest cost)
    assert_eq!(breakdown[0].0, "coder");
    assert!((breakdown[0].1 - 0.015).abs() < 1e-9);
    assert_eq!(breakdown[1].0, "qa");
    assert!((breakdown[1].1 - 0.002).abs() < 1e-9);
}

// ── 6. Multi-signal workflow (coder → QA → approved) ────────────────────────

#[tokio::test]
async fn full_workflow_event_sequence() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    // Planner phase
    bus.emit(agtrs_runtime::signals::ModelCallStarted {
        model: "claude-3-5-sonnet".into(),
        agent_id: None,
        agent_name: "coder".into(),
        messages_count: 1,
        input_tokens_estimate: 500,
    })
    .await;

    // Tool calls during coding
    bus.emit(ToolCallStarted {
        tool_name: "read_file".into(),
        tool_use_id: "t1".into(),
        agent_id: None,
        input: serde_json::json!({"path": "password_generator.py"}),
        cache_hit: false,
    })
    .await;
    bus.emit(ToolCallComplete {
        tool_name: "read_file".into(),
        tool_use_id: "t1".into(),
        agent_id: None,
        duration_ms: 8.0,
        success: true,
        error: None,
    })
    .await;
    bus.emit(ToolCallStarted {
        tool_name: "write_file".into(),
        tool_use_id: "t2".into(),
        agent_id: None,
        input: serde_json::json!({"path": "password_generator.py"}),
        cache_hit: false,
    })
    .await;
    bus.emit(ToolCallComplete {
        tool_name: "write_file".into(),
        tool_use_id: "t2".into(),
        agent_id: None,
        duration_ms: 5.0,
        success: true,
        error: None,
    })
    .await;

    // File edits committed
    let mut diffs = HashMap::new();
    diffs.insert(
        "password_generator.py".into(),
        "-import random\n+import secrets\n".into(),
    );
    bus.emit(FileEditsCommitted {
        files: vec!["password_generator.py".into()],
        total_lines_added: 1,
        total_lines_removed: 1,
        diffs,
    })
    .await;

    // LLM complete
    bus.emit(ModelCallComplete {
        model: "claude-3-5-sonnet".into(),
        agent_id: None,
        agent_name: "coder".into(),
        usage: TokenUsage::new(800, 1200),
        duration_ms: 2000.0,
        stop_reason: StopReason::EndTurn,
        cost_usd: 0.005,
        total_tokens: 2000,
        turns: 3,
    })
    .await;

    // Agent run complete
    bus.emit(AgentRunComplete {
        agent_id: "coder-1".into(),
        agent_name: "coder".into(),
        turns: 3,
        total_usage: TokenUsage::new(800, 1200),
        total_cost_usd: 0.005,
        stop_reason: StopReason::EndTurn,
    })
    .await;

    let events = drain(&mut rx).await;

    // Verify all expected event types arrived
    assert!(
        events.iter().any(
            |e| matches!(e, TuiEvent::ToolStarted { tool_name, .. } if tool_name == "read_file")
        )
    );
    assert!(events.iter().any(
        |e| matches!(e, TuiEvent::ToolStarted { tool_name, .. } if tool_name == "write_file")
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TuiEvent::ToolCompleted { success: true, .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TuiEvent::FileEditsCommitted { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TuiEvent::LlmCallComplete { cost_usd, .. } if *cost_usd > 0.0))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TuiEvent::AgentRunComplete { .. }))
    );

    // Apply to state and verify final state
    let mut state = make_state("fix password security");
    for evt in events {
        state.handle_event(evt);
    }

    assert!(state.total_llm_calls >= 1);
    assert!(state.total_cost_usd > 0.0);
    assert!(state.diff.has_diffs(), "file changes must be tracked");
    assert!(
        state.tool_log.len() >= 2,
        "should have read+write tool entries"
    );
}

// ── 7. Diff widget integration ───────────────────────────────────────────────

#[tokio::test]
async fn diff_widget_shows_after_file_edits_committed() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;
    use xaft_tui::theme::Theme;

    let mut state = make_state("task");
    assert!(!DiffWidget::has_diffs(&state.diff));

    // Emit file edits
    let mut diffs = HashMap::new();
    diffs.insert(
        "src/main.rs".into(),
        "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n fn main() {}\n+fn new() {}\n"
            .into(),
    );
    state.handle_event(TuiEvent::FileEditsCommitted {
        files: vec!["src/main.rs".into()],
        lines_added: 1,
        lines_removed: 0,
        diffs,
    });

    assert!(DiffWidget::has_diffs(&state.diff));

    // Render without panic
    let theme = Theme::dark();
    let widget = DiffWidget::new(&state.diff, &theme, false);
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
    widget.render(Rect::new(0, 0, 80, 20), &mut buf);
}

// ── 8. Theme consistency ──────────────────────────────────────────────────────

#[tokio::test]
async fn all_themes_produce_distinct_success_error_colors() {
    use xaft_config::types::TuiTheme;
    use xaft_tui::theme::Theme;

    for theme_kind in [TuiTheme::Dark, TuiTheme::Light, TuiTheme::Solarized] {
        let theme = Theme::from_config(&theme_kind);
        assert_ne!(
            theme.success, theme.error,
            "{:?}: success and error colors must differ",
            theme_kind
        );
    }
}

// ── 9. Concurrent signal emission ────────────────────────────────────────────

#[tokio::test]
async fn concurrent_tool_signals_all_received() {
    let (bus, bridge, mut rx) = make_bridge();
    bridge.attach(&bus).await;

    let bus2 = Arc::clone(&bus);
    let h1 = tokio::spawn(async move {
        for i in 0..5 {
            bus2.emit(ToolCallStarted {
                tool_name: format!("tool_{i}"),
                tool_use_id: format!("tid_{i}"),
                agent_id: None,
                input: serde_json::json!({}),
                cache_hit: false,
            })
            .await;
        }
    });

    let bus3 = Arc::clone(&bus);
    let h2 = tokio::spawn(async move {
        for i in 5..10 {
            bus3.emit(ToolCallStarted {
                tool_name: format!("tool_{i}"),
                tool_use_id: format!("tid_{i}"),
                agent_id: None,
                input: serde_json::json!({}),
                cache_hit: false,
            })
            .await;
        }
    });

    h1.await.unwrap();
    h2.await.unwrap();

    let events = drain(&mut rx).await;
    let tool_starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, TuiEvent::ToolStarted { .. }))
        .collect();
    assert_eq!(
        tool_starts.len(),
        10,
        "all 10 concurrent tool signals must arrive"
    );
}
