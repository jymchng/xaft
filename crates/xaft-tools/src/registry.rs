//! `ToolRegistry` — builder that assembles a named set of [`ErasedTool`]s
//! for a specific agent role (coder, reviewer, read-only inspector, etc.).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agtrs_git::GitRepo;
use agtrs_runtime::tool::ErasedTool;
use agtrs_shell::{CommandExecutor, ExecutionPolicy, Sandbox};
use agtrs_workspace::{InMemoryWorkspaceStore, WorkspaceStore};

use crate::browser::{BrowserSessionManager, browser_tools, read_only_browser_tools};
use crate::fs_store::FsWorkspaceStore;

use crate::fs::{
    // new write
    AppendToFileTool,
    // agenthicc fs-parity
    BatchCopyTool,
    BatchDeleteTool,
    BatchMoveTool,
    BatchReadTool,
    BatchWriteTool,
    ChecksumFileTool,
    CopyFileTool,
    CreateDirectoryTool,
    DeleteFileTool,
    // new read-only
    DiffFilesTool,
    // existing
    EditFileTool,
    FileExistsTool,
    FileStatToolFs,
    GlobToolFs,
    GrepTool,
    ListFilesTool,
    MoveFileTool,
    PatchFileTool,
    ReadFileTool,
    ReadLinesTool,
    RemoveDirectoryTool,
    SearchFilesTool,
    TouchFileTool,
    TreeToolFs,
    TruncateFileTool,
    WriteFileTool,
};
use crate::git::{
    GitAddTool, GitBlameTool, GitBranchTool, GitCheckoutFilesTool, GitCommitStagedTool,
    GitCreateBranchTool, GitDiffTool, GitGrepTool, GitLogTool, GitMergeTool, GitPushTool,
    GitRemoteTool, GitShowTool, GitStashListTool, GitStashPopTool, GitStashTool, GitStatusTool,
    GitTagTool, GitUnstageTool,
};
use crate::introspect::{Corpus, introspection_tools};
use crate::mcp::McpConnectionGuideTool;
use crate::outlook::{GraphOutlookBackend, outlook_tools, read_only_outlook_tools};
use crate::shell::{BashExecTool, RunCommandTool, RunPythonExprTool, RunPythonTool, RunTestsTool};
use crate::terminals::{
    InspectTerminalTool, StopTerminalTool, TerminalManager, WaitTerminalReadyTool, WaitTerminalTool,
};

/// A named, ordered collection of tools ready for agent registration.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<ErasedTool>>,
    order: Vec<String>,
}

