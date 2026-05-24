# Future Roadmap

## 1. Overview

This document outlines the long-term technical roadmap for xaft, organized into
three horizons: Near-Term (0-6 months), Mid-Term (6-18 months), and Long-Term
(18+ months). Each item includes technical design sketches, research questions,
and dependency analysis.

```
┌──────────────────────────────────────────────────────────────────────┐
│                     xaft Roadmap Timeline                             │
│                                                                       │
│  Near-Term (0-6mo)          Mid-Term (6-18mo)       Long-Term (18mo+)│
│  ┌─────────────────┐       ┌─────────────────┐    ┌────────────────┐│
│  │ Container Sandbox│       │ Distributed Exec│    │ Multi-Repo     ││
│  │ WASM Plugins    │       │ Semantic Search │    │ CI/CD Platform ││
│  │ PR Creation     │       │ Multi-Repo Basis│    │ Background     ││
│  │ Background Agent│       │ CI/CD Integration│   │ Agent Marketplace│
│  └─────────────────┘       └─────────────────┘    └────────────────┘│
└──────────────────────────────────────────────────────────────────────┘
```

---

## 2. Near-Term: Container Sandboxing

### 2.1 Motivation

Currently, xaft executes shell commands directly on the host machine. While the
`require_approval_for: DestructiveOperations` policy prevents obvious damage,
a malicious or confused LLM could still execute destructive commands. Container
sandboxing provides defense-in-depth.

### 2.2 Design

```
┌──────────────────────────────────────────────────────────────────────┐
│                Container Sandboxing Architecture                     │
│                                                                       │
│  ┌──────────────────────┐                                            │
│  │  xaft Host Process   │                                            │
│  │                      │                                            │
│  │  ┌────────────────┐  │   ┌──────────────────────────────────┐   │
│  │  │ AgentExecutor  │  │   │  Sandbox Container               │   │
│  │  │                ├──┼──►│                                   │   │
│  │  │ tool calls →   │  │   │  ┌──────────┐  ┌─────────────┐  │   │
│  │  │                │  │   │  │ file ops │  │ shell_exec  │  │   │
│  │  │ ← results      │  │   │  │ (bind mount│  │ (network   │  │   │
│  │  │                │◄──┼───┤  │  project) │  │  isolated) │  │   │
│  │  └────────────────┘  │   │  └──────────┘  └─────────────┘  │   │
│  │                      │   │                                   │   │
│  │  SandboxManager:     │   │  Resource Limits:                 │   │
│  │  ├─ create_container │   │  ├─ CPU: 2 cores                  │   │
│  │  ├─ destroy_container│   │  ├─ Memory: 1GB                   │   │
│  │  ├─ snapshot         │   │  ├─ Disk: 5GB                     │   │
│  │  └─ restore_snapshot │   │  ├─ Network: outbound only        │   │
│  └──────────────────────┘   │  └─ PIDs: 100                     │   │
│                              └──────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────┘
```

### 2.3 Implementation Sketch

