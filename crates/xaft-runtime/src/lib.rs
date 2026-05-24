//! `xaft-runtime` — Runtime orchestration for the xaft coding agent.
//!
//! This crate will house `XaftRuntime`, the top-level orchestrator that composes
//! all agtrs primitives (AgentExecutor, SignalBus, WorkspaceStore, GitRepo, etc.)
//! into a cohesive autonomous coding system.
//!
//! ## Current status
//!
//! This is a stub implementation. The full runtime will be implemented in the
//! `xaft-orchestrator` phase after tools and agents are available.
//!
//! ## Architecture
//!
//! ```text
//! xaft-cli
//!     └── XaftRuntime::bootstrap(cli_args, config)
//!              ├── Provider chain (CostedProvider → FallbackProvider)
//!              ├── WorkspaceStore (transactional file editing)
//!              ├── GitRepo + WorktreeGuard (branch isolation)
//!              ├── SignalBus (event routing)
//!              ├── Tool registry (ReadFile, WriteFile, BashExec, ...)
//!              ├── Agent (XaftAgent or PlanModeAgent)
//!              └── AgentExecutor::run_stream() → StreamEvent
//! ```

#![deny(missing_docs)]

pub mod dispatch;
pub mod error;
pub mod session;
pub mod types;

pub use dispatch::{RuntimeDispatch, RunRequest, RunResult};
pub use error::RuntimeError;
pub use session::{AgentSession, SessionId, SessionStatus};
pub use types::ExitCode;
