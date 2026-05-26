//! `xaft-runtime` — Runtime orchestration for the xaft coding agent.
//!
//! Composes all agtrs primitives into a cohesive autonomous coding system.
//!
//! # Architecture
//!
//! ```text
//! xaft-cli
//!     └── XaftRuntime::bootstrap(config)
//!              ├── ProviderFactory → CostedProvider(FallbackProvider(Anthropic/OpenAI))
//!              ├── FsWorkspaceStore (transactional file editing)
//!              ├── GitRepo + WorktreeGuard (branch isolation)
//!              ├── SignalBus (event routing)
//!              ├── ToolRegistry (ReadFile, WriteFile, BashExec, GitStatus, …)
//!              ├── XaftAgent (role-aware, streaming, auto-commit)
//!              └── AgentExecutor::run_stream() → EventLoop → RunResult
//! ```
//!
//! # Quick start (headless, no TUI)
//!
//! ```rust,no_run
//! use xaft_runtime::runtime::XaftRuntime;
//! use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
//! use xaft_config::XaftConfig;
//! use std::path::PathBuf;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = XaftConfig::default();
//! let runtime = XaftRuntime::bootstrap(config.clone()).await?;
//!
//! let result = runtime.run(RunRequest {
//!     task: "Add docstrings to all public functions in src/".into(),
//!     config,
//!     working_dir: PathBuf::from("."),
//!     headless: true,
//!     dry_run: false,
//!     auto_approve: false,
//!     dangerously_skip_permissions: false,
//!     resume_session_id: None,
//! }).await?;
//!
//! println!("Exit code: {}", result.exit_code);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

pub mod dispatch;
pub mod error;
pub mod event_loop;
pub mod orchestrator;
pub mod provider;
pub mod runtime;
pub mod session;
pub mod session_store;
pub mod types;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use dispatch::{RunRequest, RunResult, RuntimeDispatch, StubRuntime};
pub use error::RuntimeError;
pub use event_loop::EventLoop;
pub use provider::ProviderFactory;
pub use runtime::XaftRuntime;
pub use session::{AgentSession, SessionId, SessionStatus};
pub use session_store::{FsSessionStore, InMemorySessionStore, SessionStore};
pub use types::ExitCode;