impl ToolRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Register a tool. Replaces an existing entry with the same name.
    pub fn register(&mut self, tool: Arc<ErasedTool>) -> &mut Self {
        let name = tool.name().to_string();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, tool);
        self
    }

    /// Register a boxed `impl Tool`.
    pub fn add<T>(&mut self, tool: T) -> &mut Self
    where
        T: agtrs_runtime::tool::Tool<
                Inputs = serde_json::Value,
                Output = agtrs_runtime::tool::ToolResult,
            > + Send
            + Sync
            + 'static,
    {
        self.register(Arc::new(tool) as Arc<ErasedTool>)
    }

    /// All tools in registration order.
    pub fn all(&self) -> Vec<Arc<ErasedTool>> {
        self.order
            .iter()
            .filter_map(|n| self.tools.get(n).cloned())
            .collect()
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<ErasedTool>> {
        self.tools.get(name).cloned()
    }

    /// Registered names, in registration order.
    pub fn names(&self) -> Vec<String> {
        self.order.clone()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True if no tools registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Pre-assembled factories ──────────────────────────────────────────────

/// Builder for `ToolRegistry` — assembles tools for common agent roles.
pub struct ToolRegistryBuilder {
    workspace_root: PathBuf,
    executor_timeout: Duration,
    execution_policy: ExecutionPolicy,
    include_git: bool,
    include_shell: bool,
    include_write: bool,
    in_memory: bool,
    /// Background-terminal manager handed to the four terminal tools.
    ///
    /// `None` by default: the tools are still registered (see
    /// [`crate::terminals::tools`]) and refuse with agenthicc's
    /// `terminal manager unavailable`, so the agent learns the capability is
    /// absent instead of finding no such tool.
    terminal_manager: Option<Arc<TerminalManager>>,
    /// The session's browser manager, when its backend is not `none`.
    ///
    /// `None` by default: a registry built without a session has no browser
    /// tools, because which family exists (and whether it exists at all) is a
    /// per-session config decision — `tools.browser_backend` plus that
    /// section's `enabled` — not a property of the registry.
    browser: Option<Arc<BrowserSessionManager>>,
    /// Security policy applied to the assembled registry.
    ///
    /// `None` means "no policy configured", and the registry is left
    /// unfiltered. That is the default so an existing caller keeps its tools;
    /// a caller that has a loaded `XaftConfig` is expected to pass
    /// `Some(policy)`, at which point an empty allow-list denies everything
    /// (see `ToolPolicy::evaluate_tool`).
    policy: Option<xaft_config::ToolPolicy>,
    /// `[tools] http_timeout_s` — the read timeout of the Outlook tools'
    /// Microsoft Graph client (agenthicc's `agenthicc_http_client` default).
    http_timeout_s: f64,
    /// The Graph backend the Outlook tools use; `None` builds the default one
    /// (Graph v1.0, `MSGRAPH_TOKEN`, [`Self::with_http_timeout_s`]).
    outlook: Option<Arc<GraphOutlookBackend>>,
    /// The corpus the self-introspection tools read; `None` means the one the
    /// binary installed (`xaft_tools::install_embedded_corpus`).
    corpus: Option<Arc<Corpus>>,
}

impl ToolRegistryBuilder {
    /// Builder rooted at `workspace_root` with sensible defaults.
    ///
    /// The default `ExecutionPolicy` allows shell escape because `BashExecTool`
    /// now sets `requires_confirmation = true`, so the TUI approval gate acts as
    /// the safety barrier instead.  Destructive program names (`rm`, `dd`, etc.)
    /// are still blocked at the policy level as a defense-in-depth backstop.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            executor_timeout: Duration::from_secs(60),
            // allow_shell_escape = true: gate handles safety, policy handles backstop
            execution_policy: ExecutionPolicy {
                allow_shell_escape: true,
                ..ExecutionPolicy::default()
            },
            include_git: true,
            include_shell: false,
            include_write: false,
            in_memory: false,
            terminal_manager: None,
            browser: None,
            policy: None,
            http_timeout_s: crate::outlook::graph::DEFAULT_READ_TIMEOUT_S,
            outlook: None,
            corpus: None,
        }
    }

    /// Use an in-memory workspace store (useful in tests).
    pub fn in_memory(mut self) -> Self {
        self.in_memory = true;
        self
    }

    /// Override shell execution policy.
    pub fn with_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    /// Override default command timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.executor_timeout = timeout;
        self
    }

    /// Include `BashExecTool`.
    pub fn with_shell(mut self) -> Self {
        self.include_shell = true;
        self
    }

    /// Supply the background-terminal manager the four terminal tools use.
    ///
    /// Without this the tools are registered but refuse, which is agenthicc's
    /// behaviour when no manager is present in the execution context.
    pub fn with_terminal_manager(mut self, manager: Arc<TerminalManager>) -> Self {
        self.terminal_manager = Some(manager);
        self
    }

    /// Register the browser tools of `manager`'s family (parity rows 60-77).
    ///
    /// The coder registry gets all nine; the reader registry gets the five
    /// agenthicc classifies read-only (`*_status`, `*_open`, `*_snapshot`,
    /// `*_wait_for`, `*_screenshot`). A disabled backend registers nothing,
    /// exactly as agenthicc's `make_*_tools` returns `[]`. Build `manager` with
    /// [`BrowserSessionManager::from_tool_settings`], which also yields `None`
    /// for `tools.browser_backend = "none"`.
    pub fn with_browser_session(mut self, manager: Arc<BrowserSessionManager>) -> Self {
        self.browser = Some(manager);
        self
    }

    /// Attach the browser session `[tools]` selects, if any — the
    /// one-call form of [`Self::with_browser_session`].
    pub fn with_browser_settings(
        self,
        tools: &xaft_config::ToolSettingsConfig,
        session_id: Option<&str>,
    ) -> Self {
        let root = self.workspace_root.clone();
        match BrowserSessionManager::from_tool_settings(
            tools,
            root,
            session_id,
            crate::browser::BrowserLaunchOptions::default(),
        ) {
            Some(manager) => self.with_browser_session(manager),
            None => self,
        }
    }

    /// `[tools] http_timeout_s`: the read timeout, in seconds, of the Outlook
    /// tools' Graph client. `0` disables it, as in agenthicc.
    pub fn with_http_timeout_s(mut self, seconds: f64) -> Self {
        self.http_timeout_s = seconds;
        self
    }

    /// Use `backend` for the Outlook tools instead of the default Graph client.
    pub fn with_outlook_backend(mut self, backend: Arc<GraphOutlookBackend>) -> Self {
        self.outlook = Some(backend);
        self
    }

    /// Read `corpus` in the self-introspection tools instead of the embedded one.
    pub fn with_introspection_corpus(mut self, corpus: Arc<Corpus>) -> Self {
        self.corpus = Some(corpus);
        self
    }

    fn outlook_backend(&self) -> Arc<GraphOutlookBackend> {
        self.outlook.clone().unwrap_or_else(|| {
            Arc::new(
                GraphOutlookBackend::new(&self.workspace_root)
                    .with_read_timeout_s(self.http_timeout_s),
            )
        })
    }

    /// Include write tools (`write_file`, `edit_file`).
    pub fn with_write(mut self) -> Self {
        self.include_write = true;
        self
    }

    /// Build a policy from a loaded `XaftConfig`.
    ///
    /// The convenience path for the common case: it reads `[security]` and
    /// `[tools].allowed` / `[tools].denied` in one step, so a caller holding a
    /// whole config does not have to know which fields the policy needs.
    pub fn policy_from_config(config: &xaft_config::XaftConfig) -> xaft_config::ToolPolicy {
        xaft_config::ToolPolicy::new(
            &config.security,
            &config.tools.allowed,
            &config.tools.denied,
        )
    }

    /// The resource ceilings the assembled tools run under.
    ///
    /// Sourced from `[security].max_tool_cpu_seconds` /
    /// `max_tool_memory_mb`, and unbounded when neither is set.
    pub fn resource_limits(config: &xaft_config::XaftConfig) -> xaft_config::ResourceLimits {
        xaft_config::ResourceLimits {
            cpu_seconds: config.security.max_tool_cpu_seconds,
            memory_mb: config.security.max_tool_memory_mb,
        }
    }

    /// The filesystem roots the assembled tools are confined to.
    ///
    /// Reads `[security].allowed_paths` and reports them through the policy, so
    /// a caller can check a path without re-deriving the normalization rules.
    pub fn allowed_roots(config: &xaft_config::XaftConfig) -> Vec<PathBuf> {
        let roots = &config.security.allowed_paths;
        // Deliberately reports the *configured* roots rather than the policy's
        // normalized ones: this is what a caller needs in order to report the
        // scope to the user, and normalization is the policy's business.
        roots.clone()
    }

    /// The domains the assembled tools may reach.
    ///
    /// Reads `[security].network_allow_list`.
    pub fn network_allow_list(config: &xaft_config::XaftConfig) -> &Vec<String> {
        &config.security.network_allow_list
    }

    /// Whether an outbound host is permitted by `[security].network_allow_list`.
    pub fn is_host_allowed(config: &xaft_config::XaftConfig, host: &str) -> bool {
        xaft_config::ToolPolicy::new(
            &config.security,
            &config.tools.allowed,
            &config.tools.denied,
        )
        .is_host_allowed(host)
    }

    /// Whether a path is permitted by `[security].allowed_paths`.
    pub fn is_path_allowed(config: &xaft_config::XaftConfig, path: &std::path::Path) -> bool {
        xaft_config::ToolPolicy::new(
            &config.security,
            &config.tools.allowed,
            &config.tools.denied,
        )
        .is_path_allowed(path)
    }

    /// Whether the assembled tools run inside the sandbox.
    ///
    /// Reads `[security].sandbox_mode`.
    pub fn sandbox_enabled(config: &xaft_config::XaftConfig) -> bool {
        config.security.sandbox_mode
    }

    /// Apply a security policy to the assembled registry.
    ///
    /// With a policy set, a tool the policy denies is not registered at all, so
    /// the agent is never offered a capability it may not use. Note that an
    /// empty `tools.allowed` denies *everything* -- the policy is fail-closed,
    /// so a config that wants the whole registry has to say `"*"`.
    pub fn with_security_policy(mut self, policy: xaft_config::ToolPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Disable git tools.
    pub fn without_git(mut self) -> Self {
        self.include_git = false;
        self
    }

    /// Drop every tool the policy denies.
    ///
    /// Filtering here rather than at call time means the agent is never handed
    /// a tool it may not use: an offered-but-refused tool invites the model to
    /// try, and every refusal is a wasted turn.
    fn apply_policy(&self, registry: ToolRegistry) -> ToolRegistry {
        let Some(policy) = &self.policy else {
            return registry;
        };
        let kept = policy.filter_tools(registry.names().iter().map(String::as_str));
        let mut filtered = ToolRegistry::new();
        for name in kept {
            if let Some(tool) = registry.get(&name) {
                filtered.register(tool);
            }
        }
        filtered
    }

    /// Build a read-only registry: `list_files`, `read_file`, `grep`,
    /// new read-only fs tools, plus optionally `git_status`, `git_diff`, `git_log`.
    pub fn build_reader(self) -> Result<ToolRegistry, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.make_store();
        let root = self.workspace_root.clone();
        let mut reg = ToolRegistry::new();

        // Core existing tools
        reg.add(ListFilesTool::new(Arc::clone(&store)));
        reg.add(ReadFileTool::new(Arc::clone(&store)));
        reg.add(GrepTool::new(Arc::clone(&store)));

        // New read-only fs tools
        reg.add(GlobToolFs::new(Arc::clone(&store), &root));
        reg.add(FileStatToolFs::new(Arc::clone(&store), &root));
        reg.add(TreeToolFs::new(Arc::clone(&store), &root));
        reg.add(DiffFilesTool::new(Arc::clone(&store)));
        reg.add(BatchReadTool::new(Arc::clone(&store)));
        reg.add(SearchFilesTool::new(Arc::clone(&store)));

        // agenthicc fs-parity read tools
        reg.add(FileExistsTool::new(Arc::clone(&store), &root));
        reg.add(ReadLinesTool::new(Arc::clone(&store), &root));
        reg.add(ChecksumFileTool::new(Arc::clone(&store), &root));
        reg.add(McpConnectionGuideTool::new());

        // Self-introspection (parity rows 46-50): read-only and bounded, so
        // agenthicc keeps them in every capability-restricted role.
        for tool in introspection_tools(self.corpus.clone()) {
            reg.register(tool);
        }
        // Outlook (rows 51-59): the five read verbs, as the browser family
        // contributes its read verbs.
        for tool in read_only_outlook_tools(&self.outlook_backend()) {
            reg.add(tool);
        }

        if let Some(manager) = &self.browser {
            for tool in read_only_browser_tools(manager) {
                reg.add(tool);
            }
        }

        if self.include_git {
            if let Ok(repo) = GitRepo::open(&self.workspace_root) {
                let repo = Arc::new(repo);
                let rp = self.workspace_root.clone();
                reg.add(GitStatusTool::new(Arc::clone(&repo)));
                reg.add(GitDiffTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitLogTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitBlameTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitShowTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitBranchTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitStashListTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitRemoteTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitGrepTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitTagTool::new(Arc::clone(&repo), rp.clone()));
            }
        }
        Ok(self.apply_policy(reg))
    }

    /// Build a coder registry: all of reader + write tools (`write_file`, `edit_file`,
    /// new fs write tools), plus optionally `bash_exec`.
    pub fn build_coder(self) -> Result<ToolRegistry, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.make_store();
        let executor = self.make_executor();
        let include_shell = self.include_shell;
        let include_git = self.include_git;
        let workspace_root = self.workspace_root.clone();
        let root = workspace_root.clone();
        let terminal_manager = self.terminal_manager.clone();
        let browser = self.browser.clone();
        let outlook = self.outlook_backend();
        let corpus = self.corpus.clone();

        let mut reg = ToolRegistry::new();

        // Core existing tools
        reg.add(ListFilesTool::new(Arc::clone(&store)));
        reg.add(ReadFileTool::new(Arc::clone(&store)));
        reg.add(GrepTool::new(Arc::clone(&store)));
        reg.add(WriteFileTool::new(Arc::clone(&store)));
        reg.add(EditFileTool::new(Arc::clone(&store)));

        // New read-only fs tools
        reg.add(GlobToolFs::new(Arc::clone(&store), &root));
        reg.add(FileStatToolFs::new(Arc::clone(&store), &root));
        reg.add(TreeToolFs::new(Arc::clone(&store), &root));
        reg.add(DiffFilesTool::new(Arc::clone(&store)));
        reg.add(BatchReadTool::new(Arc::clone(&store)));
        reg.add(SearchFilesTool::new(Arc::clone(&store)));

        // agenthicc fs-parity read tools
        reg.add(FileExistsTool::new(Arc::clone(&store), &root));
        reg.add(ReadLinesTool::new(Arc::clone(&store), &root));
        reg.add(ChecksumFileTool::new(Arc::clone(&store), &root));
        reg.add(McpConnectionGuideTool::new());

        // New write fs tools
        reg.add(MoveFileTool::new(Arc::clone(&store), &root));
        reg.add(CopyFileTool::new(Arc::clone(&store), &root));
        reg.add(DeleteFileTool::new(Arc::clone(&store), &root));
        reg.add(CreateDirectoryTool::new(Arc::clone(&store), &root));
        reg.add(RemoveDirectoryTool::new(Arc::clone(&store), &root));
        reg.add(AppendToFileTool::new(Arc::clone(&store), &root));
        reg.add(PatchFileTool::new(Arc::clone(&store), &root));

        // agenthicc fs-parity write tools
        reg.add(TouchFileTool::new(Arc::clone(&store), &root));
        reg.add(TruncateFileTool::new(Arc::clone(&store), &root));
        reg.add(BatchWriteTool::new(Arc::clone(&store), &root));
        reg.add(BatchDeleteTool::new(Arc::clone(&store), &root));
        reg.add(BatchMoveTool::new(Arc::clone(&store), &root));
        reg.add(BatchCopyTool::new(Arc::clone(&store), &root));

        // Registered unconditionally, shell or not: these tools observe and
        // stop processes this registry started, and an agent that cannot see
        // them cannot know a terminal exists. With no manager they refuse
        // explicitly, which is agenthicc's `terminal_manager unavailable`.
        reg.add(InspectTerminalTool::new(terminal_manager.clone()));
        reg.add(WaitTerminalTool::new(terminal_manager.clone()));
        reg.add(WaitTerminalReadyTool::new(terminal_manager.clone()));
        reg.add(StopTerminalTool::new(terminal_manager.clone()));

        // Self-introspection (parity rows 46-50) and Outlook (rows 51-59).
        // agenthicc's `AGENT_TOOLS` carries both unconditionally; without a
        // corpus or a Graph token the tools still exist and say what is
        // missing. The four Outlook writes require approval.
        for tool in introspection_tools(corpus) {
            reg.register(tool);
        }
        for tool in outlook_tools(&outlook) {
            reg.add(tool);
        }

        // The session's browser family (agenthicc registers it alongside the
        // session tools); nothing when no session is attached or the backend
        // is disabled.
        if let Some(manager) = &browser {
            for tool in browser_tools(manager) {
                reg.add(tool);
            }
        }

        if include_shell {
            reg.add(BashExecTool::new(Arc::clone(&executor)));
            // `background: true` routes here, so the tool needs the same manager
            // the four terminal tools hold — otherwise run_command could start
            // nothing the agent could then name.
            let mut run_command = RunCommandTool::new(Arc::clone(&executor), &workspace_root);
            if let Some(manager) = terminal_manager.clone() {
                run_command = run_command.with_terminal_manager(manager);
            }
            reg.add(run_command);
            // `RunPythonTool::new` probes PATH once, at registry-build time, and
            // holds the result. On a host with no Python both tools are still
            // registered and still refuse with a named, actionable error (never
            // a raw spawn failure), so the agent gets a real answer instead of a
            // tool that silently does not exist.
            reg.add(RunPythonTool::new(Arc::clone(&executor), &workspace_root));
            reg.add(RunPythonExprTool::new(
                Arc::clone(&executor),
                &workspace_root,
            ));
            // agenthicc's `run_tests` (parity row 41), over the same executor
            // and approval gate as `run_command`.
            reg.add(RunTestsTool::new(Arc::clone(&executor), &workspace_root));
        }
        if include_git {
            if let Ok(repo) = GitRepo::open(&workspace_root) {
                let repo = Arc::new(repo);
                let rp = workspace_root.clone();
                // Read-only git tools
                reg.add(GitStatusTool::new(Arc::clone(&repo)));
                reg.add(GitDiffTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitLogTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitBlameTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitShowTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitBranchTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitStashListTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitRemoteTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitGrepTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitTagTool::new(Arc::clone(&repo), rp.clone()));
                // Write git tools
                reg.add(GitAddTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitUnstageTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitCommitStagedTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitStashTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitStashPopTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitCheckoutFilesTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitPushTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitCreateBranchTool::new(Arc::clone(&repo), rp.clone()));
                reg.add(GitMergeTool::new(Arc::clone(&repo), rp.clone()));
            }
        }
        Ok(self.apply_policy(reg))
    }

    fn make_store(&self) -> Arc<dyn WorkspaceStore> {
        if self.in_memory {
            Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>
        } else {
            Arc::new(FsWorkspaceStore::new(&self.workspace_root)) as Arc<dyn WorkspaceStore>
        }
    }

    fn make_executor(&self) -> Arc<CommandExecutor> {
        let sandbox = Sandbox::new(&self.workspace_root).with_timeout(self.executor_timeout);
        Arc::new(CommandExecutor::new(sandbox, self.execution_policy.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reader_builds_correctly() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .build_reader()
            .unwrap();
        // list_files, read_file, grep + 6 new read-only tools + 4 parity read
        // tools + 5 self-introspection tools + 5 Outlook read verbs = 23
        assert_eq!(reg.len(), 23);
        assert!(reg.get("list_files").is_some());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("grep").is_some());
        assert!(reg.get("glob").is_some());
        assert!(reg.get("file_stat").is_some());
        assert!(reg.get("tree").is_some());
        assert!(reg.get("diff_files").is_some());
        assert!(reg.get("batch_read").is_some());
        assert!(reg.get("search_files").is_some());
        assert!(reg.get("file_exists").is_some());
        assert!(reg.get("read_lines").is_some());
        assert!(reg.get("checksum_file").is_some());
        assert!(reg.get("mcp_connection_guide").is_some());
        for name in crate::INTROSPECT_TOOL_NAMES {
            assert!(reg.get(name).is_some(), "{name}");
        }
        for name in [
            "list_emails",
            "read_email",
            "search_emails",
            "list_folders",
            "calendar_events",
        ] {
            assert!(reg.get(name).is_some(), "{name}");
        }
        for name in ["send_email", "reply_email", "move_email", "create_event"] {
            assert!(
                reg.get(name).is_none(),
                "{name} writes, so not in the reader registry"
            );
        }
    }

    /// Rows 41 and 51-59: `run_tests` sits with the shell tools, the nine
    /// Outlook tools with every coder registry, and each that changes
    /// something outside the conversation asks first.
    #[test]
    fn the_parity_tools_are_registered_and_gated() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .with_shell()
            .with_http_timeout_s(12.0)
            .build_coder()
            .unwrap();
        assert!(reg.get("run_tests").unwrap().requires_confirmation());
        for name in crate::OUTLOOK_TOOL_NAMES {
            let tool = reg.get(name).unwrap_or_else(|| panic!("{name} missing"));
            let writes =
                ["send_email", "reply_email", "move_email", "create_event"].contains(&name);
            assert_eq!(tool.requires_confirmation(), writes, "{name}");
        }
        for name in crate::INTROSPECT_TOOL_NAMES {
            assert!(!reg.get(name).unwrap().requires_confirmation(), "{name}");
        }
        let no_shell = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .build_coder()
            .unwrap();
        assert!(
            no_shell.get("run_tests").is_none(),
            "run_tests needs the shell"
        );
    }

    /// The two Python tools are registered UNCONDITIONALLY, not gated on the
    /// probe succeeding. That is deliberate: on a host with no interpreter they
    /// must still be present so the agent gets a named, actionable refusal
    /// rather than silently discovering the capability is missing. Both halves
    /// are asserted — reachable here, and a plain refusal in
    /// `shell::run_python`'s absent-interpreter tests.
    #[test]
    fn the_python_tools_are_registered_and_named() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .with_shell()
            .build_coder()
            .unwrap();
        assert!(reg.get("run_python").is_some(), "run_python not registered");
        assert!(
            reg.get("run_python_expr").is_some(),
            "run_python_expr not registered"
        );
        assert_eq!(reg.get("run_python").unwrap().name(), "run_python");
        assert_eq!(
            reg.get("run_python_expr").unwrap().name(),
            "run_python_expr"
        );
        // Both sit behind the same approval gate as bash_exec: spawning a
        // process is the risk, not the language.
        assert!(reg.get("run_python").unwrap().requires_confirmation());
        assert!(reg.get("run_python_expr").unwrap().requires_confirmation());
    }

    #[test]
    fn coder_with_shell_builds_correctly() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .with_shell()
            .build_coder()
            .unwrap();
        // Measured 32 with the Python tools registered. A floor, not an exact
        // count, so an unrelated addition does not fail here; the individual
        // tools are asserted by name below and by the parity reachability test.
        assert!(reg.len() >= 32, "got {}", reg.len());
        assert!(reg.get("bash_exec").is_some());
        assert!(reg.get("run_command").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("edit_file").is_some());
        assert!(reg.get("move_file").is_some());
        assert!(reg.get("copy_file").is_some());
        assert!(reg.get("delete_file").is_some());
        assert!(reg.get("create_directory").is_some());
        assert!(reg.get("remove_directory").is_some());
        assert!(reg.get("append_to_file").is_some());
        assert!(reg.get("patch_file").is_some());
        assert!(reg.get("touch_file").is_some());
        assert!(reg.get("truncate_file").is_some());
        assert!(reg.get("batch_write").is_some());
        assert!(reg.get("batch_delete").is_some());
        assert!(reg.get("batch_move").is_some());
        assert!(reg.get("batch_copy").is_some());
    }

    #[test]
    fn all_returns_in_order() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .build_reader()
            .unwrap();
        let names: Vec<_> = reg.all().iter().map(|t| t.name().to_string()).collect();
        // First 3 should be the existing core tools
        assert_eq!(&names[..3], &["list_files", "read_file", "grep"]);
    }
}