```rust
/// Sandbox backend abstraction — supports Docker, Podman, and native (no sandbox)
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Create a new sandboxed environment for a project
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxId, SandboxError>;

    /// Execute a command inside the sandbox
    async fn exec(&self, sandbox: &SandboxId, command: &str, cwd: &Path)
        -> Result<ExecResult, SandboxError>;

    /// Read a file from the sandbox
    async fn read_file(&self, sandbox: &SandboxId, path: &Path)
        -> Result<String, SandboxError>;

    /// Write a file to the sandbox
    async fn write_file(&self, sandbox: &SandboxId, path: &Path, content: &str)
        -> Result<(), SandboxError>;

    /// Create a filesystem snapshot (for rollback)
    async fn snapshot(&self, sandbox: &SandboxId) -> Result<SnapshotId, SandboxError>;

    /// Restore to a previous snapshot
    async fn restore(&self, sandbox: &SandboxId, snapshot: &SnapshotId)
        -> Result<(), SandboxError>;

    /// Destroy the sandbox
    async fn destroy(&self, sandbox: &SandboxId) -> Result<(), SandboxError>;
}

/// Docker-based sandbox implementation
pub struct DockerSandbox {
    client: Docker,
    image: String,
    runtime: Option<String>,  // e.g., "runsc" for gVisor
}

#[async_trait]
impl SandboxBackend for DockerSandbox {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxId, SandboxError> {
        let container = self.client
            .create_container(
                None,
                ContainerConfig {
                    image: Some(&self.image),
                    cmd: Some(vec!["sleep", "infinity"]),
                    working_dir: Some("/workspace"),
                    host_config: HostConfig {
                        binds: Some(vec![format!(
                            "{}:/workspace:rw",
                            config.project_path.display()
                        )]),
                        memory: Some(config.memory_limit_bytes()),
                        memory_swap: Some(config.memory_limit_bytes()),
                        cpu_period: Some(100_000),
                        cpu_quota: Some(config.cpu_quota()),
                        network_mode: Some(
                            config.network_policy.to_docker_mode()
                        ),
                        pids_limit: Some(config.max_pids as i64),
                        readonly_rootfs: Some(config.readonly_rootfs),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .map_err(SandboxError::Docker)?;

        self.client
            .start_container(&container.id, None)
            .await
            .map_err(SandboxError::Docker)?;

        Ok(SandboxId(container.id))
    }

    async fn exec(&self, sandbox: &SandboxId, command: &str, cwd: &Path)
        -> Result<ExecResult, SandboxError>
    {
        let exec = self.client
            .create_exec(
                &sandbox.0,
                ExecConfig {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    working_dir: Some(cwd.to_str().unwrap()),
                    cmd: Some(vec!["sh", "-c", command]),
                    ..Default::default()
                },
            )
            .await
            .map_err(SandboxError::Docker)?;

        let result = self.client
            .start_exec(&exec.id, None)
            .await
            .map_err(SandboxError::Docker)?;

        Ok(ExecResult {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code.unwrap_or(-1),
        })
    }

    async fn snapshot(&self, sandbox: &SandboxId) -> Result<SnapshotId, SandboxError> {
        // Use Docker commit to create an image snapshot
        let image = self.client
            .commit_container(
                &sandbox.0,
                CommitContainerOptions::builder()
                    .tag(format!("xaft-snapshot-{}", Uuid::new_v4()))
                    .build(),
            )
            .await
            .map_err(SandboxError::Docker)?;

        Ok(SnapshotId(image.id))
    }
}

/// Sandbox configuration
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub project_path: PathBuf,
    pub cpu_cores: f64,           // Max CPU cores
    pub memory_mb: u64,           // Max memory in MB
    pub disk_gb: u64,             // Max disk in GB
    pub max_pids: u32,            // Max process count
    pub network_policy: NetworkPolicy,
    pub readonly_rootfs: bool,    // Read-only root filesystem
    pub allowed_commands: Option<Vec<String>>,  // Whitelist of commands
}

#[derive(Debug, Clone)]
pub enum NetworkPolicy {
    /// No network access at all
    Isolated,
    /// Outbound only (for package installs, git push, etc.)
    OutboundOnly,
    /// Full network access
    Unrestricted,
}
```

---

## 3. Near-Term: WASM Plugin System

### 3.1 Motivation

xaft's core tool set (file operations, shell, git) covers 80% of use cases, but
users need domain-specific tools (database operations, Kubernetes management,
custom linting). A WASM plugin system allows third-party tools without compromising
xaft's memory safety guarantees.

### 3.2 Design

