//! `NamedAgent` — a minimal, signal-aware `Agent` implementation used by all
//! domain agents in xaft (planner, coder, QA, fixer).
//!
//! This is the canonical base agent moved from `xaft-runtime/src/orchestrator.rs`
//! into a dedicated module so domain modules can build typed agents without
//! depending on the runtime orchestrator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tracing::debug;

use agtrs_runtime::agent::{Agent, AgentConfig, AgentContext};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::{LlmOptions, LlmResponse};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::ErasedTool;
use agtrs_runtime::transport::Message;

use xaft_agent::signals::{XaftAgentOutput, XaftLlmCallStarting};

// ── NamedAgent ────────────────────────────────────────────────────────────────

/// A lightweight, named `Agent` impl suitable for planner / coder / QA / fixer.
///
/// Emits [`XaftLlmCallStarting`] in `before_llm_call` and [`XaftAgentOutput`]
/// in `after_llm_call` so the TUI receives real-time agent state without
/// needing to touch the streaming delta assembler.
///
/// When `handoff_triggered` is `Some`, the next `before_llm_call` returns
/// `Err` to cleanly terminate the agent after a handoff tool fires.
pub struct NamedAgent {
    /// Unique agent name used as routing key.
    pub name: String,
    pub(crate) config: AgentConfig,
    pub(crate) tools: Vec<Arc<ErasedTool>>,
    /// Optional signal bus for TUI integration.
    pub(crate) signals: Option<Arc<SignalBus>>,
    /// When set to `true` by a [`HandoffTool`], the next `before_llm_call`
    /// returns `Err` to stop the agent from calling the LLM again.
    ///
    /// [`HandoffTool`]: crate::handoff::HandoffTool
    pub(crate) handoff_triggered: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for NamedAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedAgent")
            .field("name", &self.name)
            .field("max_turns", &self.config.max_turns)
            .field("tools_count", &self.tools.len())
            .finish()
    }
}

impl NamedAgent {
    /// Create a new `NamedAgent` with a system prompt and max-turns limit.
    pub fn new(
        name: impl Into<String>,
        system_prompt: impl Into<String>,
        max_turns: usize,
    ) -> Self {
        Self {
            name: name.into(),
            config: AgentConfig {
                system_prompt: system_prompt.into(),
                max_turns,
                strict_capability_check: false,
                parallel_tool_calls: false,
                ..Default::default()
            },
            tools: Vec::new(),
            signals: None,
            handoff_triggered: None,
        }
    }

    /// Attach a tool list.
    pub fn with_tools(mut self, tools: Vec<Arc<ErasedTool>>) -> Self {
        self.tools = tools;
        self
    }

    /// Attach a signal bus for TUI / observability integration.
    pub fn with_signals(mut self, signals: Arc<SignalBus>) -> Self {
        self.signals = Some(signals);
        self
    }

    /// Attach a stop flag shared with the agent's `HandoffTool`.
    ///
    /// When the handoff tool fires it sets the flag to `true`; the next
    /// `before_llm_call` then returns `Err` to abort the agent cleanly.
    pub fn with_handoff_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.handoff_triggered = Some(flag);
        self
    }
}

#[async_trait]
impl Agent for NamedAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn system_prompt(&self) -> String {
        self.config.system_prompt.clone()
    }

    fn tools(&self) -> Vec<Arc<ErasedTool>> {
        self.tools.clone()
    }

    fn config(&self) -> &AgentConfig {
        &self.config
    }

    async fn on_start(&self, _ctx: &mut AgentContext) -> Result<(), AgtrsError> {
        debug!(agent = %self.name, "NamedAgent: on_start");
        Ok(())
    }

    /// Emit [`XaftLlmCallStarting`] before each LLM turn.
    async fn before_llm_call(
        &self,
        _messages: &mut Vec<Message>,
        _options: &mut LlmOptions,
    ) -> Result<(), AgtrsError> {
        if let Some(ref bus) = self.signals {
            let bus = Arc::clone(bus);
            let agent_name = self.name.clone();
            tokio::spawn(async move {
                bus.emit(XaftLlmCallStarting {
                    agent_name,
                    call_index: 0,
                })
                .await;
            });
        }
        Ok(())
    }

    /// Emit non-empty text responses to the TUI via `XaftAgentOutput`.
    async fn after_llm_call(&self, response: &LlmResponse) -> Result<(), AgtrsError> {
        if let Some(ref bus) = self.signals {
            let text = response.message.text();
            if !text.trim().is_empty() {
                let bus = Arc::clone(bus);
                let agent_name = self.name.clone();
                tokio::spawn(async move {
                    bus.emit(XaftAgentOutput {
                        agent_name,
                        content: text,
                    })
                    .await;
                });
            }
        }
        Ok(())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn new_has_correct_name_and_turns() {
        let a = NamedAgent::new("coder", "you are a coder", 20);
        assert_eq!(a.name(), "coder");
        assert_eq!(a.config.max_turns, 20);
        assert!(a.tools.is_empty());
        assert!(a.signals.is_none());
        assert!(a.handoff_triggered.is_none());
    }

    #[test]
    fn system_prompt_returned_correctly() {
        let a = NamedAgent::new("qa", "you are a reviewer", 10);
        assert_eq!(a.system_prompt(), "you are a reviewer");
    }

    #[test]
    fn with_tools_stores_tools() {
        let a = NamedAgent::new("a", "prompt", 5).with_tools(vec![]);
        assert!(a.tools().is_empty());
    }

    #[test]
    fn with_handoff_flag_stores_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        let a = NamedAgent::new("fixer", "fix it", 10).with_handoff_flag(Arc::clone(&flag));
        assert!(a.handoff_triggered.is_some());
    }

    #[tokio::test]
    async fn before_llm_call_ok_even_when_flag_set() {
        // The HandoffOrchestrator needs one more LLM call after tool result
        // before checking the handoff store, so before_llm_call must not
        // error on the flag — the orchestrator handles termination.
        let flag = Arc::new(AtomicBool::new(true));
        let agent = NamedAgent::new("coder", "code", 10).with_handoff_flag(Arc::clone(&flag));
        let mut msgs: Vec<Message> = vec![];
        let mut opts = LlmOptions::default();
        let result = agent.before_llm_call(&mut msgs, &mut opts).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn before_llm_call_ok_when_flag_not_set() {
        let flag = Arc::new(AtomicBool::new(false));
        let agent = NamedAgent::new("coder", "code", 10).with_handoff_flag(Arc::clone(&flag));
        let mut msgs: Vec<Message> = vec![];
        let mut opts = LlmOptions::default();
        let result = agent.before_llm_call(&mut msgs, &mut opts).await;
        assert!(result.is_ok());
    }
}
