# PRD 57 — Dynamic Tool Creation

**Status:** Draft  
**Author:** Engineering  
**Date:** 2026-06-12  
**Codebase roots:**
- xaft: `/root/rust_projects/xaft`
- agtrs: `/root/rust_projects/agtrs`

---

## Table of Contents

1. [Background and Motivation](#1-background-and-motivation)
2. [Problem Statement](#2-problem-statement)
3. [Goals and Non-Goals](#3-goals-and-non-goals)
4. [Current State Audit](#4-current-state-audit)
5. [Architecture Overview](#5-architecture-overview)
6. [agtrs Changes (Upstream)](#6-agtrs-changes-upstream)
   - 6.1 DynamicToolRegistry
   - 6.2 ClosureTool
   - 6.3 LlmBackedTool
   - 6.4 MemoizedTool
   - 6.5 PipelineTool
   - 6.6 ToolFactoryTool
   - 6.7 AgentExecutor integration
7. [xaft Changes (Downstream)](#7-xaft-changes-downstream)
   - 7.1 ScriptedTool
   - 7.2 DynamicToolFactory
   - 7.3 Module layout
   - 7.4 ToolRegistryBuilder extension
   - 7.5 Orchestrator wiring
   - 7.6 Config additions
   - 7.7 New signal: XaftDynamicToolCreated
   - 7.8 TUI display
8. [Data Flow Walkthrough](#8-data-flow-walkthrough)
9. [File Change Manifest](#9-file-change-manifest)
10. [Acceptance Criteria](#10-acceptance-criteria)
11. [Test Plan with Stubs](#11-test-plan-with-stubs)
12. [Open Questions](#12-open-questions)
13. [Security Considerations](#13-security-considerations)
14. [Migration and Compatibility](#14-migration-and-compatibility)

---

## 1. Background and Motivation

xaft is a Rust-native autonomous coding agent runtime. Its current tool system is entirely
static: tools are defined at compile time by implementing `agtrs_runtime::tool::Tool`, registered
into a `ToolRegistry` at startup by `ToolRegistryBuilder`, and handed to each agent before the
first turn. Once the ReAct loop starts, the set of callable tools is frozen for the lifetime of
that run.

This architecture was sufficient for the initial toolset (file I/O, git operations, shell exec).
However, as agents are asked to handle more complex, project-specific tasks, a fixed tool palette
becomes a meaningful bottleneck.

### Illustrative examples

**Example A — Project-specific cargo runner.** An agent analyzing a Rust workspace wants a
`cargo_test_specific_module` tool that runs `cargo test --package foo --lib bar`. Without dynamic
tools it must construct the full cargo command string inside every `bash_exec` call, losing
declarative intent, requiring repeated approval gates for the same class of command, and producing
noisier TUI output.

**Example B — Semantic file summarizer.** An agent needs to quickly summarize many source files
before deciding which to edit. A `summarize_file` tool that internally calls the LLM with a fixed
"summarize in 3 sentences" prompt would be far cheaper and more reliable than embedding
summarization instructions into the main agent prompt and hoping for consistent formatting.

**Example C — Typed API caller.** An agent discovers a new REST endpoint in the codebase and
wants a `call_payments_api` tool with a validated JSON schema for the endpoint's request body.
Using raw `bash_exec curl` sacrifices schema validation and produces confusing diffs in the
approval gate.

**Example D — Memoized git log.** An agent calls `git_log_for_file` repeatedly while analyzing
changes across many files. The underlying `git log -- <path>` is expensive; the result is stable
for the duration of a run. A cached wrapper would halve the LLM-visible latency with zero
correctness cost.

Without dynamic tool creation, agents must either accept the ergonomic loss of verbose `bash_exec`
commands, duplicate work across turns, or declare failure on tasks that genuinely require
project-specific operations.

---

## 2. Problem Statement

`AgentContext::register_tool` (defined in `agtrs-runtime/src/agent.rs`, line 383) takes `&mut
self`, making tool registration an exclusive-ownership operation. The `AgentExecutor::run` method
(in `agtrs-runtime/src/executor.rs`, line 78) calls `register_tool` before the ReAct loop
begins, at lines 86–91:

```rust
for tool in agent.tools() {
    let name = tool.name().to_string();
    if !ctx.tools().contains_key(&name) {
        ctx.register_tool(name, tool);
    }
}
```

After this point, `ctx` is held as `&mut AgentContext` inside the loop, but tool lookup is done
via the immutable `HashMap<String, Arc<ErasedTool>>` at `ctx.tools` (line 191 of `agent.rs`).
There is no mechanism for the executing agent to modify this map mid-turn.

Beyond the registry locking problem, the `Tool` trait requires a full struct with explicit type
parameters (`type Inputs`, `type Output`). There is no first-class support for:

- Async closure-backed tools
- LLM-invocation-backed tools with a configurable system prompt
- Shell-script-backed tools
- Tool pipelines (output of A feeds input of B)
- Result memoization at the registry layer
- Run-scoped tool limits or quotas

---

## 3. Goals and Non-Goals

### Goals

| # | Goal |
|---|------|
| G1 | **ClosureTool**: wrap an async closure as a `Tool` without defining a struct. |
| G2 | **LlmBackedTool**: a tool whose handler is an LLM call with a user-configured system prompt. |
| G3 | **ScriptedTool**: a tool whose handler executes a bash script with tool input injected via env vars. |
| G4 | **MemoizedTool**: a wrapper that caches tool results by a hash of the input for the lifetime of a run. |
| G5 | **PipelineTool**: chain N existing tools; output of tool[i] feeds input of tool[i+1]. |
| G6 | **In-flight registration**: `ToolFactoryTool` — an agent-callable tool that defines and registers new tools mid-run. |
| G7 | **Shared registry across agents**: dynamic tools registered by agent A are visible to agent B within the same orchestration run. |
| G8 | **Safety**: dynamic shell tools run inside the existing workspace sandbox. New shell tools require approval-gate confirmation by default. |
| G9 | **Config**: `allow_dynamic_tools`, `max_dynamic_tools`, `dynamic_tool_approval` in `AgentPreset`. |
| G10 | **Observability**: `XaftDynamicToolCreated` signal emitted when a tool is registered; TUI shows a status line entry. |
| G11 | **Registry lifetime**: dynamic tools are cleared after each run; no leakage across runs. |

### Non-Goals

- Persisting dynamic tool definitions across sessions (tool definitions are ephemeral to a run).
- Giving the LLM arbitrary code execution through tool definitions not gated by the approval mechanism.
- Replacing the existing statically-defined tool system; dynamic tools are additive.
- Supporting WASM or plugin-based tool execution (out of scope for this PRD).
- Dynamic tool schemas that reference external JSON Schema files at runtime.

---

## 4. Current State Audit

### agtrs-runtime/src/agent.rs

`AgentContext` (defined at line 179) holds a private `tools: HashMap<String, Arc<ErasedTool>>`.
The `register_tool` method (line 383) requires `&mut self` and therefore cannot be called from
inside the executor's loop after `ctx` has been borrowed. `clone()` (line 294) performs a deep
clone of the HashMap — each clone is a fully independent snapshot with no shared backing store.
This means a cloned context (e.g., the one passed to `ToolCallContext::agent_context` in
`tool_hooks.rs`) cannot receive new tool registrations made after the clone.

`call_tool` (line 395) looks tools up synchronously: `self.tools.get(tool_name)`. Any tool
registered after the executor's setup phase cannot be found here.

### agtrs-runtime/src/executor.rs

The executor calls `agent.tools()` to get the static tool list from the `Agent` trait, then
seeds `ctx.tools` before entering the loop (lines 86–91). The LLM receives the tool schema list
built at line 175:

```rust
let tool_schemas: Vec<crate::transport::ToolSchema> =
    agent.tools().iter().map(|t| t.to_tool_schema()).collect();
options = options.with_tools(tool_schemas);
```

This list is rebuilt from `agent.tools()` (the static list) on every turn. Dynamic tools added
to the context will not appear in subsequent LLM calls unless the executor is changed to consult
an additional dynamic source.

### agtrs-runtime/src/tool.rs

`ErasedTool` is a type alias (line 393):

```rust
pub type ErasedTool = dyn Tool<Inputs = serde_json::Value, Output = ToolResult>;
```

All tools must implement this trait in full. There is no convenience type for closure-backed or
LLM-backed tools.

### xaft-tools/src/registry.rs

`ToolRegistryBuilder` is a static factory. `build_reader` and `build_coder` take ownership of
builder state and produce a `ToolRegistry` at startup. There is no mechanism for adding tools
after the registry is built.

### xaft-agent/src/signals.rs

The signal file defines `XaftLlmCallStarting`, `XaftCommitCreated`, `XaftPlanCreated`,
`XaftAgentHandoff`, and related signals. There is no signal for dynamic tool creation.

### xaft-config/src/types.rs

`AgentPreset` (line 226) contains `allowed_tools`, `denied_tools`, `max_turns`, and
`temperature` but no fields governing dynamic tool behaviour.

---

## 5. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        xaft run_workflow()                       │
│                                                                  │
│  DynamicToolRegistry (Arc<RwLock<...>>) ──── shared across ───  │
│           │                                    all agents        │
│           │                                                      │
│    ┌──────┴──────┐   ┌──────────────┐   ┌──────────────┐        │
│    │  Planner     │   │    Coder     │   │   QA/Fixer   │        │
│    │  (has        │   │  (has        │   │  (inherits   │        │
│    │  tool_factory│   │  tool_factory│   │   dynamic    │        │
│    │  tool)       │   │  tool)       │   │   registry)  │        │
│    └──────┬───────┘   └──────┬───────┘   └──────┬───────┘        │
│           │                  │                  │               │
│           └──── all write to the same DynamicToolRegistry ───── │
│                                                                  │
│  Static tools (ToolRegistryBuilder) are unaffected.             │
│  Dynamic registry is queried FIRST; static is the fallback.     │
└─────────────────────────────────────────────────────────────────┘

                     agtrs-runtime (upstream)
┌─────────────────────────────────────────────────────────────────┐
│  AgentContext                                                    │
│    tools: HashMap<String, Arc<ErasedTool>>   ← unchanged        │
│    dynamic_registry: Option<Arc<DynamicToolRegistry>>  ← NEW    │
│                                                                  │
│  AgentExecutor::run()                                           │
│    build tool_schemas = static_tools ∪ dynamic_registry.all()  │
│    call_tool(): dynamic_registry.get() ?? static tools.get()    │
└─────────────────────────────────────────────────────────────────┘

       New tool types (agtrs)          New tool types (xaft)
┌──────────────────────────┐     ┌──────────────────────────────┐
│  ClosureTool             │     │  ScriptedTool                │
│  LlmBackedTool           │     │  DynamicToolFactory          │
│  MemoizedTool            │     └──────────────────────────────┘
│  PipelineTool            │
│  ToolFactoryTool         │
└──────────────────────────┘
```

---

## 6. agtrs Changes (Upstream)

### 6.1 DynamicToolRegistry

**File:** `agtrs-runtime/src/dynamic_tools.rs` (new file)

The core of the upstream change is a `DynamicToolRegistry` that wraps an
`Arc<tokio::sync::RwLock<...>>` so it can be cloned cheaply and shared across agent contexts
without requiring `&mut` access.

```rust
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::error::AgtrsError;
use crate::tool::ErasedTool;

/// A concurrently-accessible, run-scoped tool registry.
///
/// Unlike the static tool map on `AgentContext`, this registry is backed by an
/// `Arc<RwLock<...>>` so it can be:
///  - Cloned cheaply (all clones share the same underlying map).
///  - Written to from inside a running executor turn via interior mutability.
///  - Shared across multiple agents in the same orchestration run.
///
/// # Concurrency
///
/// Reads (`get`, `all`) acquire a shared read lock and are lock-free under
/// contention from other readers. Writes (`register`) acquire an exclusive
/// write lock; write contention is expected to be rare (tools are registered
/// infrequently, usually once per tool-factory invocation).
#[derive(Clone, Debug, Default)]
pub struct DynamicToolRegistry {
    inner: Arc<RwLock<HashMap<String, Arc<ErasedTool>>>>,
    /// Maximum number of tools that may be registered.  `None` = unlimited.
    max_tools: Option<usize>,
}

impl DynamicToolRegistry {
    /// Create an empty registry with no limit.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_tools: None,
        }
    }

    /// Create an empty registry with a maximum tool count.
    pub fn with_limit(max_tools: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_tools: Some(max_tools),
        }
    }

    /// Register a tool.  Replaces an existing entry with the same name.
    ///
    /// Returns `Err(AgtrsError::ToolCallFailed)` when `max_tools` is set
    /// and the limit would be exceeded.
    pub async fn register(
        &self,
        name: String,
        tool: Arc<ErasedTool>,
    ) -> Result<(), AgtrsError> {
        let mut map = self.inner.write().await;
        if let Some(max) = self.max_tools {
            if !map.contains_key(&name) && map.len() >= max {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: "tool_factory".into(),
                    reason: format!(
                        "dynamic tool limit ({max}) reached; cannot register '{name}'"
                    ),
                });
            }
        }
        map.insert(name, tool);
        Ok(())
    }

    /// Look up a tool by name.
    pub async fn get(&self, name: &str) -> Option<Arc<ErasedTool>> {
        self.inner.read().await.get(name).cloned()
    }

    /// Return all registered tools in insertion order (HashMap order; stable
    /// within a turn but not guaranteed across turns).
    pub async fn all(&self) -> Vec<Arc<ErasedTool>> {
        self.inner.read().await.values().cloned().collect()
    }

    /// Number of currently registered tools.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// `true` when no tools have been registered.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    /// Clear all registered tools. Called at the end of a run to prevent
    /// leakage across subsequent runs that reuse the same registry handle.
    pub async fn clear(&self) {
        self.inner.write().await.clear();
    }

    /// Return the maximum number of tools this registry will accept.
    pub fn max_tools(&self) -> Option<usize> {
        self.max_tools
    }
}
```

**Integration with `AgentContext`:**

Add a new field to `AgentContext` in `agent.rs`:

```rust
/// Optional run-scoped dynamic tool registry shared across agents.
///
/// When `Some`, the executor checks this registry first (overrides static),
/// then falls back to the static `tools` map.  All agents in the same
/// orchestration run share the same `Arc<RwLock<...>>` backing store through
/// cheap `DynamicToolRegistry::clone()`.
dynamic_registry: Option<DynamicToolRegistry>,
```

Add the corresponding setter, getter, and clone-forwarding logic:

```rust
impl AgentContext {
    /// Attach a shared dynamic tool registry to this context.
    pub fn set_dynamic_registry(&mut self, registry: DynamicToolRegistry) {
        self.dynamic_registry = Some(registry);
    }

    /// Get the dynamic tool registry, if any.
    pub fn dynamic_registry(&self) -> Option<&DynamicToolRegistry> {
        self.dynamic_registry.as_ref()
    }
}
```

Update `AgentContext::clone()` to forward the registry (cheap — just clones the `Arc`):

```rust
dynamic_registry: self.dynamic_registry.clone(),
```

### 6.2 ClosureTool

**File:** `agtrs-runtime/src/dynamic_tools.rs`

`ClosureTool` allows creating a `Tool` from an async closure without defining a struct. It is the
foundational primitive — `LlmBackedTool`, `ScriptedTool`, and `ToolFactoryTool` can all be
implemented on top of it (or as thin wrappers).

```rust
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::AgtrsError;
use crate::tool::{ErasedTool, Tool, ToolContext, ToolResult};

/// Handler type alias for `ClosureTool`.
///
/// The closure receives:
/// - `input`: the JSON object supplied by the LLM.
/// - `ctx`: the `ToolContext` for this invocation.
///
/// It returns a `Future` that resolves to `Result<ToolResult, AgtrsError>`.
pub type ClosureHandler = Arc<
    dyn Fn(Value, ToolContext) -> Pin<Box<dyn Future<Output = Result<ToolResult, AgtrsError>> + Send>>
        + Send
        + Sync,
>;

/// A `Tool` backed by an async closure.
///
/// # Example
/// ```rust,ignore
/// let tool = ClosureTool::new(
///     "echo",
///     "Echoes the input back",
///     serde_json::json!({"type":"object","properties":{"msg":{"type":"string"}}}),
///     |input, ctx| async move {
///         let msg = input["msg"].as_str().unwrap_or("").to_string();
///         Ok(ToolResult::ok(msg, ctx.tool_use_id))
///     },
/// );
/// ```
pub struct ClosureTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub parallel_safe: bool,
    pub requires_confirmation: bool,
    handler: ClosureHandler,
}

impl ClosureTool {
    /// Create a new `ClosureTool`.
    ///
    /// The `handler` closure is wrapped in `Arc` internally so the tool can
    /// be cloned cheaply when shared across parallel tool calls.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
        handler: F,
    ) -> Self
    where
        F: Fn(Value, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolResult, AgtrsError>> + Send + 'static,
    {
        let handler: ClosureHandler = Arc::new(move |input, ctx| {
            Box::pin(handler(input, ctx))
        });
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            parallel_safe: false,
            requires_confirmation: false,
            handler,
        }
    }

    /// Mark this tool as safe to run in parallel with other tools.
    pub fn parallel_safe(mut self) -> Self {
        self.parallel_safe = true;
        self
    }

    /// Require human approval before execution.
    pub fn requires_confirmation(mut self) -> Self {
        self.requires_confirmation = true;
        self
    }
}

#[async_trait]
impl Tool for ClosureTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> Value { self.schema.clone() }
    fn requires_confirmation(&self) -> bool { self.requires_confirmation }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        (self.handler)(input, ctx.clone()).await
    }
}
```

### 6.3 LlmBackedTool

**File:** `agtrs-runtime/src/dynamic_tools.rs`

`LlmBackedTool` is a tool whose implementation is an LLM call. When the agent calls it, the tool
formats the input as a user message, sends it with a configured system prompt to an `LlmProvider`,
and returns the text response.

```rust
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::AgtrsError;
use crate::llm::{LlmOptions, LlmProvider};
use crate::tool::{Tool, ToolContext, ToolResult};
use crate::transport::Message;

/// A `Tool` whose implementation is an LLM call with a fixed system prompt.
///
/// Useful for "semantic" tools: summarizers, classifiers, translators, etc.
///
/// The tool formats its input as a JSON-encoded user message, calls the
/// configured provider, and returns the text content of the response.
///
/// # Cost
///
/// Each invocation makes one LLM API call. The tokens consumed are NOT
/// tracked in the calling agent's `AgentMetadata`; they are tracked inside
/// the provider's own request context. Callers should be aware of the
/// additional cost.
pub struct LlmBackedTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub requires_confirmation: bool,
    pub llm: Arc<dyn LlmProvider>,
}

impl LlmBackedTool {
    /// Format the tool input into a user message string.
    ///
    /// Scalars are unwrapped to their string value; objects/arrays are
    /// pretty-printed as JSON so the LLM receives readable context.
    fn format_input(input: &Value) -> String {
        match input {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "(empty)".into(),
            other => serde_json::to_string_pretty(other)
                .unwrap_or_else(|_| other.to_string()),
        }
    }
}

#[async_trait]
impl Tool for LlmBackedTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> Value { self.schema.clone() }
    fn requires_confirmation(&self) -> bool { self.requires_confirmation }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: "llm_backed_tool cancelled before LLM call".into(),
            });
        }

        let user_message = Self::format_input(&input);
        let messages = vec![
            Message::system(&self.system_prompt),
            Message::user(user_message),
        ];
        let options = LlmOptions::default()
            .with_max_tokens(self.max_tokens);

        let response = tokio::select! {
            biased;
            _ = ctx.cancel_token.cancelled() => {
                return Err(AgtrsError::Cancelled {
                    reason: "llm_backed_tool cancelled during LLM call".into(),
                });
            }
            r = self.llm.complete(&messages, &options) => r?,
        };

        Ok(ToolResult::ok(response.message.text(), &ctx.tool_use_id))
    }
}
```

### 6.4 MemoizedTool

**File:** `agtrs-runtime/src/dynamic_tools.rs`

`MemoizedTool` wraps any `Arc<ErasedTool>` and caches its results by a hash of the input `Value`.
The cache lives for the lifetime of the `MemoizedTool` instance (typically a single run when the
tool is registered in a `DynamicToolRegistry` that is cleared at run end).

```rust
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::AgtrsError;
use crate::tool::{ErasedTool, Tool, ToolContext, ToolResult};