```
┌──────────────────────────────────────────────────────────────────────┐
│                    WASM Plugin Architecture                           │
│                                                                       │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  xaft Host                                                      │ │
│  │                                                                  │ │
│  │  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐      │ │
│  │  │ Built-in     │    │ WASM Runtime │    │ Host Functions│      │ │
│  │  │ Tools        │    │ (wasmtime)   │    │ (ABI)         │      │ │
│  │  │              │    │              │    │               │      │ │
│  │  │ file_read    │    │ ┌──────────┐│    │ xaft_log()    │      │ │
│  │  │ file_write   │    │ │ Plugin A ││◄──►│ xaft_signal() │      │ │
│  │  │ shell_exec   │    │ │ (k8s)    ││    │ xaft_config() │      │ │
│  │  │ git_commit   │    │ └──────────┘│    │ xaft_http()   │      │ │
│  │  │              │    │ ┌──────────┐│    │               │      │ │
│  │  │              │    │ │ Plugin B ││    │               │      │ │
│  │  │              │    │ │ (db)     ││    │               │      │ │
│  │  │              │    │ └──────────┘│    │               │      │ │
│  │  └──────────────┘    └──────────────┘    └──────────────┘      │ │
│  └────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.3 Implementation Sketch

```rust
/// WASM plugin interface
pub struct WasmPlugin {
    name: String,
    version: String,
    engine: Engine,
    instance: Instance,
    tool_definitions: Vec<ToolDefinition>,
}

impl WasmPlugin {
    pub fn load(path: &Path, config: &PluginConfig) -> Result<Self, PluginError> {
        let engine = Engine::new(&wasmtime::Config::new()
            .wasm_component_model(true)
            .consume_fuel(true)  // Enable fuel-based execution limiting
        )?;

        let module = Module::from_file(&engine, path)?;
        let linker = Linker::new(&engine);

        // Define host functions available to plugins
        linker.func_wrap("xaft", "log", |cx: Caller, msg_ptr: u32, msg_len: u32| {
            // Safe logging from plugin → host
        })?;

        linker.func_wrap("xaft", "signal", |cx: Caller, signal_ptr: u32| {
            // Allow plugin to emit signals to SignalBus
        })?;

        linker.func_wrap("xaft", "http_request", |cx: Caller, req_ptr: u32| -> u32 {
            // Allow plugin to make HTTP requests (with host permission check)
        })?;

        let store = Store::new(&engine, PluginState::new(config));
        let instance = linker.instantiate(&mut store, &module)?;

        // Call the plugin's init function to get tool definitions
        let init = instance.get_typed_func::<(), u32>(&mut store, "xaft_plugin_init")?;
        let defs_ptr = init.call(&mut store, ())?;

        let tool_definitions = Self::read_tool_definitions(&store, &instance, defs_ptr)?;

        Ok(Self {
            name: config.name.clone(),
            version: config.version.clone(),
            engine,
            instance,
            tool_definitions,
        })
    }

    /// Execute a tool from this plugin
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: Value,
        fuel_limit: u64,
    ) -> Result<Value, ToolError> {
        let mut store = Store::new(&self.engine, PluginState::new(&Default::default()));
        store.set_fuel(fuel_limit)?;

        let exec_fn = self.instance
            .get_typed_func::<(u32, u32), u32>(&mut store, "xaft_tool_exec")?;

        // Serialize params and pass to plugin
        let params_bytes = serde_json::to_vec(&params)?;
        let params_ptr = Self::allocate_in_plugin(&mut store, &self.instance, &params_bytes)?;

        let result_ptr = exec_fn.call(&mut store, (params_ptr, params_bytes.len() as u32))?;

        // Check if fuel was exhausted (infinite loop protection)
        let remaining_fuel = store.get_fuel()?;
        if remaining_fuel == 0 {
            return Err(ToolError::ExecutionFailed(
                "Plugin exceeded fuel limit (possible infinite loop)".to_string()
            ));
        }

        // Read result from plugin memory
        let result = Self::read_result(&store, &self.instance, result_ptr)?;
        Ok(result)
    }
}

/// Plugin ABI — the interface that WASM plugins implement
///
/// ```wit (WebAssembly Interface Types)
/// package xaft:plugin;
///
/// interface host {
///     log: func(level: string, message: string);
///     signal: func(signal-json: string);
///     http-request: func(method: string, url: string, body: option<string>) -> result<string>;
///     read-config: func(key: string) -> option<string>;
/// }
///
/// interface plugin {
///     init: func() -> list<tool-definition>;
///     execute-tool: func(tool-name: string, params: string) -> result<string>;
///     shutdown: func();
/// }
///
/// world xaft-plugin {
///     import host;
///     export plugin;
/// }
/// ```
```

### 3.4 Plugin Manifest

```toml
# xaft-plugin.toml
[plugin]
name = "xaft-k8s"
version = "0.1.0"
description = "Kubernetes management tools for xaft"
author = "xaft-community"
min_xaft_version = "0.2.0"

