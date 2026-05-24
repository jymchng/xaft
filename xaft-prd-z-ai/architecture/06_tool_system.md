# 06 — Tool System

> Tool trait, ErasedTool, ToolContext, hooks, cache, requires_confirmation,
> schema generation, #[tool] macro, built-in tool catalog, MCP integration, and guardrails.

---

## Overview

Tools are the agent's hands — the only way it can affect the world. xaft's tool system is built on the agtrs `Tool` trait, extended with type erasure for heterogeneous collections, lifecycle hooks, caching, approval gates, and declarative schema generation. Every tool is observable, controllable, and safe.

---

## Tool Trait (agtrs)

The base `Tool` trait from agtrs:

```rust
/// Core tool trait from the agtrs framework.
/// A tool is a named, typed operation that an agent can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The input type for this tool.
    type Input: DeserializeOwned + Serialize + Send + Sync;

    /// The output type for this tool.
    type Output: Serialize + Send + Sync;

    /// Unique name for this tool (used by LLM for tool selection).
    fn name(&self) -> &str;

    /// Human-readable description of what this tool does.
    /// This description is sent to the LLM as part of the tool schema.
    fn description(&self) -> &str;

    /// JSON Schema for the input type.
    /// Generated automatically via schemars in most cases.
    fn input_schema(&self) -> serde_json::Value;

    /// JSON Schema for the output type.
    fn output_schema(&self) -> serde_json::Value;

    /// Whether this tool requires user approval before execution.
    fn requires_confirmation(&self) -> bool {
        false
    }

    /// Whether this tool modifies the workspace (vs read-only).
    fn is_destructive(&self) -> bool {
        false
    }

    /// Execute the tool with the given input and context.
    async fn execute(&self, input: Self::Input, ctx: &ToolContext) -> Result<Self::Output, ToolError>;

    /// Validate input before execution (optional).
    fn validate(&self, input: &Self::Input) -> Result<(), ToolError> {
        Ok(())
    }
}
```

---

## ErasedTool

Since agents hold collections of different tool types, xaft uses type erasure:

```rust
/// Type-erased tool for heterogeneous collections.
pub struct ErasedTool {
    /// Tool name.
    name: String,

    /// Tool description.
    description: String,

    /// Input JSON Schema.
    input_schema: serde_json::Value,

    /// Output JSON Schema.
    output_schema: serde_json::Value,

    /// Whether this tool requires confirmation.
    requires_confirmation: bool,

    /// Whether this tool is destructive.
    is_destructive: bool,

    /// Type-erased execute function.
    execute_fn: Box<dyn Fn(&str, &ToolContext) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> + Send + Sync>,

    /// Type-erased validate function.
    validate_fn: Box<dyn Fn(&str) -> Result<(), ToolError> + Send + Sync>,
}

impl ErasedTool {
    /// Create an ErasedTool from a concrete Tool implementation.
    pub fn from_tool<T: Tool>(tool: T) -> Self {
        let name = tool.name().to_string();
        let description = tool.description().to_string();
        let input_schema = tool.input_schema();
        let output_schema = tool.output_schema();
        let requires_confirmation = tool.requires_confirmation();
        let is_destructive = tool.is_destructive();

        // Capture the tool in the closure
        let tool = Arc::new(tool);

        let execute_tool = tool.clone();
        let execute_fn = Box::new(move |args: &str, ctx: &ToolContext| {
            let input: T::Input = match serde_json::from_str(args) {
                Ok(i) => i,
                Err(e) => return Box::pin(async move {
                    Err(ToolError::InvalidInput(e.to_string()))
                }) as Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>>,
            };
            let ctx = ctx.clone();
            Box::pin(async move {
                let result = execute_tool.execute(input, &ctx).await?;
                let output = ToolOutput::from_serializable(&result)?;
                Ok(output)
            })
        });

        let validate_tool = tool.clone();
        let validate_fn = Box::new(move |args: &str| {
            let input: T::Input = serde_json::from_str(args)
                .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
            validate_tool.validate(&input)
        });

        Self {
            name,
            description,
            input_schema,
            output_schema,
            requires_confirmation,
            is_destructive,
            execute_fn,
            validate_fn,
        }
    }

    /// Execute the tool with JSON string args.
    pub async fn execute(&self, args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        // Validate first
        self.validate_fn(args)?;

        // Execute
        (self.execute_fn)(args, ctx).await
    }

    /// Validate the input without executing.
    pub fn validate(&self, args: &str) -> Result<(), ToolError> {
        self.validate_fn(args)
    }

    /// Get the tool name.
    pub fn name(&self) -> &str { &self.name }

    /// Get the tool description.
    pub fn description(&self) -> &str { &self.description }

    /// Get the input schema.
    pub fn input_schema(&self) -> &serde_json::Value { &self.input_schema }

    /// Whether this tool requires confirmation.
    pub fn requires_confirmation(&self) -> bool { self.requires_confirmation }

    /// Whether this tool is destructive.
    pub fn is_destructive(&self) -> bool { self.is_destructive }
}

/// Generic tool output (type-erased).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub content: String,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl ToolOutput {
    pub fn ok(content: &str) -> Self {
        Self {
            success: true,
            content: content.to_string(),
            metadata: HashMap::new(),
        }
    }

    pub fn error(msg: &str) -> Self {
        Self {
            success: false,
            content: msg.to_string(),
            metadata: HashMap::new(),
        }
    }

    pub fn from_serializable<T: Serialize>(value: &T) -> Result<Self, ToolError> {
        Ok(Self {
            success: true,
            content: serde_json::to_string(value)
                .map_err(|e| ToolError::Serialization(e.to_string()))?,
            metadata: HashMap::new(),
        })
    }

    pub fn is_ok(&self) -> bool { self.success }

    pub fn summary(&self) -> String {
        if self.success {
            self.content.chars().take(200).collect()
        } else {
            format!("ERROR: {}", self.content.chars().take(200).collect::<String>())
        }
    }

    pub fn modified_files(&self) -> Option<Vec<PathBuf>> {
        self.metadata.get("modified_files")
            .and_then(|v| serde_json::from_value::<Vec<PathBuf>>(v.clone()).ok())
    }

    pub fn cost(&self) -> Option<f64> {
        self.metadata.get("cost_usd")
            .and_then(|v| v.as_f64())
    }
}
```