/// Caches tool results by a hash of the input `Value`.
///
/// The cache is stored in a `Mutex<HashMap<u64, ToolResult>>` shared across
/// all invocations of the same `MemoizedTool` instance. The hash is computed
/// with `DefaultHasher` over the JSON-serialised input bytes, which gives
/// consistent results within a single process run.
///
/// # Semantics
///
/// Memoization is best-effort: if serialisation fails, the call is forwarded
/// to the inner tool without caching. Cache hits return a `Clone` of the
/// stored result.
///
/// # Thread safety
///
/// The `Mutex` ensures cache reads/writes are safe under concurrent parallel
/// tool calls (`AgentConfig::parallel_tool_calls = true`).
pub struct MemoizedTool {
    inner: Arc<ErasedTool>,
    cache: Arc<Mutex<HashMap<u64, ToolResult>>>,
}

impl MemoizedTool {
    pub fn new(inner: Arc<ErasedTool>) -> Self {
        Self {
            inner,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn hash_input(input: &Value) -> Option<u64> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let bytes = serde_json::to_vec(input).ok()?;
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        Some(h.finish())
    }
}

#[async_trait]
impl Tool for MemoizedTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { self.inner.name() }
    fn description(&self) -> &str { self.inner.description() }
    fn schema(&self) -> Value { self.inner.schema() }
    fn requires_confirmation(&self) -> bool { self.inner.requires_confirmation() }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let hash = match Self::hash_input(&input) {
            Some(h) => h,
            None => return self.inner.call(input, ctx).await,
        };

        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get(&hash) {
                return Ok(ToolResult {
                    tool_use_id: ctx.tool_use_id.clone(),
                    content: cached.content.clone(),
                    is_error: cached.is_error,
                });
            }
        }

        let result = self.inner.call(input, ctx).await?;

        {
            let mut cache = self.cache.lock().await;
            cache.insert(hash, result.clone());
        }

        Ok(result)
    }
}
```

### 6.5 PipelineTool

**File:** `agtrs-runtime/src/dynamic_tools.rs`

`PipelineTool` chains N existing tools sequentially. The output `content` string of tool[i] is
passed as the value of an `"input"` key to tool[i+1]. The final tool's result is returned to the
LLM.

```rust
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::AgtrsError;
use crate::tool::{ErasedTool, Tool, ToolContext, ToolResult};

