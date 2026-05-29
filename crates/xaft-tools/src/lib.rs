//! `xaft-tools` — built-in tool implementations for xaft agents.
//!
//! Thin wrappers over [`agtrs_workspace`], [`agtrs_git`], and [`agtrs_shell`]
//! that expose a consistent [`agtrs_runtime::tool::Tool`] interface.
//!
//! # Structure
//!
//! ```text
//! xaft-tools
//! ├── fs/          — ReadFileTool, WriteFileTool, EditFileTool, ListFilesTool, GrepTool
//! ├── fs_store     — FsWorkspaceStore (filesystem-backed WorkspaceStore)
//! ├── git/         — GitStatusTool, GitDiffTool, GitLogTool
//! ├── shell/       — BashExecTool
//! ├── registry     — ToolRegistry + ToolRegistryBuilder
//! └── error        — ToolError, input helpers
//! ```
//!
//! # Quick start
//!
//! ```rust,no_run
//! use xaft_tools::registry::ToolRegistryBuilder;
//!
//! let reg = ToolRegistryBuilder::new(".")
//!     .with_shell()
//!     .build_coder()
//!     .unwrap();
//!
//! // Register on an agent context:
//! // for tool in reg.all() { ctx.register_tool(tool.name().into(), tool); }
//! ```

#![deny(missing_docs)]

pub mod error;
pub mod fs;
pub mod fs_store;
pub mod git;
pub mod registry;
pub mod shell;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use error::ToolError;
pub use fs::{
    EditFileTool, GrepTool, ListFilesTool, ReadBeforeEditHook, ReadFileTool, WriteFileTool,
};
pub use fs_store::FsWorkspaceStore;
pub use git::{GitDiffTool, GitLogTool, GitStatusTool};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use shell::BashExecTool;