---

## ToolContext

The execution context passed to every tool:

```rust
/// Execution context available to all tools.
#[derive(Clone)]
pub struct ToolContext {
    /// Workspace store for file operations.
    pub workspace: Arc<dyn WorkspaceStore>,

    /// Git repository for git operations.
    pub git: Arc<dyn GitRepo>,

    /// Signal bus for emitting events.
    pub signal_bus: Arc<SignalBus>,

    /// Cancellation token for aborting tool execution.
    pub cancellation_token: CancellationToken,

    /// Optional progress callback for streaming tool output.
    pub progress_callback: Option<Arc<dyn Fn(ToolProgressUpdate) + Send + Sync>>,

    /// Tool-specific configuration.
    pub config: Arc<XaftConfig>,
}

impl ToolContext {
    /// Emit a progress update from within a tool.
    pub fn progress(&self, update: ToolProgressUpdate) {
        if let Some(ref cb) = self.progress_callback {
            cb(update);
        }
    }

    /// Check if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }
}
```

---

## Tool Hooks

### HookedTool Wrapper

The `HookedTool` wraps any tool with before/after hooks:

```rust
/// Wrapper that adds lifecycle hooks to any tool.
pub struct HookedTool<T: Tool> {
    inner: T,
    before_hooks: Vec<Box<dyn BeforeToolHook>>,
    after_hooks: Vec<Box<dyn AfterToolHook>>,
}

#[async_trait]
pub trait BeforeToolHook: Send + Sync {
    /// Called before tool execution. Return Err to veto the call.
    async fn before(
        &self,
        tool_name: &str,
        args: &str,
        ctx: &ToolContext,
    ) -> Result<HookAction, ToolError>;
}

#[async_trait]
pub trait AfterToolHook: Send + Sync {
    /// Called after tool execution. Can modify the result.
    async fn after(
        &self,
        tool_name: &str,
        args: &str,
        result: &mut ToolOutput,
        ctx: &ToolContext,
    ) -> Result<(), ToolError>;
}

/// Action returned by a before hook.
pub enum HookAction {
    /// Allow the tool call to proceed.
    Continue,
    /// Deny the tool call with a reason.
    Deny(String),
    /// Modify the arguments before proceeding.
    ModifyArgs(String),
}

impl<T: Tool> HookedTool<T> {
    pub fn new(tool: T) -> Self {
        Self {
            inner: tool,
            before_hooks: Vec::new(),
            after_hooks: Vec::new(),
        }
    }

    pub fn with_before_hook(mut self, hook: Box<dyn BeforeToolHook>) -> Self {
        self.before_hooks.push(hook);
        self
    }

    pub fn with_after_hook(mut self, hook: Box<dyn AfterToolHook>) -> Self {
        self.after_hooks.push(hook);
        self
    }
}

#[async_trait]
impl<T: Tool> Tool for HookedTool<T> {
    type Input = T::Input;
    type Output = T::Output;

    fn name(&self) -> &str { self.inner.name() }
    fn description(&self) -> &str { self.inner.description() }
    fn input_schema(&self) -> serde_json::Value { self.inner.input_schema() }
    fn output_schema(&self) -> serde_json::Value { self.inner.output_schema() }
    fn requires_confirmation(&self) -> bool { self.inner.requires_confirmation() }
    fn is_destructive(&self) -> bool { self.inner.is_destructive() }

    async fn execute(&self, input: Self::Input, ctx: &ToolContext) -> Result<Self::Output, ToolError> {
        let args_json = serde_json::to_string(&input)?;

        // Run before hooks
        for hook in &self.before_hooks {
            match hook.before(self.name(), &args_json, ctx).await? {
                HookAction::Continue => continue,
                HookAction::Deny(reason) => return Err(ToolError::Denied(reason)),
                HookAction::ModifyArgs(new_args) => {
                    // Re-deserialize with modified args
                    // (This is a simplification; real implementation would be more careful)
                    let modified_input: Self::Input = serde_json::from_str(&new_args)?;
                    return self.inner.execute(modified_input, ctx).await;
                }
            }
        }

        // Execute the inner tool
        let mut result = self.inner.execute(input, ctx).await?;

        // Run after hooks
        let mut output = ToolOutput::from_serializable(&result)?;
        for hook in &self.after_hooks {
            hook.after(self.name(), &args_json, &mut output, ctx).await?;
        }

        Ok(result)
    }
}
```

