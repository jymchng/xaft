# XAFT Plugin System — Product Requirements Document

> **Status**: Draft v0.1  
> **Last Updated**: 2025-03-04  
> **Authors**: xaft core team  
> **Scope**: Plugin architecture, runtime registration, compile-time inventory, dynamic loading, WASM roadmap, manifest format, API surface, safety boundaries

---

## 1. Overview

xaft is an autonomous coding CLI built on the `agtrs` Rust framework. Its power comes from composability: every tool, agent, guardrail, and provider is a pluggable component. This PRD defines the plugin system that allows third parties to extend xaft without modifying core.

### 1.1 Goals

| # | Goal | Metric |
|---|------|--------|
| G1 | Zero-cost abstraction for built-in plugins | No measurable overhead vs. hardcoded path |
| G2 | Third-party Rust plugins via dynamic libraries | Load in <50 ms, no rebuild of xaft |
| G3 | Sandboxed WASM plugins (v2) | Sub-10 ms cold start, <5 MB memory cap |
| G4 | Safe API surface with capability boundaries | No plugin can access filesystem without declared cap |
| G5 | Compile-time inventory for default plugins | No runtime lookup for core tool set |

### 1.2 Non-Goals

- A visual plugin marketplace (future scope)
- Hot-reloading of plugins during a running session
- Plugin support for non-Rust languages (beyond WASM)
- Cross-process plugin isolation (plugins share the process address space in v1)

---

## 2. Architecture

### 2.1 High-Level Component Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                         XAFT PROCESS                                │
│                                                                     │
│  ┌─────────────┐    ┌──────────────┐    ┌──────────────────────┐   │
│  │  Plugin      │    │  Plugin      │    │  Plugin              │   │
│  │  Registry    │◄───┤  Loader      │◄───┤  Resolver           │   │
│  │  (inventory) │    │  (dylib/wasm)│    │  (manifest parsing) │   │
│  └──────┬───────┘    └──────┬───────┘    └──────────┬───────────┘   │
│         │                   │                       │               │
│         ▼                   ▼                       ▼               │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │                    Plugin Dispatcher                         │   │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────────┐   │   │
│  │  │  Tool    │ │  Agent   │ │ Guardrail│ │  Provider    │   │   │
│  │  │  Slot    │ │  Slot    │ │  Slot    │ │  Slot        │   │   │
│  │  └──────────┘ └──────────┘ └──────────┘ └──────────────┘   │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                              │                                      │
│                              ▼                                      │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │               Capability Guard (seccomp/caps)               │   │
│  └──────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

### 2.2 Plugin Kinds

Every plugin declares one or more **kinds** in its manifest:

| Kind | Trait Bound | Description |
|------|-------------|-------------|
| `tool` | `xaft_plugin::Tool` | Extends the tool palette available to agents |
| `agent` | `xaft_plugin::Agent` | Provides a complete autonomous agent loop |
| `guardrail` | `xaft_plugin::Guardrail` | Intercepts and validates actions before execution |
| `provider` | `xaft_plugin::Provider` | Supplies an LLM/chat completion backend |

### 2.3 Registration Flow

```
  Compile Time                  Runtime
  ───────────                  ───────
  inventory::collect!()        PluginResolver::discover()
       │                            │
       ▼                            ▼
  BUILT_IN_PLUGINS[]           Manifest::parse(toml)
       │                            │
       └──────────┬─────────────────┘
                  ▼
         PluginRegistry::register_all()
                  │
                  ├─► slot.tool = Arc<dyn Tool>
                  ├─► slot.agent = Arc<dyn Agent>
                  ├─► slot.guardrail = Arc<dyn Guardrail>
                  └─► slot.provider = Arc<dyn Provider>
```

---

## 3. Core Traits

### 3.1 Base Plugin Trait