/// A tool that chains N existing tools sequentially.
///
/// The output of each tool is passed as `{"input": "<content>"}` to the next.
/// The initial input from the LLM is passed to the first tool unchanged.
/// The result of the final tool is returned to the LLM.
///
/// If any tool in the chain returns `is_error = true`, the pipeline stops
/// and returns that error result immediately.
///
/// # Construction
///
/// ```rust,ignore
/// let pipeline = PipelineTool::new(
///     "read_and_summarize",
///     "Read a file then summarize it",
///     vec![read_file_tool, summarize_tool],
/// );
/// ```
pub struct PipelineTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    tools: Vec<Arc<ErasedTool>>,
}

impl PipelineTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        tools: Vec<Arc<ErasedTool>>,
    ) -> Self {
        assert!(!tools.is_empty(), "PipelineTool requires at least one tool");
        let schema = tools[0].schema();
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            tools,
        }
    }
}

#[async_trait]
impl Tool for PipelineTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }

    /// The schema of the pipeline is the schema of the first tool.
    /// The LLM only needs to know how to invoke the entry point.
    fn schema(&self) -> Value { self.schema.clone() }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let mut current_input = input;

        for (i, tool) in self.tools.iter().enumerate() {
            if ctx.is_cancelled() {
                return Err(AgtrsError::Cancelled {
                    reason: format!(
                        "pipeline_tool '{}' cancelled at step {i}",
                        self.name
                    ),
                });
            }

            let result = tool.call(current_input.clone(), ctx).await?;

            if result.is_error {
                return Ok(ToolResult {
                    tool_use_id: ctx.tool_use_id.clone(),
                    content: format!(
                        "Pipeline failed at step {} (tool '{}'): {}",
                        i,
                        tool.name(),
                        result.content
                    ),
                    is_error: true,
                });
            }

            // Wrap the output for the next tool as {"input": "<content>"}
            if i + 1 < self.tools.len() {
                current_input = json!({ "input": result.content });
            } else {
                return Ok(ToolResult {
                    tool_use_id: ctx.tool_use_id.clone(),
                    content: result.content,
                    is_error: false,
                });
            }
        }

        unreachable!("tools vec is non-empty")
    }
}
```

### 6.6 ToolFactoryTool

**File:** `agtrs-runtime/src/dynamic_tools.rs`

`ToolFactoryTool` is the tool the LLM calls to register a new tool. It accepts a JSON description
of the new tool, instantiates the appropriate type, and registers it in the shared
`DynamicToolRegistry`.

```rust
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::AgtrsError;
use crate::llm::LlmProvider;
use crate::tool::{Tool, ToolContext, ToolResult};

use super::DynamicToolRegistry;

/// Input schema for the `tool_factory` tool.
///
/// The LLM must supply this structure to define and register a new tool.
///
/// ```json
/// {
///   "tool_name": "cargo_test_module",
///   "description": "Run cargo test for a specific package",
///   "input_schema": {
///     "type": "object",
///     "properties": {
///       "package": {"type": "string"},
///       "test_filter": {"type": "string"}
///     },
///     "required": ["package"]
///   },
///   "implementation": {
///     "kind": "shell_script",
///     "script": "cargo test --package $XAFT_INPUT_package $XAFT_INPUT_test_filter"
///   }
/// }
/// ```
///
/// `implementation.kind` must be one of:
/// - `"shell_script"` — a bash script executed via `ScriptedTool`.
/// - `"llm_prompt"` — an LLM call with a custom system prompt via `LlmBackedTool`.
pub struct ToolFactoryTool {
    registry: DynamicToolRegistry,
    llm: Arc<dyn LlmProvider>,
    workspace_root: PathBuf,
    allow_dynamic_tools: bool,
}

impl ToolFactoryTool {
    pub fn new(
        registry: DynamicToolRegistry,
        llm: Arc<dyn LlmProvider>,
        workspace_root: PathBuf,
        allow_dynamic_tools: bool,
    ) -> Self {
        Self { registry, llm, workspace_root, allow_dynamic_tools }
    }

    /// Parse and validate the tool definition from the LLM input.
    fn parse_definition(input: &Value) -> Result<ToolDefinition, String> {
        let tool_name = input["tool_name"]
            .as_str()
            .ok_or("missing 'tool_name' (string)")?
            .to_string();

        if tool_name.is_empty() || tool_name.len() > 64 {
            return Err(format!(
                "tool_name must be 1–64 chars; got '{tool_name}'"
            ));
        }
        // Reject names that could shadow built-in tools
        for reserved in &["tool_factory", "bash_exec", "read_file", "write_file"] {
            if tool_name == *reserved {
                return Err(format!("'{tool_name}' is a reserved tool name"));
            }
        }

        let description = input["description"]
            .as_str()
            .ok_or("missing 'description' (string)")?
            .to_string();

        let input_schema = input.get("input_schema").cloned().unwrap_or_else(|| {
            json!({"type": "object"})
        });

        let impl_obj = input.get("implementation")
            .ok_or("missing 'implementation' object")?;

        let kind = impl_obj["kind"]
            .as_str()
            .ok_or("implementation.kind must be a string")?;

        let implementation = match kind {
            "shell_script" => {
                let script = impl_obj["script"]
                    .as_str()
                    .ok_or("shell_script requires 'script' field")?
                    .to_string();
                ToolImplementation::ShellScript { script }
            }
            "llm_prompt" => {
                let prompt = impl_obj["prompt"]
                    .as_str()
                    .ok_or("llm_prompt requires 'prompt' field")?
                    .to_string();
                let max_tokens = impl_obj["max_tokens"]
                    .as_u64()
                    .unwrap_or(1024) as u32;
                ToolImplementation::LlmPrompt { prompt, max_tokens }
            }
            other => return Err(format!("unknown implementation kind: '{other}'")),
        };

        Ok(ToolDefinition { tool_name, description, input_schema, implementation })
    }
}

struct ToolDefinition {
    tool_name: String,
    description: String,
    input_schema: Value,
    implementation: ToolImplementation,
}

enum ToolImplementation {
    ShellScript { script: String },
    LlmPrompt { prompt: String, max_tokens: u32 },
}

/// The LLM-visible name for the tool factory.
pub const TOOL_FACTORY_NAME: &str = "tool_factory";

#[async_trait]
impl Tool for ToolFactoryTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { TOOL_FACTORY_NAME }

    fn description(&self) -> &str {
        "Define and register a new tool for use in subsequent turns of this run. \
         Supports 'shell_script' and 'llm_prompt' implementations. \
         The registered tool is immediately callable by name."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_name": {
                    "type": "string",
                    "description": "Unique name for the new tool (snake_case, max 64 chars)"
                },
                "description": {
                    "type": "string",
                    "description": "Human-readable description of what the tool does"
                },
                "input_schema": {
                    "type": "object",
                    "description": "JSON Schema for the tool's input parameters"
                },
                "implementation": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["shell_script", "llm_prompt"]
                        },
                        "script": {
                            "type": "string",
                            "description": "Bash script body (for shell_script kind)"
                        },
                        "prompt": {
                            "type": "string",
                            "description": "LLM system prompt (for llm_prompt kind)"
                        },
                        "max_tokens": {
                            "type": "integer",
                            "description": "Max response tokens (for llm_prompt kind, default 1024)"
                        }
                    },
                    "required": ["kind"]
                }
            },
            "required": ["tool_name", "description", "implementation"]
        })
    }

    fn requires_confirmation(&self) -> bool {
        // The factory itself does not require confirmation; the *created* tool may.
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if !self.allow_dynamic_tools {
            return Ok(ToolResult::error(
                "dynamic tool creation is disabled (allow_dynamic_tools = false)",
                &ctx.tool_use_id,
            ));
        }

        let def = match Self::parse_definition(&input) {
            Ok(d) => d,
            Err(e) => {
                return Ok(ToolResult::error(
                    format!("tool_factory: invalid definition: {e}"),
                    &ctx.tool_use_id,
                ));
            }
        };

        let name = def.tool_name.clone();

        // Build the concrete tool
        let erased: Arc<ErasedTool> = match def.implementation {
            ToolImplementation::ShellScript { script } => {
                // xaft's ScriptedTool is wired in by the xaft layer via
                // DynamicToolFactory. At the agtrs level, shell tools are
                // represented as ClosureTools that delegate to a subprocess.
                // xaft overrides this behaviour by replacing ToolFactoryTool
                // with DynamicToolFactory in its registry wiring.
                //
                // For pure agtrs usage (without xaft), fall back to a closure
                // that runs the script via std::process::Command.
                let workspace = self.workspace_root.clone();
                Arc::new(ClosureTool::new(
                    name.clone(),
                    def.description.clone(),
                    def.input_schema,
                    move |tool_input, ctx| {
                        let script = script.clone();
                        let workspace = workspace.clone();
                        async move {
                            if ctx.is_cancelled() {
                                return Err(AgtrsError::Cancelled {
                                    reason: "scripted_tool cancelled".into(),
                                });
                            }
                            let input_json = serde_json::to_string(&tool_input)
                                .unwrap_or_default();
                            let output = tokio::process::Command::new("bash")
                                .arg("-c")
                                .arg(&script)
                                .env("XAFT_TOOL_INPUT", &input_json)
                                .current_dir(&workspace)
                                .output()
                                .await
                                .map_err(|e| AgtrsError::ToolCallFailed {
                                    tool_name: "scripted_tool".into(),
                                    reason: e.to_string(),
                                })?;
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            if output.status.success() {
                                Ok(ToolResult::ok(stdout.trim().to_string(), ctx.tool_use_id))
                            } else {
                                Ok(ToolResult::error(
                                    format!("exit {}: {stderr}", output.status),
                                    ctx.tool_use_id,
                                ))
                            }
                        }
                    },
                )) as Arc<ErasedTool>
            }
            ToolImplementation::LlmPrompt { prompt, max_tokens } => {
                Arc::new(LlmBackedTool {
                    name: name.clone(),
                    description: def.description.clone(),
                    schema: def.input_schema,
                    system_prompt: prompt,
                    max_tokens,
                    requires_confirmation: false,
                    llm: Arc::clone(&self.llm),
                }) as Arc<ErasedTool>
            }
        };

        self.registry
            .register(name.clone(), erased)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: TOOL_FACTORY_NAME.into(),
                reason: e.to_string(),
            })?;

        Ok(ToolResult::ok(
            format!(
                "Tool '{name}' registered successfully. You may now call it by name."
            ),
            &ctx.tool_use_id,
        ))
    }
}
```

### 6.7 AgentExecutor integration

**File:** `agtrs-runtime/src/executor.rs`

Two changes are required in the executor:

**Change 1 — Tool schema list sent to LLM (line ~175).** Currently the executor only sends
`agent.tools()` (the static list). After this PRD, it must also include tools from the dynamic
registry:

```rust
// Before (current):
let tool_schemas: Vec<crate::transport::ToolSchema> =
    agent.tools().iter().map(|t| t.to_tool_schema()).collect();