### Built-in Hooks

| Hook | Type | Purpose |
|---|---|---|
| `AuditLogHook` | After | Log all tool calls to audit trail |
| `CostTrackingHook` | After | Track tool-related costs |
| `GitAutoCommitHook` | After | Auto-commit after file-modifying tools |
| `PathSanitizationHook` | Before | Validate paths before file operations |
| `RateLimitHook` | Before | Enforce per-tool rate limits |
| `CacheCheckHook` | Before | Check cache before execution |
| `CacheStoreHook` | After | Store result in cache after execution |

---

## Cache System

```rust
/// Tool result cache with TTL and invalidation.
pub struct ToolCache {
    /// In-memory cache store.
    store: RwLock<HashMap<CacheKey, CacheEntry>>,

    /// Default TTL for cache entries.
    default_ttl: Duration,

    /// Maximum cache size in entries.
    max_entries: usize,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    tool_name: String,
    args_hash: u64,  // FNV hash of serialized args
}

#[derive(Debug, Clone)]
struct CacheEntry {
    result: ToolOutput,
    created_at: Instant,
    ttl: Duration,
    /// Which files this cache entry depends on.
    dependencies: Vec<PathBuf>,
}

impl ToolCache {
    /// Check if a cache entry exists and is still valid.
    pub async fn get(&self, tool_name: &str, args: &str) -> Option<ToolOutput> {
        let key = CacheKey {
            tool_name: tool_name.to_string(),
            args_hash: Self::hash_args(args),
        };

        let store = self.store.read().await;
        store.get(&key).and_then(|entry| {
            if entry.created_at.elapsed() < entry.ttl {
                Some(entry.result.clone())
            } else {
                None
            }
        })
    }

    /// Store a result in the cache.
    pub async fn put(
        &self,
        tool_name: &str,
        args: &str,
        result: ToolOutput,
        dependencies: Vec<PathBuf>,
    ) {
        let key = CacheKey {
            tool_name: tool_name.to_string(),
            args_hash: Self::hash_args(args),
        };

        let mut store = self.store.write().await;

        // Evict if at capacity
        if store.len() >= self.max_entries {
            // Remove oldest entry
            if let Some(oldest_key) = store.iter()
                .min_by_key(|(_, v)| v.created_at)
                .map(|(k, _)| k.clone())
            {
                store.remove(&oldest_key);
            }
        }

        store.insert(key, CacheEntry {
            result,
            created_at: Instant::now(),
            ttl: self.default_ttl,
            dependencies,
        });
    }

    /// Invalidate all cache entries that depend on a modified file.
    pub async fn invalidate_for_file(&self, path: &Path) {
        let mut store = self.store.write().await;
        store.retain(|_, entry| {
            !entry.dependencies.iter().any(|dep| dep == path)
        });
    }

    fn hash_args(args: &str) -> u64 {
        use std::hash::Hasher;
        let mut hasher = fnv::FnvHasher::default();
        hasher.write(args.as_bytes());
        hasher.finish()
    }
}
```

