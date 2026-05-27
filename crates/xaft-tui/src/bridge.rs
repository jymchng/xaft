//! Signal-to-TUI event bridge.
//!
//! Subscribes to all relevant `SignalBus` signals and converts them to a
//! unified `TuiEvent` enum delivered via an unbounded mpsc channel.  The TUI
//! main loop receives `TuiEvent`s from a single channel — no direct signal
//! bus access in the render path.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::approval::RiskLevel;
use agtrs_runtime::signals::{
    AgentCancelled, AgentRunComplete, FileEditsCommitted as AgtrsFileEditsCommitted,
    ModelCallComplete, ModelCallStarted, SignalBus, ToolCallComplete, ToolCallStarted,
    ToolPendingApproval,
};
use xaft_agent::signals::{
    XaftAgentHandoff, XaftAgentOutput, XaftCommitCreated, XaftLlmCallStarting,
};
use xaft_runtime::session::AgentSession;

// ── TuiEvent ──────────────────────────────────────────────────────────────────

/// Unified event type consumed by the TUI state machine.
///
/// Produced by the signal bridge and terminal input handler; consumed by
/// `AppState::handle_event`.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    // ── Agent lifecycle ───────────────────────────────────────────────────────
    /// An LLM call is starting (model + agent name).
    LlmCallStarting {
        agent_name: String,
        call_index: usize,
    },

    /// An LLM call completed with usage data.
    LlmCallComplete {
        agent_name: String,
        input_tokens: usize,
        output_tokens: usize,
        cost_usd: f64,
        duration_ms: f64,
    },

    /// An agent produced final output text (non-streaming, per turn/finish).
    AgentOutput { agent_name: String, content: String },

    /// An agent run completed.
    AgentRunComplete {
        agent_name: String,
        turns: usize,
        total_cost_usd: f64,
    },

    /// An agent was cancelled.
    AgentCancelled { agent_name: String, reason: String },

    // ── Tool lifecycle ────────────────────────────────────────────────────────
    /// A tool call started.
    ToolStarted {
        tool_name: String,
        tool_use_id: String,
        input: serde_json::Value,
        started_at: Instant,
    },

    /// A tool call completed.
    ToolCompleted {
        tool_name: String,
        tool_use_id: String,
        duration_ms: f64,
        success: bool,
        error: Option<String>,
    },

    /// A tool requires approval before executing.
    ToolPendingApproval {
        agent_run_id: String,
        tool_name: String,
        tool_use_id: String,
        input: serde_json::Value,
        risk: RiskLevel,
    },

    // ── Agent handoff ─────────────────────────────────────────────────────────
    /// The dynamic handoff orchestrator transferred control between agents.
    AgentHandoff {
        from_agent: String,
        to_agent: String,
        summary: String,
    },

    // ── Streaming token ───────────────────────────────────────────────────────
    /// An incremental text token from the LLM during streaming.
    /// Used to update the live typing indicator in the conversation pane.
    StreamToken { agent_name: String, token: String },

    // ── Git ───────────────────────────────────────────────────────────────────
    /// An auto-commit was created.
    CommitCreated {
        agent_name: String,
        short_sha: String,
        message: String,
        files_changed: usize,
    },

    /// File edits were committed — carries per-file diffs for the diff viewer.
    FileEditsCommitted {
        /// List of changed file paths.
        files: Vec<String>,
        /// Total lines added across all files.
        lines_added: i64,
        /// Total lines removed across all files.
        lines_removed: i64,
        /// Per-file unified diff text (path → diff).
        diffs: std::collections::HashMap<String, String>,
    },

    // ── Session ───────────────────────────────────────────────────────────────
    /// Session state updated externally.
    SessionUpdate(AgentSession),

    // ── Terminal input ────────────────────────────────────────────────────────
    /// A keyboard key was pressed.
    Key(crossterm::event::KeyEvent),
    /// A mouse event occurred.
    Mouse(crossterm::event::MouseEvent),
    /// Terminal was resized.
    Resize(u16, u16),

    // ── Internal ──────────────────────────────────────────────────────────────
    /// 60fps render tick.
    Tick,
    /// Runtime error — display in TUI and optionally abort.
    RuntimeError(String),
    /// Task completed successfully.
    TaskComplete { summary: String },
}