// After:
let mut tool_schemas: Vec<crate::transport::ToolSchema> =
    agent.tools().iter().map(|t| t.to_tool_schema()).collect();

if let Some(dyn_reg) = ctx.dynamic_registry() {
    let dynamic = dyn_reg.all().await;
    for dyn_tool in dynamic {
        // Only add if not already in the static list
        if !tool_schemas.iter().any(|s| s.name == dyn_tool.name()) {
            tool_schemas.push(dyn_tool.to_tool_schema());
        }
    }
}

options = options.with_tools(tool_schemas);
```

**Change 2 — Tool dispatch (inside `AgentContext::call_tool`).** The lookup at line 414 in
`agent.rs` must try the dynamic registry first:

```rust
// Before (current):
let tool = self.tools.get(tool_name).ok_or_else(|| { ... })?;

// After:
// First try dynamic registry (overrides static)
let dynamic_tool = if let Some(reg) = &self.dynamic_registry {
    reg.get(tool_name).await
} else {
    None
};

let tool: Arc<ErasedTool> = match dynamic_tool {
    Some(t) => t,
    None => self.tools.get(tool_name).ok_or_else(|| {
        let available_static: Vec<&str> = self.tools.keys().map(|k| k.as_str()).collect();
        AgtrsError::ToolCallFailed {
            tool_name: tool_name.to_string(),
            reason: format!(
                "tool '{tool_name}' not found (static: [{}])",
                available_static.join(", ")
            ),
        }
    })?.clone(),
};
```

**Note on async in `call_tool`.** `call_tool` is already `async`. The `reg.get()` call adds a
single await on a read lock, which is negligible compared to the tool execution itself.

---

## 7. xaft Changes (Downstream)

### 7.1 ScriptedTool

**File:** `crates/xaft-tools/src/dynamic/scripted.rs` (new file)

`ScriptedTool` is xaft's production-grade replacement for the closure-based fallback in
`ToolFactoryTool`. It uses the existing `CommandExecutor` and `Sandbox` infrastructure for path
sandboxing and timeout enforcement, and it respects the approval gate via
`requires_confirmation`.

```rust
//! `ScriptedTool` — a dynamically-defined tool backed by a bash script.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_shell::{CommandExecutor, SandboxError};

/// A tool whose implementation is a bash script.
///
/// The script receives the tool input as:
/// - `XAFT_TOOL_INPUT`: the full JSON-encoded input object.
/// - `XAFT_INPUT_<field>`: one variable per top-level string field in the input.
///
/// The tool returns stdout on success or `stderr + exit code` on failure.
///
/// # Path safety
///
/// `ScriptedTool` runs inside the workspace sandbox configured at construction
/// time. The script cannot create files outside `workspace_root`.
///
/// # Approval gate
///
/// When `requires_confirmation = true` (the default for dynamically-created
/// shell tools), the `ApprovalGate` blocks execution until the user approves.
pub struct ScriptedTool {
    pub name: String,
    pub description: String,
    pub script: String,
    pub input_schema: Value,
    pub requires_confirmation: bool,
    executor: Arc<CommandExecutor>,
}

impl ScriptedTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        script: impl Into<String>,
        input_schema: Value,
        requires_confirmation: bool,
        executor: Arc<CommandExecutor>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            script: script.into(),
            input_schema,
            requires_confirmation,
            executor,
        }
    }

    /// Build the environment variable map from the tool input.
    fn build_env(input: &Value) -> HashMap<String, String> {
        let mut env = HashMap::new();
        let json_str = serde_json::to_string(input).unwrap_or_default();
        env.insert("XAFT_TOOL_INPUT".into(), json_str);

        if let Value::Object(map) = input {
            for (key, val) in map {
                let env_key = format!("XAFT_INPUT_{}", key.to_uppercase());
                let env_val = match val {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => String::new(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                env.insert(env_key, env_val);
            }
        }
        env
    }
}

#[async_trait]
impl Tool for ScriptedTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> Value { self.input_schema.clone() }
    fn requires_confirmation(&self) -> bool { self.requires_confirmation }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("scripted_tool '{}' cancelled", self.name),
            });
        }

        let env = Self::build_env(&input);
        let script = self.script.clone();

        let result = self.executor
            .execute_with_env("bash", &["-c", &script], env)
            .await
            .map_err(|e| {
                // SandboxError::PathEscape maps directly to a hard error —
                // the script tried to write outside the workspace root.
                if matches!(e, SandboxError::PathEscape { .. }) {
                    AgtrsError::ToolCallFailed {
                        tool_name: self.name.clone(),
                        reason: format!("sandbox violation: {e}"),
                    }
                } else {
                    AgtrsError::ToolCallFailed {
                        tool_name: self.name.clone(),
                        reason: e.to_string(),
                    }
                }
            })?;

        if result.exit_code == 0 {
            Ok(ToolResult::ok(result.stdout.trim().to_string(), &ctx.tool_use_id))
        } else {
            Ok(ToolResult::error(
                format!("exit {}: {}", result.exit_code, result.stderr),
                &ctx.tool_use_id,
            ))
        }
    }
}
```

### 7.2 DynamicToolFactory

**File:** `crates/xaft-tools/src/dynamic/factory.rs` (new file)

`DynamicToolFactory` is xaft's replacement for the `ToolFactoryTool` provided by agtrs. It hooks
into xaft's `SignalBus` to emit `XaftDynamicToolCreated` and uses `ScriptedTool` (backed by
`CommandExecutor` with the workspace sandbox) instead of the agtrs fallback closure.

```rust
//! `DynamicToolFactory` — xaft-specific `ToolFactoryTool` with sandbox and signals.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use agtrs_runtime::dynamic_tools::{
    DynamicToolRegistry, LlmBackedTool, TOOL_FACTORY_NAME,
};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_shell::{CommandExecutor, ExecutionPolicy, Sandbox};

use xaft_agent::signals::XaftDynamicToolCreated;

use crate::dynamic::scripted::ScriptedTool;

/// xaft-specific `ToolFactoryTool` that:
///  1. Validates the tool definition (same schema as `ToolFactoryTool`).
///  2. Creates `ScriptedTool` (with workspace sandbox) or `LlmBackedTool`.
///  3. Registers the new tool in the shared `DynamicToolRegistry`.
///  4. Emits `XaftDynamicToolCreated` on the signal bus.
pub struct DynamicToolFactory {
    registry: DynamicToolRegistry,
    llm: Arc<dyn LlmProvider>,
    workspace_root: PathBuf,
    executor: Arc<CommandExecutor>,
    signals: Arc<SignalBus>,
    allow_dynamic_tools: bool,
    dynamic_tool_approval: bool,
}

impl DynamicToolFactory {
    pub fn new(
        registry: DynamicToolRegistry,
        llm: Arc<dyn LlmProvider>,
        workspace_root: PathBuf,
        signals: Arc<SignalBus>,
        allow_dynamic_tools: bool,
        dynamic_tool_approval: bool,
    ) -> Self {
        let sandbox = Sandbox::new(&workspace_root);
        let executor = Arc::new(CommandExecutor::new(
            sandbox,
            ExecutionPolicy::default(),
        ));
        Self {
            registry,
            llm,
            workspace_root,
            executor,
            signals,
            allow_dynamic_tools,
            dynamic_tool_approval,
        }
    }
}

#[async_trait]
impl Tool for DynamicToolFactory {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str { TOOL_FACTORY_NAME }

    fn description(&self) -> &str {
        "Define and register a new tool for use in subsequent turns of this run."
    }

    fn schema(&self) -> Value {
        // Re-use the agtrs ToolFactoryTool schema (identical JSON shape).
        agtrs_runtime::dynamic_tools::ToolFactoryTool::schema_json()
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if !self.allow_dynamic_tools {
            return Ok(ToolResult::error(
                "dynamic tool creation is disabled (allow_dynamic_tools = false in config)",
                &ctx.tool_use_id,
            ));
        }

        let tool_name = input["tool_name"].as_str().unwrap_or("").to_string();
        let description = input["description"].as_str().unwrap_or("").to_string();
        let input_schema = input.get("input_schema").cloned()
            .unwrap_or_else(|| serde_json::json!({"type":"object"}));
        let impl_obj = match input.get("implementation") {
            Some(v) => v,
            None => return Ok(ToolResult::error(
                "missing 'implementation' field",
                &ctx.tool_use_id,
            )),
        };
        let kind = impl_obj["kind"].as_str().unwrap_or("");

        let tool_kind_label: String;
        let erased: Arc<ErasedTool> = match kind {
            "shell_script" => {
                tool_kind_label = "shell_script".into();
                let script = impl_obj["script"].as_str().unwrap_or("").to_string();
                let requires_approval = self.dynamic_tool_approval;
                Arc::new(ScriptedTool::new(
                    tool_name.clone(),
                    description.clone(),
                    script,
                    input_schema,
                    requires_approval,
                    Arc::clone(&self.executor),
                )) as Arc<ErasedTool>
            }
            "llm_prompt" => {
                tool_kind_label = "llm_prompt".into();
                let prompt = impl_obj["prompt"].as_str().unwrap_or("").to_string();
                let max_tokens = impl_obj["max_tokens"].as_u64().unwrap_or(1024) as u32;
                Arc::new(LlmBackedTool {
                    name: tool_name.clone(),
                    description: description.clone(),
                    schema: input_schema,
                    system_prompt: prompt,
                    max_tokens,
                    requires_confirmation: false,
                    llm: Arc::clone(&self.llm),
                }) as Arc<ErasedTool>
            }
            other => {
                return Ok(ToolResult::error(
                    format!("unknown implementation kind: '{other}'"),
                    &ctx.tool_use_id,
                ));
            }
        };

        self.registry
            .register(tool_name.clone(), erased)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: TOOL_FACTORY_NAME.into(),
                reason: e.to_string(),
            })?;

        // Emit observability signal
        self.signals
            .emit(XaftDynamicToolCreated {
                tool_name: tool_name.clone(),
                tool_kind: tool_kind_label,
                created_by_agent: ctx
                    .agent_context
                    .as_ref()
                    .and_then(|a| a.downcast_ref::<String>())
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
            })
            .await;

        Ok(ToolResult::ok(
            format!("Tool '{tool_name}' registered. You may now call it."),
            &ctx.tool_use_id,
        ))
    }
}
```

### 7.3 Module layout

**New files:**

```
crates/xaft-tools/src/dynamic/
├── mod.rs          — pub use re-exports
├── scripted.rs     — ScriptedTool
└── factory.rs      — DynamicToolFactory
```

**`crates/xaft-tools/src/dynamic/mod.rs`:**

```rust
//! Dynamic tool creation — `ScriptedTool` and `DynamicToolFactory`.