```rust
/// Every plugin must implement `Plugin`, which provides metadata
/// and the kind(s) it satisfies.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Unique reverse-DNS identifier: "com.example.xaft-my-tool"
    fn id(&self) -> &str;

    /// Human-readable name shown in TUI.
    fn name(&self) -> &str;

    /// Semantic version (must be semver-compatible for dependency resolution).
    fn version(&self) -> &Version;

    /// Declared capability requirements.
    fn capabilities(&self) -> &[Capability];

    /// The plugin kinds this instance satisfies.
    fn kinds(&self) -> &[PluginKind];

    /// Lifecycle hook: called once after loading, before any dispatch.
    async fn initialize(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Lifecycle hook: called on graceful shutdown.
    async fn teardown(&self) -> Result<(), PluginError> { Ok(()) }
}
```

### 3.2 Tool Plugin Trait

```rust
#[async_trait]
pub trait Tool: Plugin {
    /// JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> &serde_json::Value;

    /// JSON Schema for the tool's output.
    fn output_schema(&self) -> &serde_json::Value;

    /// Human description injected into the system prompt.
    fn description(&self) -> &str;

    /// Execute the tool. The `ToolCall` carries the agent's arguments.
    async fn execute(
        &self,
        call: ToolCall,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;

    /// Optional: suggest follow-up tools the agent might want.
    fn suggest_next(&self, _output: &ToolOutput) -> Vec<ToolSuggestion> {
        vec![]
    }
}
```

### 3.3 Agent Plugin Trait

```rust
#[async_trait]
pub trait Agent: Plugin {
    /// The system prompt template for this agent.
    fn system_prompt(&self) -> &str;

    /// Tools this agent is permitted to use (by plugin ID).
    fn allowed_tools(&self) -> &[String];

    /// Maximum reasoning turns before forced stop.
    fn max_turns(&self) -> u32 { 25 }

    /// Run one agent iteration. Called by the xaft runtime loop.
    async fn step(
        &self,
        state: &AgentState,
        bus: &AgentMessageBus,
    ) -> Result<AgentAction, AgentError>;
}
```

### 3.4 Guardrail Plugin Trait

```rust
#[async_trait]
pub trait Guardrail: Plugin {
    /// Called before a tool executes. Return `Deny` to block.
    async fn check_tool_call(
        &self,
        call: &ToolCall,
        ctx: &GuardrailContext,
    ) -> GuardrailVerdict;

    /// Called before an agent sends a message. Return `Deny` to suppress.
    async fn check_message(
        &self,
        msg: &AgentMessage,
        ctx: &GuardrailContext,
    ) -> GuardrailVerdict;

    /// Priority: lower = runs first. Built-in guardrails run at 0-99.
    fn priority(&self) -> i32 { 100 }
}

pub enum GuardrailVerdict {
    Allow,
    Deny { reason: String },
    Modify { replacement: ToolCall },  // rewrite dangerous args
}
```

### 3.5 Provider Plugin Trait

```rust
#[async_trait]
pub trait Provider: Plugin {
    /// Model IDs this provider serves (e.g. "gpt-4o", "claude-3.5-sonnet").
    fn model_ids(&self) -> &[ModelId];

    /// Perform a chat completion request.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError>;

    /// Stream a chat completion.
    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Box<dyn Stream<Item = CompletionChunk>>, ProviderError>;

    /// Return token counting for cost estimation.
    fn count_tokens(&self, text: &str, model: &ModelId) -> usize;
}
```

---

## 4. Compile-Time Registration with `inventory`

### 4.1 Why inventory?

The `inventory` crate allows plugins compiled into the xaft binary to self-register without central module enumeration. Each built-in plugin submits a struct at link time.

```rust
// in xaft-plugin-derive/src/lib.rs
pub struct PluginEntry {
    pub factory: fn() -> Box<dyn Plugin>,
    pub kind: PluginKind,
}

inventory::collect!(PluginEntry);

// Macro for ergonomic registration
#[macro_export]
macro_rules! register_plugin {
    ($ty:ty, $kind:expr) => {
        inventory::submit! {
            $crate::PluginEntry {
                factory: || Box::new(<$ty>::default()),
                kind: $kind,
            }
        }
    };
}
```

