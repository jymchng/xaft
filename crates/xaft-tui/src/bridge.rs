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
    XaftAgentHandoff, XaftAgentOutput, XaftBackgroundPipelineComplete, XaftCommandRegistered,
    XaftCommitCreated, XaftContextCompacted, XaftLlmCallStarting, XaftMetaAgentCompleted,
    XaftMetaAgentSpawned,
};
use xaft_runtime::session::AgentSession;

// ── TuiEvent ──────────────────────────────────────────────────────────────────

/// Unified event type consumed by the TUI state machine.
///
/// Produced by the signal bridge and terminal input handler; consumed by
/// `AppState::handle_event`.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// `ask_user`: a round of questions for the questions overlay, posted by
    /// [`TuiQuestionGate`](crate::questions::TuiQuestionGate).
    QuestionsAsked {
        /// The gate's request id, echoed back with the answers.
        id: String,
        /// The questions, in order.
        questions: Vec<xaft_runtime::phased::question::UserQuestion>,
        /// `tools.question_timeout_s`, for the countdown.
        timeout_s: f64,
    },
    /// The round `id` timed out: close its overlay.
    QuestionsExpired {
        /// The gate's request id.
        id: String,
    },
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
    /// Bracketed-paste payload — preserves newlines, unlike raw key events.
    Paste(String),

    // ── Internal ──────────────────────────────────────────────────────────────
    /// Runtime error — display in TUI and optionally abort.
    RuntimeError(String),
    /// Task completed successfully.
    TaskComplete {
        summary: String,
        /// The session that completed — used to set `resume_session_id` on
        /// the next task so the agent has full prior-conversation context.
        session: xaft_runtime::session::AgentSession,
    },

    // ── Memory ───────────────────────────────────────────────────────────────
    /// A memory entry was stored.
    MemoryStored {
        content_summary: String,
        tags: Vec<String>,
        agent_name: String,
    },
    /// A memory recall completed.
    MemoryRecalled { query: String, results_count: usize },

    // ── Background pipeline ───────────────────────────────────────────────────
    /// A background pipeline completed (success or failure).
    ///
    /// Generated internally by `AppState::handle_event` when a `TaskComplete`
    /// or `RuntimeError` arrives while the state is in background-routing mode.
    /// Also emitted by `EventBridge` when the `XaftBackgroundPipelineComplete`
    /// signal fires (for external signal-bus consumers).
    BackgroundPipelineComplete {
        id: u64,
        task_summary: String,
        success: bool,
    },

    // ── Compaction ────────────────────────────────────────────────────────────
    /// Context was compacted — show a marker line in the transcript.
    ContextCompacted {
        agent_name: String,
        messages_removed: usize,
        tokens_saved: u64,
    },

    // ── Phased workflows ─────────────────────────────────────────────────────
    /// A phased workflow run started or resumed.
    WorkflowRunStarted {
        run_id: String,
        workflow_name: String,
        total_phases: usize,
        resumed: bool,
    },
    /// A workflow phase was entered.
    WorkflowPhaseStarted {
        workflow_name: String,
        phase_name: String,
        phase_index: usize,
        total_phases: usize,
        iteration: u32,
        model: String,
    },
    /// A workflow phase finished and chose its edge.
    WorkflowPhaseCompleted {
        phase_name: String,
        approved: Option<bool>,
        transition: Option<String>,
        next_phase: Option<String>,
    },
    /// A phased workflow run stopped.
    WorkflowRunCompleted {
        run_id: String,
        workflow_name: String,
        status: String,
        error: Option<String>,
    },

    // ── Meta-agent ────────────────────────────────────────────────────────────
    /// A specialist agent was spawned by the meta agent.
    SpecialistAgentStarted {
        agent_name: String,
        role: String,
        task_preview: String,
        spawn_mode: String,
    },

    /// A specialist agent spawned by the meta agent completed.
    SpecialistAgentCompleted {
        agent_name: String,
        success: bool,
        duration_ms: f64,
        output_preview: String,
    },

    // ── PRD-60: Dynamic command registration ─────────────────────────────────
    /// A new slash command was registered at runtime (from a skill, dynamic
    /// tool, or MCP server). The TUI updates its `CommandCatalog` in response.
    CommandRegistered {
        /// Canonical command name (no leading slash).
        name: String,
        /// Provenance string: `"skill:<name>"`, `"dynamic"`, or `"mcp:<server>"`.
        source: String,
        /// One-line description for the palette and `/help`.
        description: String,
        /// Optional argument syntax hint.
        args_hint: Option<String>,
    },
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

        // Wire XaftBackgroundPipelineComplete so external consumers see the event.
        let tx = self.tx.clone();
        bus.on::<XaftBackgroundPipelineComplete>(move |ev| {
            let _ = tx.send(TuiEvent::BackgroundPipelineComplete {
                id: ev.id,
                task_summary: ev.task_summary.clone(),
                success: ev.success,
            });
        })
        .await;

        // Wire XaftContextCompacted so the TUI shows a [compact] marker line.
        let tx = self.tx.clone();
        bus.on::<XaftContextCompacted>(move |ev| {
            let _ = tx.send(TuiEvent::ContextCompacted {
                agent_name: ev.agent_name.clone(),
                messages_removed: ev.messages_removed,
                tokens_saved: ev.tokens_saved_estimate,
            });
        })
        .await;

        // Wire phased-workflow lifecycle signals (agenthicc's WorkflowRun*/
        // WorkflowPhase* events) so the transcript shows Phase N/M progress.
        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftWorkflowRunStarted>(move |ev| {
            let _ = tx.send(TuiEvent::WorkflowRunStarted {
                run_id: ev.run_id.clone(),
                workflow_name: ev.workflow_name.clone(),
                total_phases: ev.phase_names.len(),
                resumed: ev.resumed,
            });
        })
        .await;
        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftWorkflowPhaseStarted>(move |ev| {
            let _ = tx.send(TuiEvent::WorkflowPhaseStarted {
                workflow_name: ev.workflow_name.clone(),
                phase_name: ev.phase_name.clone(),
                phase_index: ev.phase_index,
                total_phases: ev.total_phases,
                iteration: ev.iteration,
                model: ev.model.clone(),
            });
        })
        .await;
        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftWorkflowPhaseCompleted>(move |ev| {
            let _ = tx.send(TuiEvent::WorkflowPhaseCompleted {
                phase_name: ev.phase_name.clone(),
                approved: ev.approved,
                transition: ev.transition.clone(),
                next_phase: ev.next_phase.clone(),
            });
        })
        .await;
        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftWorkflowRunCompleted>(move |ev| {
            let _ = tx.send(TuiEvent::WorkflowRunCompleted {
                run_id: ev.run_id.clone(),
                workflow_name: ev.workflow_name.clone(),
                status: ev.status.clone(),
                error: ev.error.clone(),
            });
        })
        .await;

        // Wire meta-agent signals so the TUI shows specialist agent activity.
        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftMetaAgentSpawned>(move |ev| {
            let _ = tx.send(TuiEvent::SpecialistAgentStarted {
                agent_name: ev.blueprint_name.clone(),
                role: ev.blueprint_role.clone(),
                task_preview: ev.task_preview.clone(),
                spawn_mode: ev.spawn_mode.clone(),
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<xaft_agent::XaftMetaAgentCompleted>(move |ev| {
            let _ = tx.send(TuiEvent::SpecialistAgentCompleted {
                agent_name: ev.agent_name.clone(),
                success: ev.success,
                duration_ms: ev.duration_ms,
                output_preview: ev.output_preview.clone(),
            });
        })
        .await;

        // Wire XaftCommandRegistered so the TUI can update CommandCatalog immediately.
        let tx = self.tx.clone();
        bus.on::<XaftCommandRegistered>(move |ev| {
            let _ = tx.send(TuiEvent::CommandRegistered {
                name: ev.name.clone(),
                source: ev.source.clone(),
                description: ev.description.clone(),
                args_hint: ev.args_hint.clone(),
            });
        })
        .await;

        // Wire memory signals if xaft-memory is available.
        self.attach_memory_signals(bus).await;
    }

    #[cfg(feature = "memory")]
    async fn attach_memory_signals(&self, bus: &Arc<SignalBus>) {
        use xaft_memory::signals::{XaftMemoryRecalled, XaftMemoryStored};

        let tx = self.tx.clone();
        bus.on::<XaftMemoryStored>(move |ev| {
            let _ = tx.send(TuiEvent::MemoryStored {
                content_summary: ev.content_summary.clone(),
                tags: ev.tags.clone(),
                agent_name: ev.agent_name.clone(),
            });
        })
        .await;

        let tx = self.tx.clone();
        bus.on::<XaftMemoryRecalled>(move |ev| {
            let _ = tx.send(TuiEvent::MemoryRecalled {
                query: ev.query.clone(),
                results_count: ev.results_count,
            });
        })
        .await;
    }

    #[cfg(not(feature = "memory"))]
    async fn attach_memory_signals(&self, _bus: &Arc<SignalBus>) {}
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

    // ── Signal forwarding ────────────────────────────────────────────────
    //
    // `attach` subscribes to sixteen signals but only five had a test, so the
    // eleven forwarders below had never executed. Each is a field-by-field
    // copy from the agtrs/xaft signal into the `TuiEvent`; a wrong field name
    // or a dropped field compiles fine and silently corrupts the transcript,
    // so each test asserts the values actually crossed the channel.

    fn usage(input: usize, output: usize) -> agtrs_runtime::transport::TokenUsage {
        agtrs_runtime::transport::TokenUsage::new(input, output)
    }

    async fn next_event(rx: &mut mpsc::UnboundedReceiver<TuiEvent>) -> TuiEvent {
        // The bus delivers through a spawned task, so a short settle window is
        // required before the channel is guaranteed to hold the event.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        rx.try_recv().expect("expected a TuiEvent")
    }

    #[tokio::test]
    async fn bridge_forwards_model_call_started() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(ModelCallStarted {
            model: "claude-sonnet".into(),
            agent_id: Some("agent-7".into()),
            agent_name: "coder".into(),
            messages_count: 4,
            input_tokens_estimate: 120,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::LlmCallStarting {
                agent_name,
                call_index,
            } => {
                assert_eq!(agent_name, "coder");
                // The agtrs signal carries no call index, so the bridge
                // hard-codes 0 rather than inventing a count.
                assert_eq!(call_index, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_model_call_complete_with_usage_and_cost() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(ModelCallComplete {
            model: "claude-sonnet".into(),
            agent_id: None,
            agent_name: "planner".into(),
            usage: usage(100, 25),
            duration_ms: 812.5,
            stop_reason: agtrs_runtime::transport::StopReason::EndTurn,
            cost_usd: 0.0042,
            total_tokens: 125,
            turns: 1,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::LlmCallComplete {
                agent_name,
                input_tokens,
                output_tokens,
                cost_usd,
                duration_ms,
            } => {
                assert_eq!(agent_name, "planner");
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 25);
                assert!((cost_usd - 0.0042).abs() < f64::EPSILON);
                assert!((duration_ms - 812.5).abs() < f64::EPSILON);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_agent_run_complete_with_totals() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(AgentRunComplete {
            agent_id: "agent-9".into(),
            agent_name: "coder".into(),
            turns: 6,
            total_usage: usage(900, 400),
            total_cost_usd: 0.37,
            stop_reason: agtrs_runtime::transport::StopReason::EndTurn,
            tool_metrics: None,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::AgentRunComplete {
                agent_name,
                turns,
                total_cost_usd,
            } => {
                assert_eq!(agent_name, "coder");
                assert_eq!(turns, 6);
                assert!((total_cost_usd - 0.37).abs() < f64::EPSILON);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_agent_cancelled_with_its_reason() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(AgentCancelled {
            agent_id: "agent-2".into(),
            agent_name: "qa".into(),
            reason: "deadline exceeded".into(),
            turns_completed: 3,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::AgentCancelled { agent_name, reason } => {
                assert_eq!(agent_name, "qa");
                assert_eq!(reason, "deadline exceeded");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_file_edits_committed_with_per_file_diffs() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        let mut diffs = std::collections::HashMap::new();
        diffs.insert("src/lib.rs".to_string(), "+1\n-0".to_string());

        bus.emit(AgtrsFileEditsCommitted {
            files: vec!["src/lib.rs".into()],
            total_lines_added: 1,
            total_lines_removed: 0,
            diffs,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::FileEditsCommitted {
                files,
                lines_added,
                lines_removed,
                diffs,
            } => {
                assert_eq!(files, vec!["src/lib.rs".to_string()]);
                assert_eq!(lines_added, 1);
                assert_eq!(lines_removed, 0);
                assert_eq!(diffs.get("src/lib.rs").map(String::as_str), Some("+1\n-0"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_the_xaft_llm_call_index() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftLlmCallStarting {
            agent_name: "coder".into(),
            call_index: 3,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::LlmCallStarting {
                agent_name,
                call_index,
            } => {
                assert_eq!(agent_name, "coder");
                // Unlike the agtrs signal, this one carries a real index.
                assert_eq!(call_index, 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_commit_created() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftCommitCreated {
            agent_name: "coder".into(),
            short_sha: "deadbeef".into(),
            message: "fix: bound the retry loop".into(),
            files_changed: 2,
            lines_added: 30,
            lines_removed: 12,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::CommitCreated {
                agent_name,
                short_sha,
                message,
                files_changed,
            } => {
                assert_eq!(agent_name, "coder");
                assert_eq!(short_sha, "deadbeef");
                assert_eq!(message, "fix: bound the retry loop");
                assert_eq!(files_changed, 2);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_agent_handoff() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftAgentHandoff {
            from_agent: "planner".into(),
            to_agent: "coder".into(),
            summary: "plan approved, start implementing".into(),
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::AgentHandoff {
                from_agent,
                to_agent,
                summary,
            } => {
                assert_eq!(from_agent, "planner");
                assert_eq!(to_agent, "coder");
                assert_eq!(summary, "plan approved, start implementing");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_each_stream_token() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(xaft_agent::XaftStreamToken {
            agent_name: "coder".into(),
            token: "Hel".into(),
        })
        .await;
        bus.emit(xaft_agent::XaftStreamToken {
            agent_name: "coder".into(),
            token: "lo".into(),
        })
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Tokens must arrive one-per-event, not coalesced, or the live typing
        // indicator has nothing to animate.
        for want in ["Hel", "lo"] {
            match rx.try_recv().expect("expected a stream token") {
                TuiEvent::StreamToken { agent_name, token } => {
                    assert_eq!(agent_name, "coder");
                    assert_eq!(token, want);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn bridge_forwards_background_pipeline_completion() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftBackgroundPipelineComplete {
            id: 42,
            task_summary: "migrate the config loader".into(),
            success: false,
            duration_ms: 90.0,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::BackgroundPipelineComplete {
                id,
                task_summary,
                success,
            } => {
                assert_eq!(id, 42);
                assert_eq!(task_summary, "migrate the config loader");
                assert!(
                    !success,
                    "a failed pipeline must not be reported as successful"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_context_compacted_marker() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftContextCompacted {
            agent_name: "coder".into(),
            messages_removed: 18,
            chars_removed: 4000,
            summary_chars: 400,
            tokens_saved_estimate: 1000,
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::ContextCompacted {
                agent_name,
                messages_removed,
                tokens_saved,
            } => {
                assert_eq!(agent_name, "coder");
                assert_eq!(messages_removed, 18);
                // The event carries the derived estimate, not the raw chars.
                assert_eq!(tokens_saved, 1000);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_specialist_agent_lifecycle() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(xaft_agent::XaftMetaAgentSpawned {
            blueprint_name: "reviewer".into(),
            blueprint_role: "code review".into(),
            task_preview: "review the retry loop".into(),
            spawn_mode: "parallel".into(),
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::SpecialistAgentStarted {
                agent_name,
                role,
                task_preview,
                spawn_mode,
            } => {
                assert_eq!(agent_name, "reviewer");
                assert_eq!(role, "code review");
                assert_eq!(task_preview, "review the retry loop");
                assert_eq!(spawn_mode, "parallel");
            }
            other => panic!("unexpected: {other:?}"),
        }

        bus.emit(xaft_agent::XaftMetaAgentCompleted {
            agent_name: "reviewer".into(),
            success: true,
            duration_ms: 1234.0,
            output_preview: "looks good".into(),
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::SpecialistAgentCompleted {
                agent_name,
                success,
                duration_ms,
                output_preview,
            } => {
                assert_eq!(agent_name, "reviewer");
                assert!(success);
                assert!((duration_ms - 1234.0).abs() < f64::EPSILON);
                assert_eq!(output_preview, "looks good");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_forwards_dynamic_command_registration() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(XaftCommandRegistered {
            name: "deploy".into(),
            source: "skill:release".into(),
            description: "ship the current branch".into(),
            args_hint: Some("[env]".into()),
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::CommandRegistered {
                name,
                source,
                description,
                args_hint,
            } => {
                assert_eq!(name, "deploy");
                assert_eq!(source, "skill:release");
                assert_eq!(description, "ship the current branch");
                assert_eq!(args_hint.as_deref(), Some("[env]"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[cfg(feature = "memory")]
    #[tokio::test]
    async fn bridge_forwards_memory_stored_and_recalled() {
        let (bus, bridge, mut rx) = make_bridge_and_bus();
        bridge.attach(&bus).await;

        bus.emit(xaft_memory::signals::XaftMemoryStored {
            content_summary: "the gate is ceil, not round".into(),
            tags: vec!["coverage".into(), "arithmetic".into()],
            scope: "project".into(),
            agent_name: "coder".into(),
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::MemoryStored {
                content_summary,
                tags,
                agent_name,
            } => {
                assert_eq!(content_summary, "the gate is ceil, not round");
                assert_eq!(tags, vec!["coverage".to_string(), "arithmetic".to_string()]);
                assert_eq!(agent_name, "coder");
            }
            other => panic!("unexpected: {other:?}"),
        }

        bus.emit(xaft_memory::signals::XaftMemoryRecalled {
            query: "coverage gate".into(),
            results_count: 2,
            top_result_preview: Some("the gate is ceil".into()),
        })
        .await;

        match next_event(&mut rx).await {
            TuiEvent::MemoryRecalled {
                query,
                results_count,
            } => {
                assert_eq!(query, "coverage gate");
                assert_eq!(results_count, 2);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_send_helper_reaches_the_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bridge = EventBridge::new(tx);

        bridge.send(TuiEvent::Resize(80, 24));

        match rx.try_recv().expect("send must reach the channel") {
            TuiEvent::Resize(cols, rows) => assert_eq!((cols, rows), (80, 24)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_dropped_receiver_does_not_panic_the_bridge() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let bridge = EventBridge::new(tx);

        // `send` deliberately swallows the closed-channel error: the TUI
        // shutting down mid-broadcast is normal, not an error worth panicking
        // over inside a signal handler.
        bridge.send(TuiEvent::Resize(80, 24));
    }
}