[permissions]
network = ["*.kubernetes.default", "*.kubernetes.default.svc"]
filesystem = []  # No direct filesystem access
signals = ["tool.*", "agent.lifecycle"]

[[tools]]
name = "k8s_apply"
description = "Apply a Kubernetes manifest"
parameters = """
{ "type": "object", "properties": {
    "manifest_path": { "type": "string", "description": "Path to manifest YAML" },
    "namespace": { "type": "string", "default": "default" }
}, "required": ["manifest_path"] }
"""

[[tools]]
name = "k8s_rollout_status"
description = "Check rollout status of a deployment"
parameters = """
{ "type": "object", "properties": {
    "deployment": { "type": "string" },
    "namespace": { "type": "string", "default": "default" }
}, "required": ["deployment"] }
"""
```

---

## 4. Mid-Term: Distributed Execution

### 4.1 Motivation

For large-scale tasks (multi-repo refactors, organization-wide dependency updates),
a single xaft instance is insufficient. Distributed execution allows multiple xaft
workers to collaborate on a shared task queue.

### 4.2 Design

```
┌──────────────────────────────────────────────────────────────────────┐
│              Distributed xaft Architecture                            │
│                                                                       │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │                     Coordinator Node                            │ │
│  │                                                                  │ │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐      │ │
│  │  │  Task    │  │  Result  │  │  Signal  │  │  Budget  │      │ │
│  │  │  Queue   │  │  Aggregator│  │  Router  │  │  Pool   │      │ │
│  │  └─────┬────┘  └─────┬────┘  └─────┬────┘  └─────┬────┘      │ │
│  │        │              │              │              │            │ │
│  └────────┼──────────────┼──────────────┼──────────────┼────────────┘ │
│           │              │              │              │              │
│     ┌─────▼──────┐ ┌────▼──────┐ ┌────▼──────┐ ┌────▼──────┐      │
│     │  Worker 1  │ │  Worker 2  │ │  Worker 3  │ │  Worker N │      │
│     │  (xaft)    │ │  (xaft)    │ │  (xaft)    │ │  (xaft)   │      │
│     │            │ │            │ │            │ │           │      │
│     │  Agent 1   │ │  Agent 1   │ │  Agent 1   │ │  Agent 1  │      │
│     │  Agent 2   │ │  Agent 2   │ │  Agent 2   │ │  Agent 2  │      │
│     └────────────┘ └────────────┘ └────────────┘ └──────────┘      │
└──────────────────────────────────────────────────────────────────────┘
```

### 4.3 Implementation Sketch

```rust
/// Coordinator manages task distribution across workers
pub struct DistributedCoordinator {
    task_queue: Arc<TaskQueue>,
    result_aggregator: Arc<ResultAggregator>,
    signal_router: Arc<SignalRouter>,
    budget_pool: Arc<BudgetPool>,
    workers: Vec<WorkerConnection>,
}

/// Task queue backed by a persistent store (Redis, SQLite, or in-memory)
#[async_trait]
pub trait TaskQueue: Send + Sync {
    /// Enqueue a task for distribution
    async fn enqueue(&self, task: DistributedTask) -> Result<TaskId, QueueError>;

    /// Claim the next available task (called by workers)
    async fn claim(&self, worker_id: &WorkerId) -> Result<Option<DistributedTask>, QueueError>;

    /// Report task completion
    async fn complete(&self, task_id: &TaskId, result: TaskResult) -> Result<(), QueueError>;

    /// Report task failure
    async fn fail(&self, task_id: &TaskId, error: TaskError) -> Result<(), QueueError>;