### 4.2 Usage in Built-in Plugins

```rust
// xaft-builtin-tools/src/file_read.rs
pub struct FileReadTool;

register_plugin!(FileReadTool, PluginKind::Tool);

#[async_trait]
impl Tool for FileReadTool {
    fn input_schema(&self) -> &serde_json::Value {
        static SCHEMA: Lazy<serde_json::Value> = Lazy::new(|| {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute file path" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"]
            })
        });
        &SCHEMA
    }
    // ... execute, output_schema, description
}
```

### 4.3 Registry Construction

```rust
pub fn build_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();

    // Phase 1: Collect inventory (compile-time plugins)
    for entry in inventory::iter::<PluginEntry> {
        let plugin = (entry.factory)();
        registry.register(plugin, Source::BuiltIn).expect("built-in registration failed");
    }

    // Phase 2: Discover and load dynamic plugins
    let dylib_plugins = PluginLoader::discover(PluginSearchPath::default());
    for plugin in dylib_plugins {
        registry.register(plugin, Source::Dynamic).expect("dynamic registration failed");
    }

    registry
}
```

---

## 5. Dynamic Library Loading

### 5.1 Loading Strategy

```
  ┌──────────────────┐
  │ Plugin Resolver   │
  │  discovers:      │
  │  ~/.xaft/plugins/│
  │  ./xaft-plugins/ │
  │  XAFT_PLUGIN_DIR │
  └────────┬─────────┘
           │
           ▼
  ┌──────────────────┐     ┌──────────────────────┐
  │ Manifest Parse   │────►│ xaft-plugin.toml     │
  │ (per directory)  │     │ kind, id, version,   │
  └────────┬─────────┘     │ entrypoint, caps     │
           │               └──────────────────────┘
           ▼
  ┌──────────────────┐
  │ Safety Check     │
  │ - Cap boundary   │
  │ - Version compat │
  │ - Signature (v2) │
  └────────┬─────────┘
           │
           ▼
  ┌──────────────────┐
  │ dlopen + dlsym   │
  │ _xaft_plugin_new │
  └────────┬─────────┘
           │
           ▼
  ┌──────────────────┐
  │ PluginRegistry   │
  │ .register()      │
  └──────────────────┘
```

### 5.2 Dynamic Plugin Entrypoint Convention

Every `.so` / `.dylib` / `.dll` must export:

```rust
#[no_mangle]
pub extern "C" fn _xaft_plugin_new() -> *mut dyn Plugin {
    let plugin: Box<dyn Plugin> = Box::new(MyPlugin::new());
    Box::into_raw(plugin)
}

#[no_mangle]
pub extern "C" fn _xaft_plugin_version() -> u32 {
    // Encoded as (major << 16) | (minor << 8) | patch
    0x00_01_00
}

#[no_mangle]
pub extern "C" fn _xaft_plugin_free(ptr: *mut dyn Plugin) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)); }
    }
}
```

### 5.3 ABI Stability

| Component | Strategy |
|-----------|----------|
| `Plugin` trait | Versioned vtable via `abi_stable` crate |
| Data types | `#[repr(C)]` FFI-safe wrappers around Rust types |
| Error propagation | C-compatible error code + string message |
| Async runtime | Plugins receive `&Handle` (tokio handle); no runtime creation |

```rust
// Stable vtable for Tool plugins (v1)
#[repr(C)]
pub struct ToolVTable {
    pub id: extern "C" fn(*const c_void) -> StableStr,
    pub input_schema: extern "C" fn(*const c_void) -> StableJson,
    pub output_schema: extern "C" fn(*const c_void) -> StableJson,
    pub description: extern "C" fn(*const c_void) -> StableStr,
    pub execute: extern "C" fn(
        *const c_void,   // self
        *const ToolCallFFI,
        *const ToolContextFFI,
        *mut ToolOutputFFI,  // out-parameter
    ) -> i32,  // 0 = Ok
}
```

