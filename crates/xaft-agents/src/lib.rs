//! `xaft-agents` — domain agent definitions for the xaft coding runtime.
//!
//! This crate owns the named agent implementations (planner, coder, QA, fixer,
//! summarizer) and the [`AgentRegistry`] / [`HandoffTool`] infrastructure.
//! It keeps domain content out of `xaft-runtime`, which becomes pure
//! orchestration plumbing.
//!
//! # Architecture
//!
//! ```text
//! xaft-agent    ← low-level: XaftAgent, PlanModeAgent, AgentBuilder, StreamSink
//! xaft-agents   ← domain:    planner, coder, qa, fixer, summarizer, AgentRegistry
//! xaft-runtime  ← orchestration: imports from both, drives the HandoffOrchestrator
//! ```
//!
//! # Quick start
//!
//! ```rust,ignore
//! use xaft_agents::registry::{AgentRegistry, AgentDefinition, AgentToolSet};
//! use xaft_agents::coder::EditSummary;
//!
//! // Pre-built four-agent registry
//! let registry = AgentRegistry::default_xaft();
//! assert_eq!(registry.len(), 5); // planner, coder, qa, fixer, summarizer
//!
//! // Build one of those agents (inject handoff tool automatically)
//! let agent = registry.build_agent(
//!     "coder", "my task", "/workspace",
//!     &read_tools, &write_tools,
//!     handoff_store, signals,
//! ).unwrap();
//! ```

#![deny(missing_docs)]

pub mod coder;
pub mod error;
pub mod fixer;
pub mod handoff;
pub mod named;
pub mod planner;
pub mod qa;
pub mod registry;
pub mod summarizer;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use coder::EditSummary;
pub use error::AgentError;
pub use fixer::FixSummary;
pub use handoff::HandoffTool;
pub use named::NamedAgent;
pub use qa::{QaVerdict, RequestFixTool};
pub use registry::{AgentDefinition, AgentRegistry, AgentToolSet};