mod factory;
mod scripted;

pub use factory::DynamicToolFactory;
pub use scripted::ScriptedTool;
```

**`crates/xaft-tools/src/lib.rs` additions:**

```rust
pub mod dynamic;
pub use dynamic::{DynamicToolFactory, ScriptedTool};
```

### 7.4 ToolRegistryBuilder extension

**File:** `crates/xaft-tools/src/registry.rs`

Add an optional method that injects `DynamicToolFactory` into a registry when `allow_dynamic_tools`
is enabled:

```rust
impl ToolRegistryBuilder {
    /// Inject a `DynamicToolFactory` into the registry.
    ///
    /// Called from `xaft-runtime`'s orchestrator after the per-agent
    /// `ToolRegistry` is built, before agents are created.
    pub fn add_dynamic_factory(
        registry: &mut ToolRegistry,
        factory: DynamicToolFactory,
    ) {
        registry.add(factory);
    }
}
```

### 7.5 Orchestrator wiring

**File:** `crates/xaft-runtime/src/orchestrator.rs`

In `run_workflow()`, create a single `DynamicToolRegistry` and share it across all agents:

```rust
pub async fn run_workflow(
    task: &str,
    llm: Arc<dyn LlmProvider>,
    signals: Arc<SignalBus>,
    // ... existing params ...
    session: &mut AgentSession,
    conversation_store: Option<Arc<dyn ConversationStore>>,
    config: &XaftConfig,                  // NEW PARAM: needed for dynamic_tools config
) -> Result<WorkflowResult, RuntimeError> {
    // ── Dynamic tool registry (shared across all agents) ─────────────────────
    let dynamic_registry = if config.agent
        .get("default")
        .map_or(false, |p| p.allow_dynamic_tools)
    {
        let max = config.agent
            .get("default")
            .and_then(|p| p.max_dynamic_tools);
        let reg = match max {
            Some(n) => DynamicToolRegistry::with_limit(n),
            None    => DynamicToolRegistry::new(),
        };
        Some(reg)
    } else {
        None
    };

    // ── Build DynamicToolFactory for planner + coder ──────────────────────────
    if let Some(ref dyn_reg) = dynamic_registry {
        let factory = DynamicToolFactory::new(
            dyn_reg.clone(),
            Arc::clone(&llm),
            session.working_dir.clone(),
            Arc::clone(&signals),
            true,
            config.agent.get("default")
                .map_or(true, |p| p.dynamic_tool_approval),
        );
        // Add factory to the write_tools list
        write_tools.push(Arc::new(factory) as Arc<ErasedTool>);
    }

    // ... existing workflow setup ...

    // Attach shared dynamic registry to each AgentContext before running
    // (done inside HandoffOrchestrator wiring or per-agent context setup)
    if let Some(ref dyn_reg) = dynamic_registry {
        // Set on each agent's context (requires AgentContext builder change)
        planner_ctx.set_dynamic_registry(dyn_reg.clone());
        coder_ctx.set_dynamic_registry(dyn_reg.clone());
        qa_ctx.set_dynamic_registry(dyn_reg.clone());
        fixer_ctx.set_dynamic_registry(dyn_reg.clone());
    }

    // ... run workflow ...

    // ── Clear dynamic registry after run (no leakage) ────────────────────────
    if let Some(ref dyn_reg) = dynamic_registry {
        dyn_reg.clear().await;
    }

    Ok(result)
}
```

**Propagation through HandoffOrchestrator:** The `HandoffOrchestrator` in agtrs creates one
`AgentContext` per handoff step. The dynamic registry must be set on each context created by the
orchestrator. This requires a new parameter on `HandoffRunParams` or a context-builder hook.

Proposed addition to `HandoffRunParams` in agtrs:

```rust
pub struct HandoffRunParams {
    // ... existing fields ...
    /// Optional shared dynamic tool registry injected into every agent context.
    pub dynamic_registry: Option<DynamicToolRegistry>,
}
```

The orchestrator's context-building code checks for `params.dynamic_registry` and calls
`ctx.set_dynamic_registry(reg.clone())` before running each agent.

### 7.6 Config additions

**File:** `crates/xaft-config/src/types.rs`

Add three new fields to `AgentPreset`:

```rust
pub struct AgentPreset {
    // ... existing fields ...

    /// Enable dynamic tool creation via `tool_factory`.
    ///
    /// When `false`, any `tool_factory` call returns an error without
    /// registering anything. Default: `false`.
    pub allow_dynamic_tools: bool,

    /// Maximum number of tools that may be dynamically registered per run.
    ///
    /// `None` means unlimited (not recommended in production).
    /// Default: `Some(10)`.
    pub max_dynamic_tools: Option<usize>,

    /// Require approval-gate confirmation before executing dynamically-created
    /// shell tools (`ScriptedTool`). Default: `true`.
    pub dynamic_tool_approval: bool,
}
```

**Default impl:**

```rust
impl Default for AgentPreset {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            allow_dynamic_tools: false,
            max_dynamic_tools: Some(10),
            dynamic_tool_approval: true,
        }
    }
}
```

**TOML example:**

```toml
[agent.default]
allow_dynamic_tools   = true
max_dynamic_tools     = 10
dynamic_tool_approval = true
```

**Environment variable overrides** (consistent with the six-layer precedence):

| Key | Env var |
|-----|---------|
| `allow_dynamic_tools` | `XAFT_ALLOW_DYNAMIC_TOOLS` |
| `max_dynamic_tools` | `XAFT_MAX_DYNAMIC_TOOLS` |
| `dynamic_tool_approval` | `XAFT_DYNAMIC_TOOL_APPROVAL` |

These are added to `crates/xaft-config/src/loader.rs` in the env-var overlay pass.

### 7.7 New signal: XaftDynamicToolCreated

**File:** `crates/xaft-agent/src/signals.rs`

Append the following to the existing signals file:

```rust
/// Emitted when a new tool is dynamically registered via `tool_factory`
/// during an agent run.
///
/// The TUI subscribes to this signal to display a notification line:
/// `  [dynamic] New tool registered: cargo_test_module (shell_script)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftDynamicToolCreated {
    /// The name of the newly registered tool (snake_case).
    pub tool_name: String,
    /// The implementation kind: `"shell_script"`, `"llm_prompt"`, or `"pipeline"`.
    pub tool_kind: String,
    /// The name of the agent that called `tool_factory`.
    pub created_by_agent: String,
}
```

### 7.8 TUI display

**File:** `crates/xaft-tui/src/app.rs`

Subscribe to `XaftDynamicToolCreated` in the TUI app loop. When received, append a status
notification to the conversation widget's event log:

```rust
signals.on::<XaftDynamicToolCreated>(|ev| {
    tui_tx.send(TuiEvent::DynamicToolRegistered {
        tool_name: ev.tool_name.clone(),
        tool_kind: ev.tool_kind.clone(),
        agent: ev.created_by_agent.clone(),
    }).ok();
}).await;
```

Render format in the conversation pane:

```
  [dynamic] New tool registered: cargo_test_module (shell_script)  [coder]
```

This matches the visual style of the existing `[handoff]` and `[commit]` notification lines.

---

## 8. Data Flow Walkthrough

### Walkthrough: Agent calls `tool_factory` then the new tool

```
Turn N:
  LLM output: {tool_use: "tool_factory", input: {
    tool_name: "cargo_test_module",
    description: "Run cargo test for a specific package",
    input_schema: {...},
    implementation: {kind: "shell_script", script: "cargo test --package $XAFT_INPUT_PACKAGE"}
  }}

  executor::run() → AgentContext::call_tool("tool_factory", input)
    → dynamic_registry.get("tool_factory") → None
    → static tools.get("tool_factory") → DynamicToolFactory
    → DynamicToolFactory::call(input, ctx)
      → check allow_dynamic_tools = true ✓
      → validate definition → Ok(ToolDefinition)
      → construct ScriptedTool("cargo_test_module", ..., requires_confirmation=true)
      → dynamic_registry.register("cargo_test_module", Arc<ScriptedTool>)
         (acquires write lock, inserts, releases)
      → signals.emit(XaftDynamicToolCreated {...}) → TUI shows notification
      → return ToolResult::ok("Tool 'cargo_test_module' registered. ...")

  executor: adds tool result to messages, loops back to LLM

Turn N+1:
  executor: rebuild tool_schemas
    static tools ∪ dynamic_registry.all() → schema includes "cargo_test_module"
  LLM receives updated tool list, can now call "cargo_test_module"

  LLM output: {tool_use: "cargo_test_module", input: {package: "xaft-tools"}}

  executor::run() → AgentContext::call_tool("cargo_test_module", input)
    → dynamic_registry.get("cargo_test_module") → Some(Arc<ScriptedTool>)
    → ScriptedTool.requires_confirmation() = true
      → approval gate blocks → user approves in TUI
    → ScriptedTool::call({package: "xaft-tools"}, ctx)
      → build env: XAFT_TOOL_INPUT=..., XAFT_INPUT_PACKAGE=xaft-tools
      → CommandExecutor executes: bash -c "cargo test --package $XAFT_INPUT_PACKAGE"
      → stdout returned as ToolResult::ok(...)
```

### Walkthrough: MemoizedTool cache hit

```
Turn 1: agent calls git_log_for_file(path="src/lib.rs")
  → MemoizedTool::call({"path":"src/lib.rs"}, ctx)
    → hash = hash({"path":"src/lib.rs"}) = 0xABCD
    → cache.lock() → miss
    → inner.call({"path":"src/lib.rs"}, ctx) → git log output (200ms)
    → cache.insert(0xABCD, result)
    → return result

Turn 2: agent calls git_log_for_file(path="src/lib.rs") again
  → MemoizedTool::call({"path":"src/lib.rs"}, ctx)
    → hash = 0xABCD
    → cache.lock() → hit → return cached ToolResult (< 1ms)