---

## 6. Plugin Manifest Format

Each plugin directory must contain `xaft-plugin.toml`:

```toml
[plugin]
id = "com.example.xaft-docker-tool"
name = "Docker Tool"
version = "1.2.0"
description = "Run and manage Docker containers from xaft agents"
authors = ["Jane Doe <jane@example.com>"]
homepage = "https://github.com/example/xaft-docker-tool"
license = "MIT"
min_xaft_version = "0.5.0"

[plugin.entrypoint]
# For dynamic libraries:
type = "dylib"
path = "libxaft_docker_tool.so"
# For WASM (future):
# type = "wasm"
# path = "docker_tool.wasm"

[plugin.capabilities]
fs_read = ["${PROJECT_ROOT}/**", "/tmp/**"]
fs_write = ["${PROJECT_ROOT}/docker/**"]
network = true
shell = ["docker", "docker-compose"]
env_read = ["DOCKER_HOST"]
env_write = []

[plugin.dependencies]
# Other plugin IDs that must be loaded first
requires = ["com.example.xaft-shell-tool"]
# Soft dependencies — loaded if available
recommends = ["com.example.xaft-k8s-tool"]

[plugin.kind.tool]
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"

[plugin.kind.agent]  # Optional: plugin can be multi-kind
system_prompt_file = "prompts/docker-agent.md"
allowed_tools = ["com.example.xaft-docker-tool"]
max_turns = 15
```

### 6.1 Manifest Validation

```rust
pub fn validate_manifest(dir: &Path) -> Result<ValidatedManifest, ManifestError> {
    let raw: RawManifest = toml::from_str(&fs::read_to_string(dir.join("xaft-plugin.toml"))?)?;

    // 1. Required fields
    ensure!(!raw.plugin.id.is_empty(), ManifestError::MissingField("id"));
    ensure!(raw.plugin.version.parse::<Version>().is_ok(), ManifestError::InvalidVersion);

    // 2. ID format: reverse-DNS
    let id_parts: Vec<_> = raw.plugin.id.split('.').collect();
    ensure!(id_parts.len() >= 3, ManifestError::InvalidIdFormat);

    // 3. Capability validation
    for (cap, patterns) in &raw.plugin.capabilities.fs_read {
        for pat in patterns {
            ensure!(glob::Pattern::new(pat).is_ok(), ManifestError::InvalidGlob(pat.clone()));
        }
    }

    // 4. Entrypoint existence
    let entry = dir.join(&raw.plugin.entrypoint.path);
    ensure!(entry.exists(), ManifestError::EntrypointNotFound(entry));

    // 5. Schema files exist
    if let Some(tool) = &raw.plugin.kind.tool {
        let schema = dir.join(&tool.input_schema);
        ensure!(schema.exists(), ManifestError::SchemaMissing(schema));
    }

    Ok(ValidatedManifest { raw, base_dir: dir.to_path_buf() })
}
```

---

## 7. API Access and Plugin Context

### 7.1 PluginContext

Every plugin receives a `PluginContext` during initialization:

```rust
pub struct PluginContext {
    /// Access the xaft key-value store for persistent plugin state.
    pub kv: Arc<dyn KvStore>,

    /// Subscribe to system events (task start/stop, file changes, etc.).
    pub events: EventSubscriber,

    /// Spawn background tasks on the xaft tokio runtime.
    pub runtime: Handle,

    /// Access declared environment variables.
    pub env: EnvAccess,

    /// Logger scoped to this plugin's ID.
    pub log: PluginLogger,

    /// Project root path (resolved at startup).
    pub project_root: PathBuf,
}
```

### 7.2 Capability Enforcement

