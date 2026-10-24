//! Concurrent typed subagents for every interactive turn (agenthicc's
//! `subagents/`, PRD-124).
//!
//! agenthicc's agent-turn runner injects `spawn_subagents` — plus the three
//! parent communication tools — into every turn, so any agent in any
//! workflow can fan out to isolated workers of nine typed roles:
//!
//! ```text
//! parent turn ── spawn_subagents({tasks: [{type, task, context?}…]})
//!                  │  resume cache (journal) ── hit → cached aggregate
//!                  ▼
//!               SubagentPool  (≤ max_concurrent at once, per-worker timeout)
//!                  ├── explorer #1 ─┐   role prompt + inherited policy,
//!                  ├── tester #1  ──┤   role ∩ parent-visible tools,
//!                  └── …          ──┘   parent approval gate, private memory
//!                  ▼
//!               labelled aggregate ── journal ── tool result
//! ```
//!
//! [`SubagentServices::session_tools`] builds the four parent-side tools for
//! one pool of parent tools; `runtime.rs` gives them to the standard,
//! dynamic and meta pipelines' agents and the phased engine to every phase.
//! Their capabilities (`xaft_tools::capabilities`): `spawn_subagents` is
//! undeclared, as in agenthicc — Plan never offers it and Safe asks before
//! each spawn — and the communication tools are `control`.
//!
//! The spawn machinery reused from the `meta` workflow: workers are
//! `NamedAgent`s driven by the agtrs executor, and each worker's start and
//! end are the `XaftMetaAgentSpawned` / `XaftMetaAgentCompleted` signals the
//! TUI already renders as `↳ [spawn] explorer #1 (explorer): …` and
//! `✓ [spawn done] explorer #1 · 1.2s`.

pub mod communication;
pub mod journal;
pub mod policy;
pub mod pool;
pub mod tool;
pub mod types;

use std::path::PathBuf;
use std::sync::Arc;

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::ErasedTool;

pub use communication::{
    AgentMessageBroker, AgentMessageEnvelope, CHILD_TOOL_NAMES, CommunicationError,
    PARENT_TOOL_NAMES, PoolContinuationRegistry, PoolHandle,
};
pub use journal::SubagentJournal;
pub use policy::SubagentPolicy;
pub use pool::{AggregatedResult, SubagentResult, SubagentTask, aggregate};
pub use tool::{SPAWN_SUBAGENTS_TOOL_NAME, SpawnSubagentsTool};
pub use types::{DEFAULT_SUBAGENT_TIMEOUT_S, SubagentTypeRegistry, SubagentTypeSpec};

/// agenthicc's default `max_concurrent`.
pub const DEFAULT_MAX_CONCURRENT: usize = 4;

pub(crate) fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What every pool of one run shares: the parent's model, signal bus and
/// approval gate, the session's journal and continuations.
#[derive(Clone)]
pub struct SubagentServices {
    /// The run's provider; workers use the parent's model.
    pub llm: Arc<dyn LlmProvider>,
    /// The run's signal bus — cost accounting, tool events and the TUI's
    /// approval dialog all hang off it.
    pub signals: Arc<SignalBus>,
    /// DI context for tools.
    pub resolve_ctx: Arc<injectable_rs_runtime::ResolveContext>,
    /// The parent's approval gate (`XaftRuntime::effective_approval_gate`):
    /// a worker's approval-gated call asks exactly where the parent's would.
    pub approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// The session (agenthicc's `conversation_id`).
    pub session_id: String,
    /// The run that spawned the pools.
    pub parent_run_id: String,
    /// The workspace.
    pub working_dir: PathBuf,
    /// The session's durable journal.
    pub journal: Option<Arc<SubagentJournal>>,
    /// The session's pools awaiting answers.
    pub continuations: Arc<PoolContinuationRegistry>,
    /// The roles.
    pub registry: Arc<SubagentTypeRegistry>,
    /// Default `max_concurrent`.
    pub max_concurrent: usize,
}

impl std::fmt::Debug for SubagentServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentServices")
            .field("session_id", &self.session_id)
            .field("working_dir", &self.working_dir)
            .finish_non_exhaustive()
    }
}

impl SubagentServices {
    /// The parent-side tools for a turn whose visible tools are `pool`:
    /// `spawn_subagents` (drawing its workers' tools from `pool`) and
    /// `answer_subagent`, `send_subagent_message`, `collect_subagent_results`.
    ///
    /// `requires_approval` is the turn's approval filter (Safe): when it
    /// selects `spawn_subagents`, the spawn itself must be approved, as
    /// agenthicc's capability gate asks for an undeclared tool.
    pub fn session_tools(
        &self,
        pool: Vec<Arc<ErasedTool>>,
        policy: SubagentPolicy,
        requires_approval: Option<&crate::dispatch::ModeToolFilter>,
    ) -> Vec<Arc<ErasedTool>> {
        let spawn: Arc<ErasedTool> = Arc::new(SpawnSubagentsTool::new(self.clone(), pool, policy));
        let mut tools = vec![spawn];
        tools.extend(communication::parent_tools(
            &self.continuations,
            &self.session_id,
        ));
        match requires_approval {
            Some(filter) => crate::mode_gate::require_approval(tools, filter),
            None => tools,
        }
    }
}

/// Whether `name` is one of the session-bound subagent tools — the four a
/// parent turn gets or the six a worker gets. Fan-outs that are not
/// `spawn_subagents` (`explore_repository`'s file explorers) leave them out,
/// so no worker of any kind spawns workers of its own.
pub fn is_session_tool(name: &str) -> bool {
    name == SPAWN_SUBAGENTS_TOOL_NAME
        || PARENT_TOOL_NAMES.contains(&name)
        || CHILD_TOOL_NAMES.contains(&name)
}

/// The policy a turn's pools inherit, from the turn's own tools.
pub fn policy_for(
    pool: &[Arc<ErasedTool>],
    read_only: bool,
    approval_gated: bool,
    system_prompt_suffix: Option<&str>,
    working_dir: &std::path::Path,
) -> SubagentPolicy {
    SubagentPolicy {
        read_only,
        approval_gated,
        system_prompt_suffix: system_prompt_suffix.unwrap_or_default().to_string(),
        visible_tools: pool.iter().map(|t| t.name().to_string()).collect(),
        approval_tools: pool
            .iter()
            .filter(|t| t.requires_confirmation())
            .map(|t| t.name().to_string())
            .collect(),
        workspace: working_dir.display().to_string(),
    }
}