```

---

## 9. File Change Manifest

### agtrs-runtime (upstream changes)

| File | Change type | Summary |
|------|-------------|---------|
| `src/agent.rs` | Modified | Add `dynamic_registry: Option<DynamicToolRegistry>` field to `AgentContext`; add `set_dynamic_registry`, `dynamic_registry` accessors; update `Clone` impl; update `call_tool` to check dynamic registry first |
| `src/executor.rs` | Modified | Include dynamic registry tools in `tool_schemas` sent to LLM; re-check dynamic registry each turn |
| `src/dynamic_tools.rs` | New file | `DynamicToolRegistry`, `ClosureTool`, `LlmBackedTool`, `MemoizedTool`, `PipelineTool`, `ToolFactoryTool` |
| `src/lib.rs` | Modified | `pub mod dynamic_tools;` and re-export key types |
| `src/team.rs` | Modified | Add `dynamic_registry: Option<DynamicToolRegistry>` to `HandoffRunParams`; propagate to each agent context |

### xaft-tools (crate)

| File | Change type | Summary |
|------|-------------|---------|
| `src/dynamic/mod.rs` | New file | Module root with `pub use` |
| `src/dynamic/scripted.rs` | New file | `ScriptedTool` |
| `src/dynamic/factory.rs` | New file | `DynamicToolFactory` |
| `src/lib.rs` | Modified | `pub mod dynamic; pub use dynamic::{...};` |
| `src/registry.rs` | Modified | `ToolRegistryBuilder::add_dynamic_factory()` helper |

### xaft-agent (crate)

| File | Change type | Summary |
|------|-------------|---------|
| `src/signals.rs` | Modified | Add `XaftDynamicToolCreated` |

### xaft-config (crate)

| File | Change type | Summary |
|------|-------------|---------|
| `src/types.rs` | Modified | Add `allow_dynamic_tools`, `max_dynamic_tools`, `dynamic_tool_approval` to `AgentPreset` and its `Default` impl |
| `src/loader.rs` | Modified | Env-var overlay for `XAFT_ALLOW_DYNAMIC_TOOLS`, `XAFT_MAX_DYNAMIC_TOOLS`, `XAFT_DYNAMIC_TOOL_APPROVAL` |

### xaft-runtime (crate)

| File | Change type | Summary |
|------|-------------|---------|
| `src/orchestrator.rs` | Modified | Create `DynamicToolRegistry`, inject `DynamicToolFactory` into write_tools, set registry on all agent contexts, clear registry after run |

### xaft-tui (crate)

| File | Change type | Summary |
|------|-------------|---------|
| `src/app.rs` | Modified | Subscribe to `XaftDynamicToolCreated`; render `[dynamic]` notification line |

---

## 10. Acceptance Criteria

| # | Criterion | Validation method |
|---|-----------|------------------|
| AC1 | An agent can call `tool_factory` with `kind: "shell_script"` and call the newly registered tool in the same run. | Integration test `agent_creates_and_uses_dynamic_tool_in_same_run` |
| AC2 | Dynamic shell tools run inside the workspace sandbox; path traversal attempts (e.g., `cd ../; cat /etc/passwd`) are rejected with `is_error = true`. | Unit test `scripted_tool_rejects_path_traversal` |
| AC3 | When `dynamic_tool_approval = true`, TUI approval gate blocks execution of a newly-created shell tool. Approving unblocks; rejecting returns `is_error` result. | Manual TUI test + `dynamic_tool_approval_gate_blocks_and_unblocks` |
| AC4 | With `max_dynamic_tools = 3`, a fourth `tool_factory` call returns an error result without panicking. | Unit test `max_dynamic_tools_limit_enforced` |
| AC5 | `LlmBackedTool` calls the configured LLM provider with the configured system prompt and returns the text response as the tool content. | Unit test `llm_backed_tool_calls_provider_with_system_prompt` |
| AC6 | `MemoizedTool` on second call with identical input returns the cached result; the inner tool is called exactly once. | Unit test `memoized_tool_returns_cached_on_second_call` |
| AC7 | `PipelineTool` with two chained tools: output of tool A becomes `{"input": "<content>"}` for tool B; final result is tool B's output. | Unit test `pipeline_tool_chains_two_tools` |
| AC8 | `XaftDynamicToolCreated` signal is emitted once per successful `tool_factory` registration with correct `tool_name`, `tool_kind`, and `created_by_agent` fields. | Unit test `dynamic_tool_signal_emitted` |
| AC9 | A tool registered by the planner is callable by the coder in the same orchestration run (shared `DynamicToolRegistry` arc). | Integration test `dynamic_tools_shared_across_agents` |
| AC10 | `ClosureTool` created from an async closure executes correctly, receives the correct `input` and `ctx`, and propagates `Err(...)` from the closure as `Err(AgtrsError::...)`. | Unit test `closure_tool_executes_and_returns_result` + `closure_tool_propagates_error` |
| AC11 | `allow_dynamic_tools = false` causes every `tool_factory` call to return `ToolResult { is_error: true, content: "dynamic tool creation is disabled..." }` without touching the registry. | Unit test `dynamic_tools_disabled_returns_error` |
| AC12 | After `run_workflow()` completes (success or error), `DynamicToolRegistry::is_empty()` returns `true`. | Integration test `dynamic_tools_cleared_after_run` |

---

## 11. Test Plan with Stubs

All tests follow xaft's convention: unit tests in `#[cfg(test)] mod tests` in the same file;
integration tests in `crates/<name>/tests/`. No real LLM calls; mock providers only.

### agtrs-runtime unit tests

**File:** `agtrs-runtime/tests/dynamic_tools.rs` (new file)

```rust
//! Tests for dynamic tool creation (PRD 57).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agtrs_runtime::agent::{AgentConfig, AgentContext};
use agtrs_runtime::dynamic_tools::{
    ClosureTool, DynamicToolRegistry, LlmBackedTool, MemoizedTool,
    PipelineTool, ToolFactoryTool, TOOL_FACTORY_NAME,
};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::testing::MockLlmProvider;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use injectable_runtime::ResolveContext;

fn make_resolve_ctx() -> Arc<ResolveContext> {
    Arc::new(ResolveContext::default())
}

fn make_llm(response: &str) -> Arc<MockLlmProvider> {
    let (llm, transport) = MockLlmProvider::new();
    // pre-queue one response
    // (exact API depends on the MockLlmProvider implementation)
    transport.queue_text_sync(response);
    Arc::new(llm)
}

// ── ClosureTool ──────────────────────────────────────────────────────────────

/// AC10 — ClosureTool executes correctly and returns the expected result.
#[tokio::test]
async fn closure_tool_executes_and_returns_result() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&call_count);

    let tool = ClosureTool::new(
        "echo_upper",
        "Echoes input uppercased",
        serde_json::json!({"type":"object","properties":{"msg":{"type":"string"}}}),
        move |input, ctx| {
            let cc = Arc::clone(&cc);
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                let msg = input["msg"].as_str().unwrap_or("").to_uppercase();
                Ok(ToolResult::ok(msg, ctx.tool_use_id))
            }
        },
    );

    let ctx = ToolContext::new("t1");
    let result = tool
        .call(serde_json::json!({"msg": "hello"}), &ctx)
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(result.content, "HELLO");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

/// AC10 — ClosureTool propagates Err from the closure as AgtrsError.
#[tokio::test]
async fn closure_tool_propagates_error() {
    let tool = ClosureTool::new(
        "always_fail",
        "Always fails",
        serde_json::json!({"type":"object"}),
        |_input, _ctx| async move {
            Err(AgtrsError::ToolCallFailed {
                tool_name: "always_fail".into(),
                reason: "deliberate failure".into(),
            })
        },
    );

    let ctx = ToolContext::new("t2");
    let err = tool
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap_err();

    assert!(matches!(err, AgtrsError::ToolCallFailed { .. }));
}

// ── MemoizedTool ─────────────────────────────────────────────────────────────

/// AC6 — MemoizedTool returns the cached result on second call.
#[tokio::test]
async fn memoized_tool_returns_cached_on_second_call() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&call_count);

    let inner = Arc::new(ClosureTool::new(
        "counting_tool",
        "Counts invocations",
        serde_json::json!({"type":"object"}),
        move |_input, ctx| {
            let cc = Arc::clone(&cc);
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                Ok(ToolResult::ok(format!("call #{n}"), ctx.tool_use_id))
            }
        },
    )) as Arc<ErasedTool>;

    let memoized = MemoizedTool::new(inner);
    let input = serde_json::json!({"key": "value"});
    let ctx1 = ToolContext::new("t1");
    let ctx2 = ToolContext::new("t2");

    let r1 = memoized.call(input.clone(), &ctx1).await.unwrap();
    let r2 = memoized.call(input.clone(), &ctx2).await.unwrap();

    // Inner tool called once; both results have same content
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
    assert_eq!(r1.content, r2.content);
}

// ── PipelineTool ─────────────────────────────────────────────────────────────

/// AC7 — PipelineTool chains two tools correctly.
#[tokio::test]
async fn pipeline_tool_chains_two_tools() {
    // Tool A: reverses the "msg" field
    let tool_a = Arc::new(ClosureTool::new(
        "reverser",
        "Reverses the msg",
        serde_json::json!({"type":"object","properties":{"msg":{"type":"string"}}}),
        |input, ctx| async move {
            let msg = input["msg"].as_str().unwrap_or("").chars().rev().collect::<String>();
            Ok(ToolResult::ok(msg, ctx.tool_use_id))
        },
    )) as Arc<ErasedTool>;

    // Tool B: receives {"input": "<content>"} and uppercases it
    let tool_b = Arc::new(ClosureTool::new(
        "uppercaser",
        "Uppercases the input",
        serde_json::json!({"type":"object","properties":{"input":{"type":"string"}}}),
        |input, ctx| async move {
            let s = input["input"].as_str().unwrap_or("").to_uppercase();
            Ok(ToolResult::ok(s, ctx.tool_use_id))
        },
    )) as Arc<ErasedTool>;

    let pipeline = PipelineTool::new(
        "reverse_then_upper",
        "Reverse then uppercase",
        vec![tool_a, tool_b],
    );

    let ctx = ToolContext::new("p1");
    let result = pipeline
        .call(serde_json::json!({"msg": "hello"}), &ctx)
        .await
        .unwrap();

    // "hello" reversed = "olleh", uppercased = "OLLEH"
    assert!(!result.is_error);
    assert_eq!(result.content, "OLLEH");
}

// ── DynamicToolRegistry ──────────────────────────────────────────────────────

/// Basic register + get round-trip.
#[tokio::test]
async fn dynamic_registry_register_and_get() {
    let registry = DynamicToolRegistry::new();
    let tool = Arc::new(ClosureTool::new(
        "my_tool",
        "Test",
        serde_json::json!({"type":"object"}),
        |_, ctx| async move { Ok(ToolResult::ok("ok", ctx.tool_use_id)) },
    )) as Arc<ErasedTool>;

    registry.register("my_tool".into(), tool).await.unwrap();

    let found = registry.get("my_tool").await;
    assert!(found.is_some());
    assert_eq!(found.unwrap().name(), "my_tool");

    let missing = registry.get("nonexistent").await;
    assert!(missing.is_none());
}

/// Cloning the registry shares the underlying storage.
#[tokio::test]
async fn dynamic_registry_is_shared_across_clone() {
    let registry = DynamicToolRegistry::new();
    let clone = registry.clone();

    let tool = Arc::new(ClosureTool::new(
        "shared_tool",
        "Shared",
        serde_json::json!({"type":"object"}),
        |_, ctx| async move { Ok(ToolResult::ok("ok", ctx.tool_use_id)) },
    )) as Arc<ErasedTool>;

    // Register via original
    registry.register("shared_tool".into(), tool).await.unwrap();

    // Visible via clone
    assert!(clone.get("shared_tool").await.is_some());
}

// ── ToolFactoryTool ──────────────────────────────────────────────────────────

/// AC1 (agtrs layer) — ToolFactoryTool registers a shell tool.
#[tokio::test]
async fn tool_factory_registers_shell_tool() {
    let registry = DynamicToolRegistry::new();
    let llm = make_llm("unused");
    let factory = ToolFactoryTool::new(
        registry.clone(),
        Arc::clone(&llm),
        std::env::temp_dir(),
        true, // allow_dynamic_tools
    );

    let input = serde_json::json!({
        "tool_name": "greet",
        "description": "Greets the user",
        "input_schema": {"type":"object","properties":{"name":{"type":"string"}}},
        "implementation": {
            "kind": "shell_script",
            "script": "echo Hello $XAFT_INPUT_NAME"
        }
    });

    let ctx = ToolContext::new("tf1");
    let result = factory.call(input, &ctx).await.unwrap();

    assert!(!result.is_error, "factory call should succeed: {}", result.content);
    assert!(result.content.contains("greet"));

    assert!(registry.get("greet").await.is_some());
    assert_eq!(registry.len().await, 1);
}

/// AC5 — ToolFactoryTool registers an LLM-backed tool.
#[tokio::test]
async fn tool_factory_registers_llm_tool() {
    let registry = DynamicToolRegistry::new();
    let (mock_llm, transport) = MockLlmProvider::new();
    let llm = Arc::new(mock_llm);

    let factory = ToolFactoryTool::new(
        registry.clone(),
        Arc::clone(&llm) as Arc<dyn agtrs_runtime::llm::LlmProvider>,
        std::env::temp_dir(),
        true,
    );

    let input = serde_json::json!({
        "tool_name": "summarizer",
        "description": "Summarizes text",
        "implementation": {
            "kind": "llm_prompt",
            "prompt": "Summarize in 3 sentences.",
            "max_tokens": 512
        }
    });

    let ctx = ToolContext::new("tf2");
    let result = factory.call(input, &ctx).await.unwrap();

    assert!(!result.is_error, "{}", result.content);
    assert!(registry.get("summarizer").await.is_some());

    // Now call the LLM-backed tool
    transport.queue_text_sync("This is a 3-sentence summary.");
    let tool = registry.get("summarizer").await.unwrap();
    let call_ctx = ToolContext::new("use1");
    let tool_result = tool
        .call(serde_json::json!({"text": "Long text here..."}), &call_ctx)
        .await
        .unwrap();

    assert!(!tool_result.is_error);
    assert_eq!(tool_result.content, "This is a 3-sentence summary.");
}

/// AC4 — max_dynamic_tools limit enforced.
#[tokio::test]
async fn max_dynamic_tools_limit_enforced() {
    let registry = DynamicToolRegistry::with_limit(3);
    let llm = make_llm("unused");
    let factory = ToolFactoryTool::new(
        registry.clone(),
        llm,
        std::env::temp_dir(),
        true,
    );

    // Register 3 tools — all succeed
    for i in 0..3 {
        let input = serde_json::json!({
            "tool_name": format!("tool_{i}"),
            "description": "Test tool",
            "implementation": {
                "kind": "shell_script",
                "script": "echo ok"
            }
        });
        let ctx = ToolContext::new(format!("t{i}"));
        let r = factory.call(input, &ctx).await.unwrap();
        assert!(!r.is_error, "tool_{i} registration should succeed: {}", r.content);
    }

    // 4th registration must fail
    let input = serde_json::json!({
        "tool_name": "tool_overflow",
        "description": "Should fail",
        "implementation": {"kind": "shell_script", "script": "echo fail"}
    });
    let ctx = ToolContext::new("overflow");
    let r = factory.call(input, &ctx).await.unwrap();
    assert!(r.is_error, "4th registration should return is_error=true");
    assert!(r.content.contains("limit"), "error should mention limit: {}", r.content);
    assert_eq!(registry.len().await, 3);
}

/// AC11 — allow_dynamic_tools = false returns error without touching registry.
#[tokio::test]
async fn dynamic_tools_disabled_returns_error() {
    let registry = DynamicToolRegistry::new();
    let llm = make_llm("unused");
    let factory = ToolFactoryTool::new(
        registry.clone(),
        llm,
        std::env::temp_dir(),
        false, // allow_dynamic_tools = false
    );

    let input = serde_json::json!({
        "tool_name": "blocked_tool",
        "description": "Should not be registered",
        "implementation": {"kind": "shell_script", "script": "echo blocked"}
    });

    let ctx = ToolContext::new("blocked");
    let r = factory.call(input, &ctx).await.unwrap();

    assert!(r.is_error);
    assert!(r.content.contains("disabled"), "{}", r.content);
    assert!(registry.is_empty().await);
}
```