### Cache TTL by Tool Type

| Tool | Default TTL | Cache Key | Invalidation |
|---|---|---|---|
| `read_file` | 60s | (path, file_mtime) | File modification |
| `list_files` | 30s | (root, pattern) | File creation/deletion |
| `grep` | 120s | (pattern, options) | File modification |
| `git_status` | 10s | (repo_root) | Git operations |
| `git_diff` | 30s | (repo_root, ref) | Git commits |
| `bash_exec` | No cache | N/A | Never cached |
| `edit_file` | No cache | N/A | Never cached |
| `semantic_search` | 300s | (query, filters) | Index rebuild |

---

## requires_confirmation

The confirmation system determines which tools need user approval:

```rust
/// Approval policy configuration.
#[derive(Debug, Clone)]
pub struct ApprovalPolicy {
    /// Default policy for tools not explicitly listed.
    default: ToolApproval,

    /// Per-tool override policies.
    overrides: HashMap<String, ToolApproval>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolApproval {
    /// Always require confirmation.
    Confirm,

    /// Automatically approve without confirmation.
    AutoApprove,

    /// Always deny this tool.
    Deny,
}

impl ApprovalPolicy {
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        let policy = self.overrides
            .get(tool_name)
            .unwrap_or(&self.default);

        match policy {
            ToolApproval::Confirm => true,
            ToolApproval::AutoApprove => false,
            ToolApproval::Deny => true, // Deny also needs to show the user
        }
    }

    pub fn is_denied(&self, tool_name: &str) -> bool {
        let policy = self.overrides
            .get(tool_name)
            .unwrap_or(&self.default);

        matches!(policy, ToolApproval::Deny)
    }
}
```

### Default Approval Policies

| Tool | Default Policy | Rationale |
|---|---|---|
| `read_file` | AutoApprove | Read-only, no risk |
| `list_files` | AutoApprove | Read-only |
| `grep` | AutoApprove | Read-only |
| `semantic_search` | AutoApprove | Read-only |
| `edit_file` | Confirm | Modifies workspace |
| `write_file` | Confirm | Creates/overwrites files |
| `delete_file` | Confirm | Destructive |
| `bash_exec` | Confirm | Can execute arbitrary commands |
| `git_commit` | AutoApprove | Non-destructive git operation |
| `git_push` | Confirm | Pushes to remote |
| `git_branch` | AutoApprove | Non-destructive |
| `mcp_*` | Confirm | Unknown external tools |

---

## Schema Generation via schemars

Every tool's input and output types generate JSON Schemas via `schemars`:

```rust
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

/// Example: ReadFile tool with schemars-derived schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileInput {
    /// Path to the file to read, relative to workspace root.
    pub path: String,

    /// Optional start line (1-indexed). If omitted, reads from beginning.
    pub start_line: Option<u32>,

    /// Optional end line (inclusive). If omitted, reads to end.
    pub end_line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileOutput {
    /// The file content.
    pub content: String,

    /// Total number of lines in the file.
    pub total_lines: u32,

    /// The lines that were read.
    pub lines: Vec<LineOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LineOutput {
    pub number: u32,
    pub content: String,
}

/// The ReadFile tool implementation.
pub struct ReadFileTool {
    workspace: Arc<dyn WorkspaceStore>,
}

#[async_trait]
impl Tool for ReadFileTool {
    type Input = ReadFileInput;
    type Output = ReadFileOutput;

    fn name(&self) -> &str { "read_file" }

    fn description(&self) -> &str {
        "Read the contents of a file in the workspace. Returns the file content \
         with line numbers. Optionally specify a line range to read only a portion \
         of the file."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(ReadFileInput)).unwrap()
    }

    fn output_schema(&self) -> serde_json::Value {
        serde_json::to_value(schema_for!(ReadFileOutput)).unwrap()
    }

    fn requires_confirmation(&self) -> bool { false }
    fn is_destructive(&self) -> bool { false }

    async fn execute(&self, input: Self::Input, ctx: &ToolContext) -> Result<Self::Output, ToolError> {
        let path = Path::new(&input.path);
        let content = self.workspace.read_file(path).await
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        let all_lines: Vec<LineOutput> = content.lines()
            .enumerate()
            .map(|(i, line)| LineOutput {
                number: i as u32 + 1,
                content: line.to_string(),
            })
            .collect();

        let start = input.start_line.unwrap_or(1) as usize;
        let end = input.end_line.unwrap_or(all_lines.len() as u32) as usize;

        let lines: Vec<LineOutput> = all_lines.into_iter()
            .filter(|l| l.number as usize >= start && l.number as usize <= end)
            .collect();

        Ok(ReadFileOutput {
            content: lines.iter().map(|l| &l.content).cloned().collect::<Vec<_>>().join("\n"),
            total_lines: content.lines().count() as u32,
            lines,
        })
    }
}
```

---

## The #[tool] Macro

xaft provides a procedural macro for declarative tool definition:

```rust
/// Declarative tool definition using the #[tool] macro.
///
/// This generates:
/// 1. The Tool implementation
/// 2. JSON Schema via schemars
/// 3. Input validation
/// 4. Schema generation for the LLM
///
/// Example usage:
#[tool(
    name = "edit_file",
    description = "Edit a file by replacing a block of lines. \
                   The old content must match exactly for the edit to succeed.",
    requires_confirmation = true,
    is_destructive = true
)]
pub struct EditFileInput {
    /// Path to the file to edit, relative to workspace root.
    pub path: String,

    /// The content to find (must match exactly).
    pub old_content: String,

    /// The content to replace it with.
    pub new_content: String,
}

/// This expands to:
///
/// pub struct EditFileTool { workspace: Arc<dyn WorkspaceStore> }
///
/// impl Tool for EditFileTool {
///     type Input = EditFileInput;
///     type Output = EditFileOutput;
///     fn name(&self) -> &str { "edit_file" }
///     fn description(&self) -> &str { "Edit a file by replacing..." }
///     fn input_schema(&self) -> serde_json::Value { schema_for!(EditFileInput) }
///     fn requires_confirmation(&self) -> bool { true }
///     fn is_destructive(&self) -> bool { true }
///     async fn execute(&self, input: EditFileInput, ctx: &ToolContext) -> Result<EditFileOutput, ToolError> {
///         // Generated implementation using FileEditor
///     }
/// }
```

### Macro Expansion

```rust
/// The #[tool] macro expansion (simplified).
pub fn tool_macro(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_args = parse_macro_input!(attr as ToolAttributes);
    let input = parse_macro_input!(item as DeriveInput);

    let name = &attr_args.name;
    let description = &attr_args.description;
    let requires_confirmation = attr_args.requires_confirmation;
    let is_destructive = attr_args.is_destructive;

    let input_type = &input.ident;
    let output_type = format_ident!("{}Output", input_type);
    let tool_struct = format_ident!("{}Tool", to_pascal_case(name));

    let expanded = quote! {
        // Keep the input struct
        #input

        // Generate output struct
        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
        pub struct #output_type {
            pub success: bool,
            pub path: String,
            pub lines_changed: u32,
            pub diff: String,
        }

        // Generate tool struct
        pub struct #tool_struct {
            workspace: Arc<dyn WorkspaceStore>,
        }

        impl #tool_struct {
            pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
                Self { workspace }
            }
        }

        #[async_trait]
        impl Tool for #tool_struct {
            type Input = #input_type;
            type Output = #output_type;

            fn name(&self) -> &str { #name }
            fn description(&self) -> &str { #description }
            fn input_schema(&self) -> serde_json::Value {
                serde_json::to_value(schemars::schema_for!(#input_type)).unwrap()
            }
            fn output_schema(&self) -> serde_json::Value {
                serde_json::to_value(schemars::schema_for!(#output_type)).unwrap()
            }
            fn requires_confirmation(&self) -> bool { #requires_confirmation }
            fn is_destructive(&self) -> bool { #is_destructive }

            async fn execute(&self, input: Self::Input, ctx: &ToolContext) -> Result<Self::Output, ToolError> {
                // Tool-specific execute logic (generated or manual)
                todo!("Implement execute for {}", #name)
            }
        }
    };

    TokenStream::from(expanded)
}
```