    /// Get queue depth
    async fn depth(&self) -> Result<usize, QueueError>;
}

/// A distributed task with dependency tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedTask {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub prompt: String,
    pub working_directory: PathBuf,
    pub required_tools: Vec<String>,
    pub dependencies: Vec<TaskId>,    // Must complete before this task starts
    pub priority: TaskPriority,
    pub budget_limit: f64,
    pub assigned_worker: Option<WorkerId>,
    pub status: TaskStatus,
}

/// Worker connection to the coordinator
pub struct WorkerConnection {
    id: WorkerId,
    address: SocketAddr,
    capabilities: WorkerCapabilities,
    current_load: f64,
    last_heartbeat: Instant,
}

/// Worker capabilities advertised to coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCapabilities {
    pub max_concurrent_tasks: usize,
    pub available_models: Vec<String>,
    pub available_tools: Vec<String>,
    pub memory_mb: u64,
    pub cpu_cores: u32,
    pub sandbox_support: bool,
    pub labels: HashMap<String, String>,  // For task matching (e.g., "os=linux")
}

/// Budget pool shared across distributed workers
pub struct BudgetPool {
    total_budget: AtomicF64,
    allocated: Mutex<HashMap<WorkerId, f64>>,
    rate_limiter: RateLimiter,
}

impl BudgetPool {
    /// Request budget allocation for a worker
    pub async fn allocate(&self, worker_id: &WorkerId, amount: f64) -> Result<(), BudgetError> {
        let remaining = self.total_budget.load(Ordering::SeqCst);
        if remaining < amount {
            return Err(BudgetError::InsufficientFunds { requested: amount, remaining });
        }
        self.total_budget.fetch_sub(amount, Ordering::SeqCst);
        self.allocated.lock().await.insert(worker_id.clone(), amount);
        Ok(())
    }

    /// Return unused budget from a worker
    pub async fn release(&self, worker_id: &WorkerId, used: f64) {
        let allocated = self.allocated.lock().await.remove(worker_id).unwrap_or(0.0);
        let returned = allocated - used;
        if returned > 0.0 {
            self.total_budget.fetch_add(returned, Ordering::SeqCst);
        }
    }
}
```

---

## 5. Mid-Term: Semantic Search

### 5.1 Motivation

As codebases grow, LLM context windows become insufficient for providing full file
contents. Semantic search allows xaft to find relevant code without loading entire
files into context.

### 5.2 Design

```rust
/// Semantic search engine for codebase understanding
pub struct SemanticSearchEngine {
    embedder: Box<dyn Embedder>,
    index: VectorIndex,
    chunker: CodeChunker,
    cache: EmbeddingCache,
}

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Generate embeddings for a batch of text chunks
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// The dimension of the embedding vectors
    fn dimension(&self) -> usize;

    /// The model identifier
    fn model_name(&self) -> &str;
}

/// Code-aware chunking that preserves semantic boundaries
pub struct CodeChunker {
    max_chunk_tokens: usize,
    overlap_tokens: usize,
    language_configs: HashMap<String, LanguageConfig>,
}

impl CodeChunker {
    pub fn chunk_file(&self, path: &Path, content: &str) -> Vec<CodeChunk> {
        let language = self.detect_language(path);
        let config = self.language_configs.get(&language)
            .unwrap_or(&LanguageConfig::default());

        let tree = config.parse(content);
        let mut chunks = Vec::new();

        // Split at semantic boundaries (function, class, impl blocks)
        for node in tree.root_node().children(&mut tree.walk()) {
            if config.is_chunk_boundary(&node) {
                let text = &content[node.byte_range()];
                if text.len() > self.max_chunk_tokens * 4 {
                    // Further split large functions
                    chunks.extend(self.split_large_node(&node, content, config));
                } else {
                    chunks.push(CodeChunk {
                        path: path.to_path_buf(),
                        content: text.to_string(),
                        kind: node.kind().to_string(),
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        embedding: None,
                    });
                }
            }
        }

        chunks
    }
}