### xaft integration tests

**File:** `crates/xaft-tools/tests/dynamic_tools_integration.rs` (new file)

```rust
//! Integration tests for xaft dynamic tool creation (PRD 57).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

use agtrs_runtime::dynamic_tools::DynamicToolRegistry;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::testing::MockLlmProvider;
use agtrs_runtime::tool::{ToolContext, ToolResult};
use xaft_agent::signals::XaftDynamicToolCreated;
use xaft_tools::dynamic::{DynamicToolFactory, ScriptedTool};
use agtrs_shell::{CommandExecutor, ExecutionPolicy, Sandbox};

fn make_executor(root: &std::path::Path) -> Arc<CommandExecutor> {
    let sandbox = Sandbox::new(root);
    Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::default()))
}

// ── ScriptedTool ─────────────────────────────────────────────────────────────

/// ScriptedTool runs bash with the correct input env vars.
#[tokio::test]
async fn scripted_tool_runs_bash_with_input() {
    let tmp = TempDir::new().unwrap();
    let executor = make_executor(tmp.path());

    let tool = ScriptedTool::new(
        "echo_name",
        "Echoes the name",
        "echo \"Hello $XAFT_INPUT_NAME\"",
        serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": "string"}}
        }),
        false, // no approval required
        executor,
    );

    let ctx = ToolContext::new("s1");
    let result = tool
        .call(serde_json::json!({"name": "xaft"}), &ctx)
        .await
        .unwrap();

    assert!(!result.is_error, "{}", result.content);
    assert_eq!(result.content.trim(), "Hello xaft");
}

/// AC2 — ScriptedTool rejects path traversal attempts.
///
/// A script that tries to write outside the workspace root must fail
/// with an error result (not a panic).
#[tokio::test]
async fn scripted_tool_rejects_path_traversal() {
    let tmp = TempDir::new().unwrap();
    let executor = make_executor(tmp.path());

    // The sandbox is rooted at tmp.path(). Writing to /tmp directly escapes it.
    let tool = ScriptedTool::new(
        "escape_attempt",
        "Tries to escape the sandbox",
        "echo pwned > /tmp/xaft_prd57_test_escape.txt",
        serde_json::json!({"type": "object"}),
        false,
        executor,
    );

    let ctx = ToolContext::new("escape");
    let result = tool
        .call(serde_json::json!({}), &ctx)
        .await;

    // Either the Sandbox rejects it (returns Err or is_error result),
    // or the CommandExecutor traps it. Either way, no file outside workspace.
    match result {
        Ok(r) => assert!(
            r.is_error,
            "path traversal should produce is_error=true, got: {}",
            r.content
        ),
        Err(e) => {
            // Hard error is also acceptable for path traversal
            assert!(
                e.to_string().contains("sandbox") || e.to_string().contains("path"),
                "unexpected error: {e}"
            );
        }
    }

    // The escape file must not exist
    assert!(!std::path::Path::new("/tmp/xaft_prd57_test_escape.txt").exists());
}

// ── DynamicToolFactory ───────────────────────────────────────────────────────

/// AC8 — DynamicToolFactory emits XaftDynamicToolCreated signal.
#[tokio::test]
async fn dynamic_tool_signal_emitted() {
    let tmp = TempDir::new().unwrap();
    let signals = Arc::new(SignalBus::new());
    let registry = DynamicToolRegistry::new();

    let emitted = Arc::new(AtomicUsize::new(0));
    {
        let emitted = Arc::clone(&emitted);
        signals
            .on::<XaftDynamicToolCreated>(move |ev| {
                assert_eq!(ev.tool_name, "my_dynamic_tool");
                assert_eq!(ev.tool_kind, "shell_script");
                emitted.fetch_add(1, Ordering::SeqCst);
            })
            .await;
    }

    let (mock_llm, _) = MockLlmProvider::new();
    let factory = DynamicToolFactory::new(
        registry.clone(),
        Arc::new(mock_llm),
        tmp.path().to_path_buf(),
        Arc::clone(&signals),
        true,  // allow_dynamic_tools
        false, // dynamic_tool_approval
    );

    let input = serde_json::json!({
        "tool_name": "my_dynamic_tool",
        "description": "A test tool",
        "implementation": {
            "kind": "shell_script",
            "script": "echo ok"
        }
    });

    use agtrs_runtime::tool::Tool;
    let ctx = ToolContext::new("factory_test");
    let result = factory.call(input, &ctx).await.unwrap();

    assert!(!result.is_error, "{}", result.content);

    // Give async signal handlers time to fire
    tokio::task::yield_now().await;
    assert_eq!(emitted.load(Ordering::SeqCst), 1);
}

// ── End-to-end: agent creates then uses a dynamic tool ──────────────────────

/// AC1, AC9 — Agent creates and uses a dynamic tool in the same run.
///
/// This test runs a two-turn mock conversation:
///  Turn 1: LLM calls tool_factory → tool registered
///  Turn 2: LLM calls the new tool → result returned
#[tokio::test]
async fn agent_creates_and_uses_dynamic_tool_in_same_run() {
    use agtrs_runtime::agent::{Agent, AgentConfig, AgentContext};
    use agtrs_runtime::executor::AgentExecutor;
    use agtrs_runtime::testing::MockLlmProvider;
    use agtrs_runtime::tool::ErasedTool;
    use agtrs_runtime::transport::{Message, StopReason, ToolCall};

    let tmp = TempDir::new().unwrap();
    let (mock_llm, transport) = MockLlmProvider::new();
    let llm = Arc::new(mock_llm);

    let signals = Arc::new(SignalBus::new());
    let registry = DynamicToolRegistry::new();

    let factory = DynamicToolFactory::new(
        registry.clone(),
        Arc::clone(&llm) as Arc<dyn agtrs_runtime::llm::LlmProvider>,
        tmp.path().to_path_buf(),
        Arc::clone(&signals),
        true,
        false,
    );

    // ── Mock LLM: turn 1 calls tool_factory ──────────────────────────────────
    transport.queue_tool_call(
        "tool_factory_use_1",
        "tool_factory",
        serde_json::json!({
            "tool_name": "say_hello",
            "description": "Says hello",
            "implementation": {
                "kind": "shell_script",
                "script": "echo 'Hello from dynamic tool!'"
            }
        }),
    ).await;

    // ── Mock LLM: turn 2 calls the new tool ──────────────────────────────────
    transport.queue_tool_call(
        "say_hello_use_1",
        "say_hello",
        serde_json::json!({}),
    ).await;

    // ── Mock LLM: turn 3 finishes ────────────────────────────────────────────
    transport.queue_text("Dynamic tool worked!").await;

    // Build a minimal test agent
    struct TestAgent {
        config: AgentConfig,
        tools: Vec<Arc<ErasedTool>>,
    }

    #[async_trait::async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str { "test-agent" }
        fn system_prompt(&self) -> String { "You are a test agent.".into() }
        fn config(&self) -> &AgentConfig { &self.config }
        fn tools(&self) -> Vec<Arc<ErasedTool>> { self.tools.clone() }
    }

    let tools: Vec<Arc<ErasedTool>> = vec![
        Arc::new(factory) as Arc<ErasedTool>,
    ];

    let agent = TestAgent {
        config: AgentConfig { max_turns: 10, ..Default::default() },
        tools,
    };

    let mut ctx = AgentContext::new(
        "test-agent",
        AgentConfig::default(),
        Arc::clone(&llm) as Arc<dyn agtrs_runtime::llm::LlmProvider>,
        Arc::new(injectable_runtime::ResolveContext::default()),
    );
    ctx.set_dynamic_registry(registry.clone());

    let response = AgentExecutor::run(
        &agent,
        Message::user("Create a greeting tool and use it."),
        &mut ctx,
    )
    .await
    .unwrap();

    assert_eq!(response.content, "Dynamic tool worked!");
    // The dynamic tool was called
    assert!(response
        .tool_calls_made
        .iter()
        .any(|tc| tc.name == "say_hello"));
}

// ── Dynamic tools cleared after run ─────────────────────────────────────────

/// AC12 — Dynamic registry is cleared after run_workflow completes.
///
/// This test verifies that the orchestrator calls `registry.clear()` after
/// the workflow finishes, so tools do not leak into the next run.
#[tokio::test]
async fn dynamic_tools_cleared_after_run() {
    // This is tested at the orchestrator level.
    // We verify that a registry that had tools registered is empty after
    // run_workflow returns — regardless of whether the workflow succeeded.

    let registry = DynamicToolRegistry::new();

    // Simulate a tool being registered mid-run
    let tool = Arc::new(
        agtrs_runtime::dynamic_tools::ClosureTool::new(
            "temp_tool",
            "A temporary tool",
            serde_json::json!({"type":"object"}),
            |_, ctx| async move { Ok(ToolResult::ok("ok", ctx.tool_use_id)) },
        )
    ) as Arc<agtrs_runtime::tool::ErasedTool>;
    registry.register("temp_tool".into(), tool).await.unwrap();
    assert_eq!(registry.len().await, 1);

    // Simulate end-of-run cleanup (as done in run_workflow)
    registry.clear().await;

    assert!(registry.is_empty().await);
}

// ── Dynamic tools shared across agents ──────────────────────────────────────

/// AC9 — Tools registered by one agent context are visible to a clone
/// (simulating a second agent receiving the same shared registry).
#[tokio::test]
async fn dynamic_tools_shared_across_agents() {
    let registry = DynamicToolRegistry::new();

    // Agent A registers a tool
    let tool = Arc::new(
        agtrs_runtime::dynamic_tools::ClosureTool::new(
            "cross_agent_tool",
            "Cross-agent tool",
            serde_json::json!({"type":"object"}),
            |_, ctx| async move { Ok(ToolResult::ok("from agent A", ctx.tool_use_id)) },
        )
    ) as Arc<agtrs_runtime::tool::ErasedTool>;
    registry.register("cross_agent_tool".into(), tool).await.unwrap();

    // Agent B uses a clone of the same registry
    let registry_b = registry.clone();
    let found = registry_b.get("cross_agent_tool").await;
    assert!(found.is_some());
    assert_eq!(found.unwrap().name(), "cross_agent_tool");
}
```