```rust
pub struct CapabilityGuard {
    caps: HashSet<Capability>,
    fs_read_patterns: Vec<glob::Pattern>,
    fs_write_patterns: Vec<glob::Pattern>,
    allowed_shells: Vec<String>,
    network: bool,
}

impl CapabilityGuard {
    pub fn check_fs_read(&self, path: &Path) -> Result<(), CapabilityError> {
        let canonical = path.canonicalize()?;
        if self.fs_read_patterns.iter().any(|p| p.matches_path(&canonical)) {
            Ok(())
        } else {
            Err(CapabilityError::FsReadDenied(canonical))
        }
    }

    pub fn check_shell(&self, cmd: &str) -> Result<(), CapabilityError> {
        let base = cmd.split_whitespace().next().unwrap_or(cmd);
        if self.allowed_shells.iter().any(|s| s == base) {
            Ok(())
        } else {
            Err(CapabilityError::ShellDenied(base.to_string()))
        }
    }
}
```

The `ToolContext` passed to tool execution wraps the `CapabilityGuard`:

```rust
pub struct ToolContext {
    guard: Arc<CapabilityGuard>,
    project_root: PathBuf,
    worktree: Arc<WorktreeGuard>,
    session_id: SessionId,
}

impl ToolContext {
    pub fn read_file(&self, path: &Path) -> Result<String, ToolError> {
        self.guard.check_fs_read(path)?;
        fs::read_to_string(path).map_err(ToolError::Io)
    }

    pub fn run_command(&self, cmd: &str, args: &[&str]) -> Result<Output, ToolError> {
        self.guard.check_shell(cmd)?;
        Command::new(cmd).args(args).output().map_err(ToolError::Io)
    }
}
```

---

## 8. WASM Plugin Future (v2)

### 8.1 Architecture

```
┌─────────────────────────────────────────────┐
│              XAFT PROCESS                    │
│  ┌────────────────────────────────────────┐ │
│  │         WASM Runtime (wasmtime)        │ │
│  │  ┌──────────┐  ┌──────────┐           │ │
│  │  │ Instance │  │ Instance │  ...      │ │
│  │  │ Plugin A │  │ Plugin B │           │ │
│  │  └────┬─────┘  └────┬─────┘           │ │
│  │       │              │                 │ │
│  │  ┌────▼──────────────▼──────┐         │ │
│  │  │   WASI + Xaft Host Fns  │         │ │
│  │  │  - xaft_kv_get()        │         │ │
│  │  │  - xaft_kv_set()        │         │ │
│  │  │  - xaft_fs_read()       │         │ │
│  │  │  - xaft_log()           │         │ │
│  │  │  - xaft_event_pub()     │         │ │
│  │  └─────────────────────────┘         │ │
│  └────────────────────────────────────────┘ │
└─────────────────────────────────────────────┘
```

### 8.2 WASM Host Functions

```rust
// Host-side: exported to WASM guests
fn xaft_fs_read(caller: &mut Caller<'_, WasmCtx>, path_ptr: u32, path_len: u32, buf_ptr: u32) -> u32 {
    let ctx = caller.user_data();
    let path = ctx.memory().read_str(path_ptr, path_len);

    // Capability check via the guard stored in WasmCtx
    match ctx.capability_guard.check_fs_read(Path::new(&path)) {
        Ok(()) => {
            let data = fs::read(&path).unwrap_or_default();
            ctx.memory().write_bytes(buf_ptr, &data);
            data.len() as u32
        }
        Err(_) => 0xFFFF_FFFF, // sentinel for denied
    }
}

fn xaft_kv_get(caller: &mut Caller<'_, WasmCtx>, key_ptr: u32, key_len: u32, val_ptr: u32) -> u32 {
    let ctx = caller.user_data();
    let key = ctx.memory().read_str(key_ptr, key_len);
    if let Some(val) = ctx.kv_store.get(&key) {
        ctx.memory().write_bytes(val_ptr, val.as_bytes());
        val.len() as u32
    } else {
        0
    }
}
```

### 8.3 WASM Resource Limits