/// A semantically meaningful chunk of code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub path: PathBuf,
    pub content: String,
    pub kind: String,          // "function", "struct", "impl", "module"
    pub start_line: usize,
    pub end_line: usize,
    pub embedding: Option<Vec<f32>>,
}

/// Vector index for similarity search
pub struct VectorIndex {
    storage: Box<dyn VectorStorage>,
    dimension: usize,
}

#[async_trait]
pub trait VectorStorage: Send + Sync {
    /// Insert vectors with metadata
    async fn insert(&self, id: &str, vector: &[f32], metadata: Value) -> Result<(), StorageError>;

    /// Search for nearest neighbors
    async fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchResult>, StorageError>;

    /// Delete vectors by metadata filter
    async fn delete_by_filter(&self, filter: Value) -> Result<usize, StorageError>;

    /// Count total indexed vectors
    async fn count(&self) -> Result<usize, StorageError>;
}
```

---

## 6. Mid-Term: CI/CD Integration

### 6.1 Design

```rust
/// CI/CD integration modes
pub enum CiMode {
    /// Run as a GitHub Action
    GitHubAction {
        token: String,
        repository: String,
        event: GitHubEvent,
    },
    /// Run as a GitLab CI job
    GitLabCi {
        token: String,
        project_id: String,
        merge_request_iid: u64,
    },
    /// Run as a standalone CLI in any CI environment
    Standalone {
        vcs: VcsBackend,
        output_format: CiOutputFormat,
    },
}

/// CI-specific execution configuration
pub struct CiConfig {
    pub mode: CiMode,
    /// Auto-approve non-destructive operations
    pub auto_approve: bool,
    /// Fail the CI job if any issues are found (review mode)
    pub fail_on_issues: bool,
    /// Only run review, don't apply fixes
    pub review_only: bool,
    /// Maximum budget for this CI run
    pub budget_limit: f64,
    /// Report format
    pub report_format: ReportFormat,
}

/// CI execution result
pub struct CiResult {
    pub exit_code: i32,
    pub findings: Vec<Finding>,
    pub fixes_applied: Vec<FileChange>,
    pub cost: f64,
    pub report: String,
}

impl CiResult {
    /// Generate GitHub Actions annotation format
    pub fn to_github_annotations(&self) -> String {
        let mut output = String::new();
        for finding in &self.findings {
            let level = match finding.severity {
                Severity::Critical | Severity::High => "error",
                Severity::Medium => "warning",
                Severity::Low | Severity::Info => "notice",
            };
            output.push_str(&format!(
                "::{} file={},line={}::{}\n",
                level, finding.file, finding.line, finding.message
            ));
        }
        output
    }
}
```

---

## 7. Long-Term: Multi-Repository Support

### 7.1 Design

```rust
/// Multi-repository workspace
pub struct MultiRepoWorkspace {
    repositories: HashMap<RepoId, Repository>,
    cross_repo_index: CrossRepoIndex,
    change_coordinator: ChangeCoordinator,
}

/// A repository within the multi-repo workspace
pub struct Repository {
    id: RepoId,
    path: PathBuf,
    vcs: GitRepo,
    language: DetectedLanguage,
    dependencies: Vec<Dependency>,
    dependents: Vec<RepoId>,  // Other repos that depend on this one
}

/// Coordinates changes across repositories
pub struct ChangeCoordinator {
    /// Dependency graph between repositories
    dep_graph: PetGraph<RepoId, DependencyEdge>,
    /// Change propagation strategy
    propagation: PropagationStrategy,
}

pub enum PropagationStrategy {
    /// Apply changes to all repos simultaneously
    Parallel,
    /// Apply changes respecting dependency order
    Sequential,
    /// Apply to leaf repos first, then propagate inward
    OutsideIn,
}