---

## Built-in Tool Catalog

### Workspace Tools

| Tool | Name | Destructive | Confirm | Description |
|---|---|---|---|---|
| `ReadFileTool` | `read_file` | No | No | Read file contents with optional line range |
| `WriteFileTool` | `write_file` | Yes | Yes | Create or overwrite a file |
| `EditFileTool` | `edit_file` | Yes | Yes | Replace a block of lines in a file |
| `DeleteFileTool` | `delete_file` | Yes | Yes | Delete a file |
| `ListFilesTool` | `list_files` | No | No | List files matching a pattern |
| `GrepTool` | `grep` | No | No | Search for patterns in files |
| `TreeTool` | `tree` | No | No | Show directory tree structure |

### Git Tools

| Tool | Name | Destructive | Confirm | Description |
|---|---|---|---|---|
| `GitStatusTool` | `git_status` | No | No | Show git working tree status |
| `GitDiffTool` | `git_diff` | No | No | Show unstaged/staged changes |
| `GitLogTool` | `git_log` | No | No | Show commit history |
| `GitBranchTool` | `git_branch` | No | No | List/create/switch branches |
| `GitCommitTool` | `git_commit` | No | No | Commit staged changes |
| `GitAddTool` | `git_add` | No | No | Stage files for commit |
| `GitPushTool` | `git_push` | Yes | Yes | Push to remote |
| `GitStashTool` | `git_stash` | No | No | Stash/unstash changes |

### Shell Tools

| Tool | Name | Destructive | Confirm | Description |
|---|---|---|---|---|
| `BashExecTool` | `bash_exec` | Yes | Yes | Execute a bash command |
| `CargoTool` | `cargo` | Yes | Yes | Run cargo commands (build, test, etc.) |
| `NpmTool` | `npm` | Yes | Yes | Run npm commands |

### Search Tools

| Tool | Name | Destructive | Confirm | Description |
|---|---|---|---|---|
| `SemanticSearchTool` | `semantic_search` | No | No | Search by semantic meaning |
| `SymbolSearchTool` | `symbol_search` | No | No | Search for code symbols |
| `FileSearchTool` | `file_search` | No | No | Find files by name pattern |

### MCP Tools

Dynamically registered from MCP server configuration:

```rust
/// MCP tool registration.
pub struct McpToolRegistration {
    pub server_name: String,
    pub tool_name: String,
    pub schema: serde_json::Value,
}

impl McpToolRegistration {
    /// Convert to an ErasedTool.
    pub fn into_tool(self, client: Arc<McpClient>) -> ErasedTool {
        let name = format!("mcp_{}_{}", self.server_name, self.tool_name);
        let description = self.schema.get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("MCP tool")
            .to_string();

        let schema = self.schema.clone();
        let client = client.clone();
        let tool_name = self.tool_name.clone();

        // Create a dynamic ErasedTool that calls the MCP server
        ErasedTool::from_dynamic(
            name,
            description,
            schema,
            true,  // requires_confirmation (MCP tools default to confirm)
            false, // is_destructive (unknown)
            move |args: &str, ctx: &ToolContext| {
                let client = client.clone();
                let tool_name = tool_name.clone();
                let args = args.to_string();
                Box::pin(async move {
                    client.call_tool(&tool_name, &args).await
                        .map_err(|e| ToolError::Execution(e.to_string()))
                })
            },
        )
    }
}
```

---

## Guardrail Integration

The `Guardrail` trait provides a higher-level safety layer on top of tool hooks:

```rust
/// Guardrail trait for enforcing safety policies on tool calls.
pub trait Guardrail: Send + Sync {
    /// Name of this guardrail for logging.
    fn name(&self) -> &str;

    /// Check a tool call before execution.
    /// Return Allow, Deny, or Modify.
    fn check_tool_call(
        &self,
        tool_name: &str,
        args: &str,
    ) -> Result<GuardrailVerdict, GuardrailError>;

    /// Pre-check before the agent starts (e.g., validate workspace state).
    fn pre_check(
        &self,
        scratchpad: &Scratchpad,
        workspace: &dyn WorkspaceStore,
    ) -> Result<(), GuardrailError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum GuardrailVerdict {
    Allow,
    Deny(String),
    Modify(String),
}

/// Built-in guardrails.
pub mod guardrails {
    /// Prevent writing to files outside the workspace root.
    pub struct PathBoundaryGuardrail;

    /// Prevent executing commands that match dangerous patterns.
    pub struct CommandSafetyGuardrail {
        blocked_patterns: Vec<regex::Regex>,
    }

    /// Enforce rate limits on tool calls.
    pub struct RateLimitGuardrail {
        max_calls_per_minute: HashMap<String, u32>,
        call_times: RwLock<HashMap<String, Vec<Instant>>>,
    }

    /// Prevent modifying files that haven't been read first (read-before-write).
    pub struct ReadBeforeWriteGuardrail {
        read_files: RwLock<HashSet<PathBuf>>,
    }

    /// Enforce maximum file size limits.
    pub struct FileSizeGuardrail {
        max_file_size_bytes: u64,
    }
}

impl Guardrail for CommandSafetyGuardrail {
    fn name(&self) -> &str { "command_safety" }

    fn check_tool_call(&self, tool_name: &str, args: &str) -> Result<GuardrailVerdict, GuardrailError> {
        if tool_name != "bash_exec" {
            return Ok(GuardrailVerdict::Allow);
        }

        // Parse command from args
        let input: serde_json::Value = serde_json::from_str(args)
            .map_err(|e| GuardrailError::ParseError(e.to_string()))?;

        let command = input.get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        for pattern in &self.blocked_patterns {
            if pattern.is_match(command) {
                return Ok(GuardrailVerdict::Deny(
                    format!("Command matches blocked pattern: {}", pattern.as_str())
                ));
            }
        }

        Ok(GuardrailVerdict::Allow)
    }
}
```

### Default Blocked Command Patterns

```rust
impl CommandSafetyGuardrail {
    pub fn default() -> Self {
        let blocked_patterns = vec![
            regex::Regex::new(r"rm\s+-rf\s+/").unwrap(),          // rm -rf /
            regex::Regex::new(r"sudo\s+").unwrap(),                // sudo
            regex::Regex::new(r"chmod\s+777").unwrap(),            // chmod 777
            regex::Regex::new(r">\s*/dev/sd").unwrap(),            // Write to disk devices
            regex::Regex::new(r"curl\s+.*\|\s*sh").unwrap(),       // Curl pipe shell
            regex::Regex::new(r"wget\s+.*\|\s*sh").unwrap(),       // Wget pipe shell
            regex::Regex::new(r"mkfs\.").unwrap(),                 // Format filesystem
            regex::Regex::new(r"dd\s+if=.*of=/dev/").unwrap(),    // DD to device
            regex::Regex::new(r":\(\)\{.*\}").unwrap(),            // Fork bomb
        ];

        Self { blocked_patterns }
    }
}
```

---

## SubagentTool with Typed Returns

`SubagentTool<T>` allows spawning sub-agents with typed output:

```rust
/// Tool that spawns a sub-agent and returns a typed result.
pub struct SubagentTool<T: Serialize + DeserializeOwned> {
    /// Tool name.
    name: String,

    /// The sub-agent to spawn.
    agent: Box<dyn Agent>,

    /// Signal bus for event propagation.
    signal_bus: Arc<SignalBus>,

    /// Phantom data for the return type.
    _marker: PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> SubagentTool<T> {
    pub fn new(
        name: &str,
        agent: Box<dyn Agent>,
        signal_bus: Arc<SignalBus>,
    ) -> Self {
        Self {
            name: name.to_string(),
            agent,
            signal_bus,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> Tool for SubagentTool<T> {
    type Input = SubagentInput;
    type Output = T;

    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { "Spawn a sub-agent to handle a sub-task" }
    fn requires_confirmation(&self) -> bool { false }
    fn is_destructive(&self) -> bool { true } // Sub-agents can modify workspace

    async fn execute(&self, input: SubagentInput, ctx: &ToolContext) -> Result<T, ToolError> {
        self.signal_bus.emit(Signal::SubagentStarted {
            agent_id: self.agent.id().clone(),
            parent_id: AgentId::current(),
        }).ok();

        // Run the sub-agent
        let executor = AgentExecutor::new(AgentExecutorConfig::default());
        let result = executor.run(
            &*self.agent,
            &input.task,
            /* provider */,
            ctx.cancellation_token.clone(),
        ).await
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        // Extract the typed result from the agent's final output
        let typed: T = serde_json::from_str(&result.summary)
            .map_err(|e| ToolError::Execution(
                format!("Failed to parse sub-agent output as {}: {}", std::any::type_name::<T>(), e)
            ))?;

        self.signal_bus.emit(Signal::SubagentComplete {
            agent_id: self.agent.id().clone(),
            result_summary: format!("{:?}", typed),
        }).ok();

        Ok(typed)
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubagentInput {
    /// The task description for the sub-agent.
    pub task: String,
    /// Optional context to inject.
    pub context: Option<String>,
}
```

