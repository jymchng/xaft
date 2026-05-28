//! Agent registry for dynamic multi-agent handoff workflows.
//!
//! This module re-exports the registry infrastructure from [`xaft_agents`].
//! Domain agent definitions (planner, coder, QA, fixer, summarizer) live in
//! `xaft-agents`; `xaft-runtime` re-exports them for backwards compatibility.

// ── Re-exports from xaft-agents ───────────────────────────────────────────────

pub use xaft_agents::handoff::HandoffTool;
pub use xaft_agents::registry::{AgentDefinition, AgentRegistry, AgentToolSet, WorkflowConfig};
