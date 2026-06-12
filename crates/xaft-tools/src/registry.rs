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

use crate::fs_store::FsWorkspaceStore;

use crate::fs::{
    // new write
    AppendToFileTool,
    CopyFileTool,
    CreateDirectoryTool,
    DeleteFileTool,
    // new read-only
    DiffFilesTool,
    // existing
    EditFileTool,
    FileStatToolFs,
    GlobToolFs,
    GrepTool,
    ListFilesTool,
    MoveFileTool,
    PatchFileTool,
    ReadFileTool,
    ReadManyTool,
    RemoveDirectoryTool,
    SearchFilesTool,
    TreeToolFs,
    WriteFileTool,
};
use crate::git::{
    GitAddTool, GitBlameTool, GitBranchTool, GitCheckoutFilesTool, GitCommitStagedTool,
    GitCreateBranchTool, GitDiffTool, GitGrepTool, GitLogTool, GitMergeTool, GitPushTool,
    GitRemoteTool, GitShowTool, GitStashListTool, GitStashPopTool, GitStashTool, GitStatusTool,
    GitTagTool, GitUnstageTool,
};
use crate::shell::BashExecTool;

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

// ── Pre-assembled factories ───────────────────────────────────────────────────

/// Builder for `ToolRegistry` — assembles tools for common agent roles.
pub struct ToolRegistryBuilder {
    workspace_root: PathBuf,
    executor_timeout: Duration,
    execution_policy: ExecutionPolicy,
    include_git: bool,
    include_shell: bool,
    include_write: bool,
    in_memory: bool,
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

    /// Include write tools (`write_file`, `edit_file`).
    pub fn with_write(mut self) -> Self {
        self.include_write = true;
        self
    }

    /// Disable git tools.
    pub fn without_git(mut self) -> Self {
        self.include_git = false;
        self
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
        reg.add(ReadManyTool::new(Arc::clone(&store)));
        reg.add(SearchFilesTool::new(Arc::clone(&store)));

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
        Ok(reg)
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
        reg.add(ReadManyTool::new(Arc::clone(&store)));
        reg.add(SearchFilesTool::new(Arc::clone(&store)));

        // New write fs tools
        reg.add(MoveFileTool::new(Arc::clone(&store), &root));
        reg.add(CopyFileTool::new(Arc::clone(&store), &root));
        reg.add(DeleteFileTool::new(Arc::clone(&store), &root));
        reg.add(CreateDirectoryTool::new(Arc::clone(&store), &root));
        reg.add(RemoveDirectoryTool::new(Arc::clone(&store), &root));
        reg.add(AppendToFileTool::new(Arc::clone(&store), &root));
        reg.add(PatchFileTool::new(Arc::clone(&store), &root));

        if include_shell {
            reg.add(BashExecTool::new(Arc::clone(&executor)));
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
        Ok(reg)
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
        // list_files, read_file, grep + 6 new read-only tools = 9
        assert_eq!(reg.len(), 9);
        assert!(reg.get("list_files").is_some());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("grep").is_some());
        assert!(reg.get("glob").is_some());
        assert!(reg.get("file_stat").is_some());
        assert!(reg.get("tree").is_some());
        assert!(reg.get("diff_files").is_some());
        assert!(reg.get("read_many").is_some());
        assert!(reg.get("search_files").is_some());
    }

    #[test]
    fn coder_with_shell_builds_correctly() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .without_git()
            .with_shell()
            .build_coder()
            .unwrap();
        // 9 reader + write_file + edit_file + 7 new write tools + bash_exec = 19
        assert!(reg.len() >= 19);
        assert!(reg.get("bash_exec").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("edit_file").is_some());
        assert!(reg.get("move_file").is_some());
        assert!(reg.get("copy_file").is_some());
        assert!(reg.get("delete_file").is_some());
        assert!(reg.get("create_directory").is_some());
        assert!(reg.get("remove_directory").is_some());
        assert!(reg.get("append_to_file").is_some());
        assert!(reg.get("patch_file").is_some());
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