impl ChangeCoordinator {
    /// Plan a cross-repo change, determining the order of modifications
    pub fn plan_change(&self, change: &CrossRepoChange) -> ChangePlan {
        let affected_repos = self.find_affected_repos(change);
        let sorted = self.topological_sort(&affected_repos);

        ChangePlan {
            steps: sorted.iter().map(|repo_id| {
                ChangeStep {
                    repo_id: *repo_id,
                    changes: change.changes_for(*repo_id),
                    depends_on: sorted.iter()
                        .filter(|&&id| self.depends_on(*repo_id, id))
                        .cloned()
                        .collect(),
                }
            }).collect(),
            propagation: self.propagation.clone(),
        }
    }
}
```

---

## 8. Long-Term: Background Agents

### 8.1 Design

```rust
/// Background agent that runs independently and notifies on completion
pub struct BackgroundAgent {
    agent: Agent,
    notification_sink: Box<dyn NotificationSink>,
    persistence: Box<dyn AgentPersistence>,
    schedule: Option<AgentSchedule>,
}

/// Notification channels for background agents
#[async_trait]
pub trait NotificationSink: Send + Sync {
    async fn notify(&self, event: BackgroundEvent) -> Result<(), NotificationError>;
}

/// Slack notification implementation
pub struct SlackNotifier {
    webhook_url: String,
    channel: String,
}

#[async_trait]
impl NotificationSink for SlackNotifier {
    async fn notify(&self, event: BackgroundEvent) -> Result<(), NotificationError> {
        let message = match &event {
            BackgroundEvent::Completed { task, result } => {
                format!(
                    "✅ Task '{}' completed. {} files modified. Cost: ${:.4}",
                    task, result.modified_files.len(), result.cost
                )
            }
            BackgroundEvent::Failed { task, error } => {
                format!("❌ Task '{}' failed: {}", task, error)
            }
            BackgroundEvent::BudgetWarning { task, percentage } => {
                format!("⚠️ Task '{}' has used {:.0}% of budget", task, percentage)
            }
            BackgroundEvent::AwaitingApproval { task, reason } => {
                format!("🔐 Task '{}' requires approval: {}", task, reason)
            }
        };

        self.send_slack_message(&message).await
    }
}

/// Agent scheduling for recurring tasks
pub enum AgentSchedule {
    /// Run on a cron schedule
    Cron { expression: String },
    /// Run when a file changes
    OnFileChange { paths: Vec<String>, debounce: Duration },
    /// Run when a PR is opened
    OnPullRequest { branches: Vec<String> },
    /// Run on CI failure
    OnCiFailure { branches: Vec<String> },
}
```

---

## 9. Open Research Questions

### 9.1 Agent Planning Reliability

**Question**: How can we ensure that LLM-generated plans are reliable and complete?

Current approaches have known failure modes:
- Plans that omit necessary steps
- Plans with incorrect dependency ordering
- Plans that don't account for error paths

**Research Directions**:
- Plan validation via symbolic execution of the plan steps
- Plan refinement through iterative self-critique
- Formal verification of plan completeness using type-state patterns

```rust
/// Potential approach: Type-state validated plans
/// Each step has preconditions and postconditions encoded as type states

struct Plan<S: PlanState> {
    steps: Vec<PlanStep>,
    _state: PhantomData<S>,
}

// Compile-time enforcement: can't execute before validation
impl Plan<Draft> {
    fn validate(self) -> Result<Plan<Validated>, PlanValidationError> {
        // Check step dependencies, verify preconditions
        // ...
        Ok(Plan { steps: self.steps, _state: PhantomData })
    }
}

impl Plan<Validated> {
    async fn execute(self) -> Result<Plan<Completed>, ExecutionError> {
        // Only validated plans can be executed
        // ...
    }
}
```

### 9.2 Optimal Context Window Management

**Question**: How should xaft select which context to include in each LLM call
when the codebase exceeds the context window?

**Research Directions**:
- Retrieval-Augmented Generation (RAG) for code
- Attention-based context ranking
- Dynamic context compression (summarize old turns)

### 9.3 Multi-Agent Coordination

**Question**: What is the optimal coordination protocol for multi-agent systems?

Current coordination patterns (delegation, broadcast, pipeline) each have tradeoffs.
Open questions include:
- How to prevent conflicting concurrent edits?
- How to handle agent failures in a delegation chain?
- How to share context efficiently between agents?

```rust
/// Potential approach: CRDT-based conflict resolution for concurrent edits
pub trait ConcurrentEditResolution: Send + Sync {
    /// Merge two concurrent edits to the same file
    fn merge(&self, base: &str, edit_a: &FileEdit, edit_b: &FileEdit)
        -> Result<MergedEdit, ConflictError>;
}

