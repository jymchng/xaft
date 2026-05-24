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

use crate::fs::{EditFileTool, GrepTool, ListFilesTool, ReadFileTool, WriteFileTool};
use crate::git::{GitDiffTool, GitLogTool, GitStatusTool};
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
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            executor_timeout: Duration::from_secs(60),
            execution_policy: ExecutionPolicy::default(),
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
    /// plus optionally `git_status`, `git_diff`, `git_log`.
    pub fn build_reader(self) -> Result<ToolRegistry, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.make_store();
        let mut reg = ToolRegistry::new();
        reg.add(ListFilesTool::new(Arc::clone(&store)));
        reg.add(ReadFileTool::new(Arc::clone(&store)));
        reg.add(GrepTool::new(Arc::clone(&store)));
        if self.include_git {
            if let Ok(repo) = GitRepo::open(&self.workspace_root) {
                let repo = Arc::new(repo);
                reg.add(GitStatusTool::new(Arc::clone(&repo)));
                reg.add(GitDiffTool::new(Arc::clone(&repo)));
                reg.add(GitLogTool::new(Arc::clone(&repo)));
            }
        }
        Ok(reg)
    }

    /// Build a coder registry: all of reader + `write_file`, `edit_file`,
    /// plus optionally `bash_exec`.
    pub fn build_coder(self) -> Result<ToolRegistry, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.make_store();
        let executor = self.make_executor();
        let include_shell = self.include_shell;
        let include_git = self.include_git;
        let workspace_root = self.workspace_root.clone();

        let mut reg = ToolRegistry::new();
        reg.add(ListFilesTool::new(Arc::clone(&store)));
        reg.add(ReadFileTool::new(Arc::clone(&store)));
        reg.add(GrepTool::new(Arc::clone(&store)));
        reg.add(WriteFileTool::new(Arc::clone(&store)));
        reg.add(EditFileTool::new(Arc::clone(&store)));
        if include_shell {
            reg.add(BashExecTool::new(Arc::clone(&executor)));
        }
        if include_git {
            if let Ok(repo) = GitRepo::open(&workspace_root) {
                let repo = Arc::new(repo);
                reg.add(GitStatusTool::new(Arc::clone(&repo)));
                reg.add(GitDiffTool::new(Arc::clone(&repo)));
                reg.add(GitLogTool::new(Arc::clone(&repo)));
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
            .in_memory()
            .without_git()
            .build_reader()
            .unwrap();
        assert_eq!(reg.len(), 3); // list_files, read_file, grep
        assert!(reg.get("list_files").is_some());
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("grep").is_some());
    }

    #[test]
    fn coder_with_shell_builds_correctly() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .in_memory()
            .without_git()
            .with_shell()
            .build_coder()
            .unwrap();
        assert_eq!(reg.len(), 6); // list, read, grep, write, edit, bash
        assert!(reg.get("bash_exec").is_some());
        assert!(reg.get("write_file").is_some());
        assert!(reg.get("edit_file").is_some());
    }

    #[test]
    fn all_returns_in_order() {
        let tmp = TempDir::new().unwrap();
        let reg = ToolRegistryBuilder::new(tmp.path())
            .in_memory()
            .without_git()
            .build_reader()
            .unwrap();
        let names: Vec<_> = reg.all().iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, vec!["list_files", "read_file", "grep"]);
    }
}