---

## StructuredLlm Tool

Some tools need to make LLM calls themselves (e.g., for summarization or analysis). `StructuredLlm<T>` provides typed LLM responses:

```rust
/// Tool that uses the LLM for structured analysis.
pub struct AnalyzeCodeTool {
    structured_llm: StructuredLlm<CodeAnalysis>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CodeAnalysis {
    pub complexity: Complexity,
    pub issues: Vec<CodeIssue>,
    pub suggestions: Vec<String>,
    pub test_coverage_estimate: f32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub enum Complexity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CodeIssue {
    pub severity: String,
    pub description: String,
    pub line: Option<u32>,
}

#[async_trait]
impl Tool for AnalyzeCodeTool {
    type Input = AnalyzeCodeInput;
    type Output = CodeAnalysis;

    fn name(&self) -> &str { "analyze_code" }
    fn description(&self) -> &str { "Analyze code for issues, complexity, and suggestions" }
    fn requires_confirmation(&self) -> bool { false }
    fn is_destructive(&self) -> bool { false }

    async fn execute(&self, input: Self::Input, ctx: &ToolContext) -> Result<Self::Output, ToolError> {
        let code = ctx.workspace.read_file(Path::new(&input.path)).await
            .map_err(|e| ToolError::Execution(e.to_string()))?;

        self.structured_llm.complete(&format!(
            "Analyze this code:\n```\n{}\n```",
            code
        )).await
            .map_err(|e| ToolError::Execution(e.to_string()))
    }
}
```

---

## Tool Registration Flow

```
XaftRuntime::bootstrap()
    │
    ├── Phase 8: Register Tools
    │
    │   ├── Workspace tools (read, write, edit, list, grep, tree)
    │   │   └── Each wrapped in HookedTool with:
    │   │       ├── AuditLogHook (after)
    │   │       ├── CacheCheckHook (before) / CacheStoreHook (after)
    │   │       └── PathSanitizationHook (before, for file tools)
    │   │
    │   ├── Git tools (status, diff, log, branch, commit, add, push, stash)
    │   │   └── Each wrapped in HookedTool with:
    │   │       ├── AuditLogHook (after)
    │   │       └── GitAutoCommitHook (after, for commit)
    │   │
    │   ├── Shell tools (bash_exec, cargo, npm)
    │   │   └── Each wrapped in HookedTool with:
    │   │       ├── AuditLogHook (after)
    │   │       ├── CommandSafetyGuardrail (before)
    │   │       └── RateLimitGuardrail (before)
    │   │
    │   ├── Search tools (semantic, symbol, file)
    │   │   └── Each wrapped in HookedTool with:
    │   │       ├── AuditLogHook (after)
    │   │       └── CacheCheckHook/CacheStoreHook
    │   │
    │   ├── MCP tools (loaded from config)
    │   │   └── Each registered as ErasedTool with:
    │   │       └── AuditLogHook (after)
    │   │
    │   └── Sub-agent tools (test_runner, code_reviewer, etc.)
    │       └── Each registered as SubagentTool<T>
    │
    ├── Convert all HookedTool<T> → ErasedTool
    │
    ├── Apply approval policy overrides
    │
    └── Register with AgentExecutor
```

---

## Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Tool denied: {0}")]
    Denied(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Timeout after {0}ms")]
    Timeout(u64),

    #[error("Cancelled")]
    Cancelled,

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("MCP error: {0}")]
    Mcp(String),
}
```