| Resource | Limit | Configurable |
|----------|-------|-------------|
| Memory | 5 MB | Per-manifest `wasm.max_memory` |
| CPU time per call | 5 s | Per-manifest `wasm.max_cpu_secs` |
| Filesystem access | WASI only via host fns | Capability-gated |
| Network | Blocked by default | `network = true` in manifest |
| Stack depth | 100 frames | Hard limit |

---

## 9. Plugin Lifecycle State Machine

```
                    ┌───────────┐
                    │ Discovered │
                    └─────┬──────┘
                          │ manifest validated
                          ▼
                    ┌───────────┐
                    │  Loaded    │
                    └─────┬──────┘
                          │ dependencies resolved
                          ▼
                    ┌───────────┐
          ┌────────│ Initialized│◄─────── re-init after crash
          │        └─────┬──────┘
          │              │
          │    ┌─────────┼──────────┐
          │    ▼         ▼          ▼
          │  Active    Suspended   Errored
          │    │         │          │
          │    │   resume│          │ retry
          │    └────►────┘          │
          │              │          │
          │              ▼          │
          │          Suspended      │
          │              │          │
          └──────────────┴──────────┘
                          │
                          ▼
                     Teardown
```

### 9.1 State Transition Rules

```rust
pub enum PluginState {
    Discovered,
    Loaded,
    Initialized,
    Active,
    Suspended,
    Errored { message: String, retries: u32 },
    Teardown,
}

impl PluginRegistry {
    pub fn transition(&mut self, id: &str, event: PluginEvent) -> Result<(), TransitionError> {
        let slot = self.slots.get_mut(id).ok_or(TransitionError::NotFound)?;
        let next = match (&slot.state, event) {
            (Discovered, PluginEvent::ManifestValidated) => Loaded,
            (Loaded, PluginEvent::Initialized) => Initialized,
            (Initialized, PluginEvent::Activated) => Active,
            (Active, PluginEvent::Suspend) => Suspended,
            (Suspended, PluginEvent::Resume) => Active,
            (Active | Suspended, PluginEvent::Error(msg)) => Errored { message: msg, retries: 0 },
            (Errored { retries, .. }, PluginEvent::Retry) if *retries < 3 => {
                Errored { message: String::new(), retries: retries + 1 }
            }
            (Errored { .. }, PluginEvent::Teardown) => Teardown,
            (_, PluginEvent::Teardown) => Teardown,
            _ => return Err(TransitionError::InvalidTransition),
        };
        slot.state = next;
        Ok(())
    }
}
```

---

## 10. Error Handling

### 10.1 Error Hierarchy

```rust
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("manifest validation: {0}")]
    Manifest(#[from] ManifestError),

    #[error("ABI version mismatch: plugin={plugin}, xaft={xaft}")]
    AbiVersion { plugin: u32, xaft: u32 },

    #[error("capability denied: {0}")]
    Capability(#[from] CapabilityError),

    #[error("dependency unsatisfied: {0}")]
    Dependency(String),

    #[error("load failure: {0}")]
    Load(String),

    #[error("initialization failed: {0}")]
    Init(String),

    #[error("execution error: {0}")]
    Execution(String),
}

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("filesystem read denied for {0}")]
    FsReadDenied(PathBuf),

    #[error("filesystem write denied for {0}")]
    FsWriteDenied(PathBuf),

    #[error("network access denied")]
    NetworkDenied,

    #[error("shell command denied: {0}")]
    ShellDenied(String),

    #[error("environment variable read denied: {0}")]
    EnvReadDenied(String),
}
```

### 10.2 Error Propagation in Dispatch

When a plugin tool fails, the dispatcher wraps the error for the agent:

```rust
pub enum ToolError {
    /// Plugin reported a handled error; agent may retry with different args.
    Recoverable { message: String, suggestion: Option<String> },

    /// Plugin panicked or violated ABI; agent must not retry.
    Fatal { message: String },

    /// Capability denied; agent should not attempt similar operations.
    Denied { reason: String },
}
```

