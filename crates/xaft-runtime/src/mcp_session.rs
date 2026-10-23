//! The MCP servers a run uses (agenthicc's session-start registry, PRD-172).
//!
//! agenthicc builds one `McpSessionManager` when a session starts, registers
//! every configured server, starts the `auto_connect` ones — in the
//! background for the TUI, before the first turn for a headless session — and
//! shares that one manager with every turn, workflow phase and subagent pool.
//!
//! xaft has the same two shapes:
//!
//! * **A session that outlives a run** (the TUI) builds the manager itself,
//!   starts it in the background and injects it with
//!   [`XaftRuntime::with_mcp_manager`](crate::runtime::XaftRuntime::with_mcp_manager).
//!   Every run then takes whatever the manager has published *now*; a server
//!   still starting simply contributes no tools to that turn, as agenthicc's
//!   optional servers do.
//! * **A run with no injected manager** (`xaft run`, the stdin protocol, a
//!   background job) is a one-run session: it builds a manager from the run's
//!   configuration, awaits `start_all` before the tools are assembled —
//!   agenthicc's headless path — and shuts it down when the run ends.
//!
//! Either way the tools are `mcp__<server>__<tool>`, capability `network`
//! (`xaft_tools::capabilities`), so the run's mode filter, Safe's approval
//! wrapper and a phase's capability allowlist treat them like any other
//! outbound tool, and the server instructions reach every system prompt as
//! agenthicc's provenance-labelled block.

use std::path::Path;
use std::sync::Arc;

use agtrs_runtime::tool::ErasedTool;
use agtrs_runtime::transport::Message;
use xaft_config::XaftConfig;
use xaft_mcp::{McpSessionManager, NetworkGuard, entries_from_config};

/// The MCP manager one run uses.
pub(crate) struct RunMcp {
    manager: Arc<McpSessionManager>,
    /// Built for this run alone, so this run shuts it down.
    owned: bool,
}

impl RunMcp {
    /// The injected session manager, or — when the run's configuration
    /// declares servers — a manager for this run alone, already started.
    pub(crate) async fn resolve(
        injected: Option<&Arc<McpSessionManager>>,
        config: &XaftConfig,
        working_dir: &Path,
    ) -> Option<Self> {
        if let Some(manager) = injected {
            return Some(Self {
                manager: Arc::clone(manager),
                owned: false,
            });
        }
        let entries = entries_from_config(config);
        if entries.is_empty() {
            return None;
        }
        let manager = McpSessionManager::new(
            entries,
            working_dir,
            Some(NetworkGuard::from_config(config)),
        );
        manager.start_all().await;
        Some(Self {
            manager,
            owned: true,
        })
    }

    /// The published tools, as agent tools.
    pub(crate) fn tools(&self) -> Vec<Arc<ErasedTool>> {
        self.manager.tools()
    }

    /// The servers' instructions as one system message (agenthicc's
    /// `prompt_instructions()`), or `None` when no ready server sent any.
    pub(crate) fn instructions(&self) -> Option<Message> {
        let block = self.manager.prompt_instructions();
        (!block.trim().is_empty()).then(|| Message::system(block))
    }

    /// End the run: a manager built for it is shut down, an injected one is
    /// left to its session.
    pub(crate) async fn finish(self) {
        if self.owned {
            self.manager.shutdown().await;
        }
    }
}
