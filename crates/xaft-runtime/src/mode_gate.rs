//! Mode-level approval gate — the runtime half of agenthicc's Safe mode
//! (parity §3 row 1).
//!
//! agenthicc's `Safe` is *gate-and-ask*: read, search and git-read tools run
//! directly, and every tool that can write, execute, change git state, reach
//! the network — or that declares no capability at all — runs only after the
//! user explicitly approves it (`RuntimeMode.approval_required`, enforced by
//! its runtime capability gate). A mode-side `[Y/n]` instruction would leave
//! that to the model; this module makes it a property of the tool instead.
//!
//! A run whose [`RunRequest::mode_approval_filter`](crate::dispatch::RunRequest)
//! is set has each matching tool wrapped in [`ApprovalRequiredTool`], whose
//! `requires_confirmation()` is `true`. The agtrs executor already routes every
//! such tool through the run's `ApprovalGate` before executing it — the same
//! path `bash_exec` takes — so a Safe-mode write blocks on the user exactly as
//! a shell command does. A tool that already requires confirmation is left as
//! it is.

use std::sync::Arc;
use std::time::Duration;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_runtime::tool_hooks::ToolHook;
use async_trait::async_trait;

use crate::dispatch::ModeToolFilter;

/// The approval gate of a run with nobody to ask — agenthicc's
/// `_HeadlessApprovalService`.
///
/// Headless runs have no approval dialog, and an approval-gated call must not
/// hang forever or, worse, run unprompted. It is denied unless the run opted
/// in with `--dangerously-skip-permissions` (or xaft's `--auto-approve`), and
/// the denial names those flags so whoever reads the log knows how to allow it.
#[derive(Debug, Clone, Copy)]
pub struct HeadlessApprovalGate {
    allow: bool,
}

impl HeadlessApprovalGate {
    /// A gate that approves every request when `allow`, and denies every
    /// request otherwise.
    pub fn new(allow: bool) -> Self {
        Self { allow }
    }

    /// Whether this gate approves requests.
    pub fn allows(&self) -> bool {
        self.allow
    }
}

#[async_trait]
impl agtrs_runtime::approval::ApprovalGate for HeadlessApprovalGate {
    async fn request(
        &self,
        tool_name: &str,
        _tool_use_id: &str,
        _input: &serde_json::Value,
    ) -> bool {
        if !self.allow {
            tracing::warn!(
                tool = tool_name,
                "headless approval denied; pass --dangerously-skip-permissions (or --auto-approve) \
                 to allow it"
            );
        }
        self.allow
    }
}

/// A tool that must be approved before every call, otherwise identical to
/// the tool it wraps.
pub struct ApprovalRequiredTool {
    inner: Arc<ErasedTool>,
}

impl ApprovalRequiredTool {
    /// Wrap `inner`.
    pub fn new(inner: Arc<ErasedTool>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Tool for ApprovalRequiredTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn schema(&self) -> serde_json::Value {
        self.inner.schema()
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        self.inner.call(input, ctx).await
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    fn parallel_safe(&self) -> bool {
        self.inner.parallel_safe()
    }

    fn cache_ttl(&self) -> Option<Duration> {
        self.inner.cache_ttl()
    }

    fn hooks(&self) -> Vec<Arc<dyn ToolHook>> {
        self.inner.hooks()
    }
}

/// Wrap every tool `requires_approval` selects so it needs explicit approval.
pub fn require_approval(
    tools: Vec<Arc<ErasedTool>>,
    requires_approval: &ModeToolFilter,
) -> Vec<Arc<ErasedTool>> {
    tools
        .into_iter()
        .map(|tool| {
            if requires_approval(tool.name()) && !tool.requires_confirmation() {
                Arc::new(ApprovalRequiredTool::new(tool)) as Arc<ErasedTool>
            } else {
                tool
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// agenthicc's headless approvals fail closed: denied unless the run
    /// opted in, approved when it did.
    #[tokio::test]
    async fn the_headless_gate_fails_closed_unless_allowed() {
        let input = serde_json::json!({"command": "git push"});
        let deny = HeadlessApprovalGate::new(false);
        assert!(
            !agtrs_runtime::approval::ApprovalGate::request(&deny, "git_push", "t1", &input).await
        );
        let allow = HeadlessApprovalGate::new(true);
        assert!(
            agtrs_runtime::approval::ApprovalGate::request(&allow, "git_push", "t2", &input).await
        );
        assert!(allow.allows() && !deny.allows());
    }

    struct Probe {
        name: &'static str,
        confirm: bool,
    }

    #[async_trait]
    impl Tool for Probe {
        type Inputs = serde_json::Value;
        type Output = ToolResult;
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "probe"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(
            &self,
            _input: serde_json::Value,
            ctx: &ToolContext,
        ) -> Result<ToolResult, AgtrsError> {
            Ok(ToolResult::ok(
                format!("ran {}", self.name),
                &ctx.tool_use_id,
            ))
        }
        fn requires_confirmation(&self) -> bool {
            self.confirm
        }
        fn parallel_safe(&self) -> bool {
            true
        }
    }

    fn tool(name: &'static str, confirm: bool) -> Arc<ErasedTool> {
        Arc::new(Probe { name, confirm }) as Arc<ErasedTool>
    }

    #[test]
    fn only_selected_tools_gain_the_approval_requirement() {
        let filter: ModeToolFilter = Arc::new(|name: &str| name != "read_file");
        let tools = require_approval(
            vec![tool("read_file", false), tool("write_file", false)],
            &filter,
        );
        assert!(!tools[0].requires_confirmation(), "a read runs directly");
        assert!(tools[1].requires_confirmation(), "a write must be approved");
        assert_eq!(tools[1].name(), "write_file");
        assert!(
            tools[1].parallel_safe(),
            "everything else is the inner tool's"
        );
    }

    #[test]
    fn a_tool_that_already_asks_is_not_rewrapped() {
        let filter: ModeToolFilter = Arc::new(|_: &str| true);
        let original = tool("bash_exec", true);
        let tools = require_approval(vec![Arc::clone(&original)], &filter);
        assert!(Arc::ptr_eq(&tools[0], &original));
    }

    #[tokio::test]
    async fn the_wrapper_still_runs_the_inner_tool() {
        let wrapped = ApprovalRequiredTool::new(tool("write_file", false));
        let ctx = ToolContext::new("t-1");
        let out = wrapped.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(out.content.contains("ran write_file"), "{out:?}");
    }
}