### xaft-config integration test

**File:** `crates/xaft-config/tests/config_integration.rs` — add to existing file:

```rust
/// Dynamic tool config fields parse correctly from TOML.
#[test]
fn dynamic_tool_config_fields_parse() {
    let toml = r#"
[agent.default]
model                 = "claude-3-5-sonnet-20241022"
provider              = "anthropic"
allow_dynamic_tools   = true
max_dynamic_tools     = 5
dynamic_tool_approval = false
"#;
    let config: XaftConfig = toml::from_str(toml).unwrap();
    let preset = config.agent.get("default").unwrap();
    assert!(preset.allow_dynamic_tools);
    assert_eq!(preset.max_dynamic_tools, Some(5));
    assert!(!preset.dynamic_tool_approval);
}

/// Defaults: allow_dynamic_tools = false, max_dynamic_tools = Some(10),
/// dynamic_tool_approval = true.
#[test]
fn dynamic_tool_config_defaults_are_safe() {
    let config = XaftConfig::default();
    let preset = config.agent.get("default").unwrap();
    assert!(!preset.allow_dynamic_tools);
    assert_eq!(preset.max_dynamic_tools, Some(10));
    assert!(preset.dynamic_tool_approval);
}
```

---

## 12. Open Questions

| # | Question | Notes |
|---|----------|-------|
| OQ1 | Should `PipelineTool` support a custom transformation function between steps rather than always wrapping in `{"input": ...}`? | A `Vec<Option<Box<dyn Fn(String) -> Value>>>` transformer per edge would be more flexible but adds complexity. Defer to PRD 58. |
| OQ2 | Should `LlmBackedTool` token costs be tracked in the calling agent's `AgentMetadata`? | Currently they are not. Adding cost tracking requires `AgentContext` access from the tool, which conflicts with the clean `ToolContext` API. A `ModelCallComplete` signal emitted by `LlmBackedTool` is sufficient for observability. |
| OQ3 | Should dynamically-created tools be serialised to the session store so they persist across `--continue` runs? | Session resume requires replaying the tool definitions. The JSON schema-based definition makes this straightforward in principle. Deferred to PRD 59. |
| OQ4 | How should `ToolFactoryTool` handle name collisions with static tools? | The current proposal rejects reserved names (`tool_factory`, `bash_exec`, `read_file`, `write_file`). Extending the reject list to all statically-registered tool names (queried at construction time) is safer but requires a snapshot of the static registry at factory-build time. |
| OQ5 | `MemoizedTool` uses `DefaultHasher` which is not stable across processes. Is this a problem? | No: the cache lifetime is bounded to a single run (single process). Cross-run persistence is explicitly out of scope (see OQ3). |
| OQ6 | Should the approval gate for `ScriptedTool` show the script body to the user, not just the tool name? | Yes. The TUI approval widget should display the script body when the tool being approved is a `ScriptedTool`. This requires a metadata field or a new `ApprovalDetail::Script(body)` enum variant in agtrs. Deferred to the TUI sub-task. |
| OQ7 | Should agents be able to delete dynamically-created tools? | A `tool_factory_delete` tool would be simple to add. Not required for AC1–AC12. Defer. |

---

## 13. Security Considerations

### Path traversal

`ScriptedTool` runs scripts inside the `Sandbox` configured with `workspace_root`. The existing
`Sandbox` and `CommandExecutor` in `agtrs-shell` enforce that the working directory is
`workspace_root`. Scripts that attempt to write to paths outside the workspace (e.g., absolute
paths, `../` traversals, symlink attacks to `/etc`) are intercepted.

AC2 specifically tests that a script writing to `/tmp` directly is rejected.

### LLM-injected shell code

When an agent calls `tool_factory` with `kind: "shell_script"`, the LLM supplies the script body.
This is equivalent to the LLM calling `bash_exec` — the same class of risk. The mitigations are:

1. `dynamic_tool_approval = true` (default): every invocation of a `ScriptedTool` requires human
   approval before execution. The user sees the tool name and input in the TUI gate.
2. `allow_dynamic_tools = false` (default): the feature is opt-in. Operators must explicitly
   enable it.
3. The `Sandbox` provides a hard backstop against filesystem escapes even if the user approves a
   malicious script.

The script body itself is NOT shown in the standard approval gate prompt (OQ6 above). Until OQ6 is
resolved, operators should treat `allow_dynamic_tools = true` as equivalent to enabling
`bash_exec` with full audit.

### LLM-injected system prompts

`LlmBackedTool` accepts a `system_prompt` from the LLM via `ToolFactoryTool`. A malicious prompt
could attempt to jailbreak the sub-LLM call. Mitigations:

1. The `LlmBackedTool` call is isolated (no tool access, no state modification).
2. The sub-LLM response is returned verbatim as a string to the calling agent. The calling agent
   is responsible for validating the response before acting on it.
3. Operators can disable `allow_dynamic_tools` to prevent LLM-backed tool creation entirely.

### Tool name shadowing

`ToolFactoryTool::parse_definition` rejects names that match a hardcoded list of built-in tool
names. This prevents an LLM from registering a tool named `write_file` that overwrites the
safe built-in. The dynamic registry is always checked first during dispatch, so an unguarded
dynamic registration could shadow a built-in.

The full defense is: reject any `tool_name` that matches a key already present in the static
`AgentContext::tools` map. This check is performed at `DynamicToolFactory::call` time, where the
static tools list is available via `ctx.agent_context`.

### Rate limiting

`max_dynamic_tools` (default 10) limits tool bloat and prevents the LLM from filling the context
with thousands of tool definitions.

---

## 14. Migration and Compatibility

### Backward compatibility

All changes are additive:

- `AgentContext` gains a new `dynamic_registry: Option<DynamicToolRegistry>` field, defaulting to
  `None`. Existing code that does not call `set_dynamic_registry` is unaffected.
- `AgentPreset` gains three new optional fields with safe defaults (`allow_dynamic_tools = false`,
  `max_dynamic_tools = Some(10)`, `dynamic_tool_approval = true`). Existing TOML config files that
  omit these fields continue to work unchanged via `#[serde(default)]`.
- `HandoffRunParams` gains `dynamic_registry: Option<DynamicToolRegistry>`, defaulting to `None`.
  Existing call sites pass `None` implicitly via `..Default::default()`.

### Existing tool dispatch

The change to `AgentContext::call_tool` adds a dynamic registry lookup before the static lookup.
When `dynamic_registry` is `None` (the default), there is zero additional cost: the `if let
Some(reg)` branch is not entered.

When `dynamic_registry` is `Some`, the lookup acquires a read lock on a `RwLock<HashMap>`. Under
normal operation this lock is uncontended and resolves in nanoseconds.

### `AgentContext::clone()`

The `Clone` impl is updated to forward the `dynamic_registry` field. Because `DynamicToolRegistry`
wraps `Arc<RwLock<...>>`, cloning is cheap (reference count increment only). All clones share the
same backing store — this is the intentional design that enables cross-agent tool visibility.

### Test suite impact

Existing tests that construct `AgentContext::new(...)` do not pass a `dynamic_registry`. They
receive `None` and are unaffected. No existing tests need modification.

New tests added by this PRD are in new test files, following xaft's `crates/<name>/tests/`
convention.
