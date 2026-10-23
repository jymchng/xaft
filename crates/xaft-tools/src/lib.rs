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
//! ├── shell/       — BashExecTool, RunCommandTool
//! ├── mcp          — McpConnectionGuideTool
//! ├── plugin_trust — plugin trust decisions and the load audit journal
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

pub mod capabilities;
pub mod dynamic;
pub mod error;
pub mod fs;
pub mod fs_store;
pub mod git;
pub mod mcp;
pub mod plugin_trust;
pub mod registry;
pub mod shell;
pub mod terminals;

// ── Flat re-exports ──────────────────────────────────────────────────────

pub use dynamic::{DynamicToolFactory, ScriptedTool};
pub use error::ToolError;
pub use fs::{
    // new write FS tools (PRD 53)
    AppendToFileTool,
    // agenthicc fs-parity tools (section 6 of the parity matrix)
    BatchCopyTool,
    BatchDeleteTool,
    BatchMoveTool,
    BatchReadTool,
    BatchWriteTool,
    ChecksumFileTool,
    CopyFileTool,
    CreateDirectoryTool,
    DeleteFileTool,
    // new read-only FS tools (PRD 53)
    DiffFilesTool,
    // original tools
    EditFileTool,
    FileExistsTool,
    FileStatTool,
    FileStatToolFs,
    GlobTool,
    GlobToolFs,
    GrepTool,
    ListFilesTool,
    MoveFileTool,
    PatchFileTool,
    ReadBeforeEditHook,
    ReadFileTool,
    ReadLinesTool,
    RemoveDirectoryTool,
    SearchFilesTool,
    TouchFileTool,
    TreeTool,
    TreeToolFs,
    TruncateFileTool,
    WriteFileTool,
};
pub use fs_store::FsWorkspaceStore;
pub use git::{
    // new write git tools (PRD 54)
    GitAddTool,
    // new read-only git tools (PRD 54)
    GitBlameTool,
    GitBranchTool,
    GitCheckoutFilesTool,
    GitCommitStagedTool,
    GitCreateBranchTool,
    // original tools
    GitDiffTool,
    GitGrepTool,
    GitLogTool,
    GitMergeTool,
    GitPushTool,
    GitRemoteTool,
    GitShowTool,
    GitStashListTool,
    GitStashListTool as _,
    GitStashPopTool,
    GitStashTool,
    GitStatusTool,
    GitTagTool,
    GitUnstageTool,
};
pub use mcp::McpConnectionGuideTool;
pub use plugin_trust::{
    AuditRecord, InstallPlan, PluginTrust, TrustDecision, TrustManifest, TrustRecord, sha256_hex,
};
pub use registry::{ToolRegistry, ToolRegistryBuilder};
pub use shell::{BashExecTool, RunCommandTool};
pub use terminals::{
    InspectTerminalTool, StopTerminalTool, TerminalManager, TerminalManagerConfig, TerminalRecord,
    TerminalState, TerminalStore, WaitTerminalReadyTool, WaitTerminalTool,
};
