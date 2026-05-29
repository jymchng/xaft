//! `xaft-memory` — product-level memory system for xaft coding agents.
//!
//! Builds on top of [`agtrs_memory`] to provide:
//!
//! - coding-agent workflows
//! - project-scoped architectural memory
//! - git-aware memory behavior (branch, commit, worktree)
//! - memory tools for agent toolchains (remember, recall, forget, summarize)
//! - runtime bootstrapping and lifecycle integration
//! - TUI memory event streaming
//! - auto-extraction of knowledge from agent turns
//!
//! # Quick start
//!
//! ```rust,no_run
//! use xaft_memory::{XaftMemoryManager, MemoryConfig};
//! use std::sync::Arc;
//!
//! # #[tokio::main] async fn main() {
//! let config = MemoryConfig::default();
//! let manager = XaftMemoryManager::in_memory(config).await.unwrap();
//! manager.remember("The auth service uses JWT tokens", &["architecture"]).await.unwrap();
//! let results = manager.recall("auth service").await.unwrap();
//! assert!(!results.is_empty());
//! # }
//! ```

pub mod config;
pub mod error;
pub mod manager;
pub mod signals;
pub mod tools;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use config::MemoryConfig;
pub use error::{MemoryError, MemoryResult};
pub use manager::XaftMemoryManager;
pub use signals::{XaftMemoryRecalled, XaftMemoryStored};

pub use tools::{ForgetTool, MemoryToolset, RecallTool, RememberTool, SummarizeMemoryTool};
