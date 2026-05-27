//! `xaft-agent` — production-grade coding agents for xaft.
//!
//! Provides two agent implementations wrapping the agtrs [`Agent`] trait:
//!
//! - [`XaftAgent`] — a role-aware, streaming-capable coding agent with git
//!   auto-commit support.
//! - [`PlanModeAgent`] — extends `XaftAgent` with a two-tier planning cascade
//!   (OneShotPlanner → IterativeRefinementPlanner) that decomposes tasks into
//!   ordered execution plans before handing off to the agtrs executor.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use xaft_agent::builder::{AgentBuilder, PlanAgentBuilder};
//! use xaft_agent::config::AgentRole;
//!
//! // Simple coder agent
//! let agent = AgentBuilder::new("coder")
//!     .role(AgentRole::Coder)
//!     .max_turns(20)
//!     .build();
//!
//! // Plan-mode agent
//! let plan_agent = PlanAgentBuilder::new("plan-coder")
//!     .role(AgentRole::Coder)
//!     .max_turns(30)
//!     .build();
//! ```
//!
//! # Architecture
//!
//! ```text
//! xaft-agent
//! ├── XaftAgent               (Agent trait impl)
//! │   ├── on_start            inject context state
//! │   ├── before_llm_call     emit XaftLlmCallStarting signal
//! │   ├── on_tool_result      forward StreamEvent::ToolResult to sink
//! │   ├── on_turn_complete    log turn metrics
//! │   └── on_finish           emit Done, git auto-commit
//! │
//! ├── PlanModeAgent           (wraps XaftAgent, overrides run())
//! │   └── run()
//! │       ├── OneShotPlanner::plan()
//! │       ├── → escalate to IterativeRefinementPlanner if policy says so
//! │       ├── inject plan context message
//! │       └── AgentExecutor::run()
//! │
//! ├── AgentBuilder            fluent XaftAgent builder
//! ├── PlanAgentBuilder        fluent PlanModeAgent builder
//! └── stream::StreamSink      sink trait + ChannelSink + CollectSink
//! ```

#![deny(missing_docs)]

pub mod agent;
pub mod builder;
pub mod config;
pub mod error;
pub mod plan_mode;
pub mod prompts;
pub mod signals;
pub mod stream;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use agent::XaftAgent;
pub use builder::{AgentBuilder, PlanAgentBuilder};
pub use config::{AgentRole, CommitPolicy, EscalationPolicy, PlanModeConfig, XaftAgentConfig};
pub use error::AgentError;
pub use plan_mode::PlanModeAgent;
pub use prompts::{build_system_prompt, default_prompt_for};
pub use signals::{
    XaftAgentHandoff, XaftAgentOutput, XaftCommitCreated, XaftLlmCallStarting, XaftPlanCreated,
    XaftPlanEmpty,
};
pub use stream::{ChannelSink, CollectSink, NopSink, StreamSink};