// ── EventBridge ───────────────────────────────────────────────────────────────

/// Subscribes to all relevant signals and forwards them to the TUI event channel.
pub struct EventBridge {
    tx: mpsc::UnboundedSender<TuiEvent>,
}

impl EventBridge {
    /// Create a new bridge that sends events to `tx`.
    pub fn new(tx: mpsc::UnboundedSender<TuiEvent>) -> Self {
        Self { tx }
    }

    /// Subscribe to all relevant signals on `bus` and spawn background tasks
    /// that forward them as `TuiEvent`s.
    pub async fn attach(&self, bus: &Arc<SignalBus>) {
        self.attach_llm_signals(bus).await;
        self.attach_tool_signals(bus).await;
        self.attach_agent_signals(bus).await;
        self.attach_xaft_signals(bus).await;
    }

    fn send(&self, event: TuiEvent) {
        let _ = self.tx.send(event);
    }

    async fn attach_llm_signals(&self, bus: &Arc<SignalBus>) {
        let tx = self.tx.clone();
        bus.on::<ModelCallStarted>(move |ev| {
            let _ = tx.send(TuiEvent::LlmCallStarting {
                agent_name: ev.agent_name.clone(),
                call_index: 0, // approximate — signal doesn't carry index
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<ModelCallComplete>(move |ev| {
            let _ = tx.send(TuiEvent::LlmCallComplete {
                agent_name: ev.agent_name.clone(),
                input_tokens: ev.usage.input_tokens,
                output_tokens: ev.usage.output_tokens,
                cost_usd: ev.cost_usd,
                duration_ms: ev.duration_ms,
            });
        })
        .await;
    }

    async fn attach_tool_signals(&self, bus: &Arc<SignalBus>) {
        let tx = self.tx.clone();
        bus.on::<ToolCallStarted>(move |ev| {
            let _ = tx.send(TuiEvent::ToolStarted {
                tool_name: ev.tool_name.clone(),
                tool_use_id: ev.tool_use_id.clone(),
                input: ev.input.clone(),
                started_at: Instant::now(),
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<ToolCallComplete>(move |ev| {
            let _ = tx.send(TuiEvent::ToolCompleted {
                tool_name: ev.tool_name.clone(),
                tool_use_id: ev.tool_use_id.clone(),
                duration_ms: ev.duration_ms,
                success: ev.success,
                error: ev.error.clone(),
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<ToolPendingApproval>(move |ev| {
            let risk = RiskLevel::from_tool_call(&ev.tool_name, &ev.input);
            let _ = tx.send(TuiEvent::ToolPendingApproval {
                agent_run_id: ev.agent_run_id.clone(),
                tool_name: ev.tool_name.clone(),
                tool_use_id: ev.tool_use_id.clone(),
                input: ev.input.clone(),
                risk,
            });
        })
        .await;
    }

    async fn attach_agent_signals(&self, bus: &Arc<SignalBus>) {
        let tx = self.tx.clone();
        bus.on::<AgentRunComplete>(move |ev| {
            let _ = tx.send(TuiEvent::AgentRunComplete {
                agent_name: ev.agent_name.clone(),
                turns: ev.turns,
                total_cost_usd: ev.total_cost_usd,
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<AgentCancelled>(move |ev| {
            let _ = tx.send(TuiEvent::AgentCancelled {
                agent_name: ev.agent_name.clone(),
                reason: ev.reason.clone(),
            });
        })
        .await;
    }

    async fn attach_xaft_signals(&self, bus: &Arc<SignalBus>) {
        let tx = self.tx.clone();
        bus.on::<XaftAgentOutput>(move |ev| {
            if !ev.content.is_empty() {
                let _ = tx.send(TuiEvent::AgentOutput {
                    agent_name: ev.agent_name.clone(),
                    content: ev.content.clone(),
                });
            }
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<XaftLlmCallStarting>(move |ev| {
            let _ = tx.send(TuiEvent::LlmCallStarting {
                agent_name: ev.agent_name.clone(),
                call_index: ev.call_index,
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<XaftCommitCreated>(move |ev| {
            let _ = tx.send(TuiEvent::CommitCreated {
                agent_name: ev.agent_name.clone(),
                short_sha: ev.short_sha.clone(),
                message: ev.message.clone(),
                files_changed: ev.files_changed,
            });
        })
        .await;

        // Wire FileEditsCommitted so the diff viewer shows per-file changes.
        let tx = self.tx.clone();
        bus.on::<AgtrsFileEditsCommitted>(move |ev| {
            let _ = tx.send(TuiEvent::FileEditsCommitted {
                files: ev.files.clone(),
                lines_added: ev.total_lines_added,
                lines_removed: ev.total_lines_removed,
                diffs: ev.diffs.clone(),
            });
        })
        .await;

        // Wire XaftAgentHandoff so the TUI can display agent transitions.
        let tx = self.tx.clone();
        bus.on::<XaftAgentHandoff>(move |ev| {
            let _ = tx.send(TuiEvent::AgentHandoff {
                from_agent: ev.from_agent.clone(),
                to_agent: ev.to_agent.clone(),
                summary: ev.summary.clone(),
            });
        })
        .await;

        // Wire XaftStreamToken so per-token streaming updates the TUI live.
        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftStreamToken>(move |ev| {
            let _ = tx.send(TuiEvent::StreamToken {
                agent_name: ev.agent_name.clone(),
                token: ev.token.clone(),
            });
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::signals::SignalBus;

    fn make_bridge_and_bus() -> (
        Arc<SignalBus>,
        EventBridge,
        mpsc::UnboundedReceiver<TuiEvent>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let bus = Arc::new(SignalBus::new());
        let bridge = EventBridge::new(tx);
        (bus, bridge, rx)
    }

    #[tokio::test]
    async fn bridge_forwards_tool_started() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(ToolCallStarted {
            tool_name: "read_file".into(),
            tool_use_id: "tid-1".into(),
            agent_id: None,
            input: serde_json::json!({"path": "test.py"}),
            cache_hit: false,
        })
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let event = rx.try_recv().expect("expected TuiEvent");
        match event {
            TuiEvent::ToolStarted { tool_name, .. } => {
                assert_eq!(tool_name, "read_file");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_tool_completed() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(ToolCallComplete {
            tool_name: "write_file".into(),
            tool_use_id: "tid-2".into(),
            agent_id: None,
            duration_ms: 42.0,
            success: true,
            error: None,
        })
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let event = rx.try_recv().expect("expected TuiEvent");
        match event {
            TuiEvent::ToolCompleted { success, .. } => assert!(success),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_pending_approval() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(ToolPendingApproval {
            agent_id: "agent-1".into(),
            agent_run_id: "run-1".into(),
            tool_name: "bash_exec".into(),
            tool_use_id: "tid-3".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        })
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let event = rx.try_recv().expect("expected TuiEvent");
        match event {
            TuiEvent::ToolPendingApproval { tool_name, .. } => {
                assert_eq!(tool_name, "bash_exec");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_xaft_agent_output() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftAgentOutput {
            agent_name: "coder".into(),
            content: "I've replaced all random.choice calls.".into(),
        })
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let event = rx.try_recv().expect("expected TuiEvent");
        match event {
            TuiEvent::AgentOutput {
                agent_name,
                content,
            } => {
                assert_eq!(agent_name, "coder");
                assert!(content.contains("random.choice"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_agent_output_not_forwarded() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftAgentOutput {
            agent_name: "coder".into(),
            content: String::new(), // empty — should not be forwarded
        })
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert!(
            rx.try_recv().is_err(),
            "empty output should not be forwarded"
        );
    }
}