---

## 11. Plugin Discovery and Search Paths

### 11.1 Search Order

```
1. Built-in (inventory)                — always loaded first
2. ${XDG_CONFIG_HOME}/xaft/plugins/    — user-global
3. ${PROJECT_ROOT}/.xaft/plugins/      — project-local
4. ${XAFT_PLUGIN_DIR}                  — explicit override
```

### 11.2 Conflict Resolution

When two plugins provide the same kind with the same `id`:

1. **Built-in wins** over dynamic (cannot override core plugins)
2. **Project-local wins** over user-global (project-specific overrides)
3. **Explicit override wins** over everything except built-in
4. If still ambiguous: **fail fast** with a diagnostic

```rust
impl PluginRegistry {
    fn resolve_conflict(&self, existing: &PluginSlot, incoming: &PluginSlot) -> ConflictResolution {
        match (existing.source, incoming.source) {
            (BuiltIn, _) => ConflictResolution::KeepExisting,
            (_, BuiltIn) => ConflictResolution::UseIncoming,
            (ProjectLocal, UserGlobal) => ConflictResolution::KeepExisting,
            (UserGlobal, ProjectLocal) => ConflictResolution::UseIncoming,
            (Explicit, _) => ConflictResolution::UseIncoming,
            (_, Explicit) => ConflictResolution::KeepExisting,
            _ => ConflictResolution::Error,
        }
    }
}
```

---

## 12. Testing Strategy

| Level | Scope | Tool |
|-------|-------|------|
| Unit | Individual plugin trait impl | `#[test]` + mock `ToolContext` |
| Integration | Plugin loading + registration | Test dylib in `tests/fixtures/` |
| Fuzz | Manifest parsing | `cargo-fuzz` on TOML input |
| WASM sandbox | Resource limit enforcement | wasmtime test harness |
| E2E | Full agent loop with plugin tool | xaft test framework |

### 12.1 Mock Plugin Context

```rust
pub struct MockToolContext {
    pub fs_contents: HashMap<PathBuf, String>,
    pub allowed_shells: Vec<String>,
    pub captured_commands: Mutex<Vec<String>>,
}

impl ToolContextProvider for MockToolContext {
    fn read_file(&self, path: &Path) -> Result<String, ToolError> {
        self.fs_contents.get(path)
            .cloned()
            .ok_or(ToolError::Recoverable {
                message: format!("file not found: {}", path.display()),
                suggestion: None,
            })
    }

    fn run_command(&self, cmd: &str, _args: &[&str]) -> Result<Output, ToolError> {
        self.captured_commands.lock().unwrap().push(cmd.to_string());
        Ok(Output { status: ExitStatus::from_raw(0), stdout: Vec::new(), stderr: Vec::new() })
    }
}
```

---

## 13. Milestones

| Phase | Deliverable | Timeline |
|-------|-------------|----------|
| P1 | Core traits + inventory registration + static plugins | Week 1-2 |
| P2 | Dynamic library loading + manifest format + capability guard | Week 3-4 |
| P3 | Plugin resolver + conflict resolution + error hierarchy | Week 5 |
| P4 | WASM runtime integration + host functions + resource limits | Week 6-8 |
| P5 | Plugin SDK crate (derive macros, test harness, docs) | Week 9-10 |

---

## 14. Open Questions

1. **Signature verification**: Should we require Ed25519 signatures on dynamic plugin `.so` files? Adds security but friction.
2. **Plugin interop**: Should plugins be able to call other plugins' tools directly, or only via `AgentMessageBus`?
3. **Rate limiting**: Per-plugin call rate limits in the dispatcher — needed for WASM or premature optimization?
4. **Migration path**: How do we handle ABI-breaking changes between xaft minor versions? Semantic versioning of the vtable?
5. **Debugging**: Should the TUI include a plugin inspector panel showing state, capabilities, and recent errors?