/// Operational Transform for concurrent text editing
pub struct OperationalTransform;

impl ConcurrentEditResolution for OperationalTransform {
    fn merge(&self, base: &str, edit_a: &FileEdit, edit_b: &FileEdit)
        -> Result<MergedEdit, ConflictError>
    {
        let ops_a = edit_a.to_operations(base);
        let ops_b = edit_b.to_operations(base);

        // Transform operations against each other
        let (transformed_a, transformed_b) = Self::transform(&ops_a, &ops_b);

        // Apply in order
        let merged = Self::compose(&transformed_a, &transformed_b);
        Ok(MergedEdit { operations: merged })
    }
}
```

### 9.4 Cost-Optimal Model Selection

**Question**: How should xaft dynamically choose between models (e.g., Haiku vs Sonnet
vs Opus) to minimize cost while maintaining quality?

**Research Directions**:
- Quality-cost Pareto frontier estimation
- Progressive model escalation (start cheap, escalate if needed)
- Task complexity prediction for model routing

### 9.5 Formal Verification of Agent Behavior

**Question**: Can we formally verify that an agent will never perform certain
dangerous actions (e.g., deleting `.git`, pushing to main)?

**Research Directions**:
- Temporal logic specifications for agent policies
- Model checking of agent state machines
- Proof-carrying tool definitions

```rust
/// Potential approach: Policy DSL with formal verification
///
/// policy safe_agent {
///     // Temporal: "globally, it is never the case that..."
///     never {
///         file_delete(path: ".git/**");
///         git_push(branch: "main");
///         shell_exec(command: /rm -rf/);
///     }
///
///     // Temporal: "eventually, if an edit is made, compilation is checked"
///     always implies {
///         file_write(_, _) => eventually { shell_exec("cargo check") };
///     }
///
///     // Budget constraint
///     constraint budget_limit: f64 = 5.0;
/// }
```

---

## 10. Roadmap Dependency Graph

```
┌──────────────────────────────────────────────────────────────────────┐
│                     Feature Dependency Graph                          │
│                                                                       │
│  Container Sandbox ─────────────────────────┐                        │
│       │                                      │                        │
│       ▼                                      ▼                        │
│  Background Agents ───────► Distributed Execution                    │
│       │                                      │                        │
│       ▼                                      ▼                        │
│  CI/CD Integration ◄───────────────── Multi-Repo Support             │
│       │                                      ▲                        │
│       ▼                                      │                        │
│  PR Creation ◄──────── Semantic Search ──────┘                        │
│                                                                       │
│  WASM Plugins ───────────────────► Agent Marketplace                  │
│                                                                       │
│  Legend:                                                              │
│  A ──► B means "A is a prerequisite for B"                            │
│  Horizontal = same horizon level                                      │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 11. Version Milestones

| Version | Timeline | Key Features |
|---------|----------|-------------|
| v0.1.0 | Month 0 | Core agent loop, #[agent]/#[tool] macros, file/shell/git tools |
| v0.2.0 | Month 2 | Plan-and-execute mode, TUI dashboard, cost tracking |
| v0.3.0 | Month 4 | Multi-agent delegation, container sandboxing |
| v0.4.0 | Month 6 | WASM plugin system, background agents |
| v0.5.0 | Month 9 | Semantic search, CI/CD integration |
| v0.6.0 | Month 12 | Distributed execution, PR creation |
| v0.7.0 | Month 15 | Multi-repo support, agent marketplace |
| v1.0.0 | Month 18 | Stable API, production-ready, full feature set |

This roadmap is ambitious but grounded in xaft's architectural advantages. The Rust
foundation, compile-time validation, and transactional editing primitives provide
a solid base for each evolution step.
