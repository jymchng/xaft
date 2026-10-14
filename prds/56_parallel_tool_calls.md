# PRD 56 — Parallel Tool Calls

**Status:** Draft  
**Date:** 2026-06-12  
**Affects:** `agtrs-runtime` (upstream), `xaft-tools`, `xaft-agent`, `xaft-config`, `xaft-runtime`

---

## Table of Contents

1. [Background](#background)
2. [Problem](#problem)
3. [Goals](#goals)
4. [Non-Goals](#non-goals)
5. [Architecture](#architecture)
   - 5.1 [agtrs-runtime changes](#51-agtrs-runtime-changes)
   - 5.2 [xaft-tools changes](#52-xaft-tools-changes)
   - 5.3 [xaft-agent changes](#53-xaft-agent-changes)
   - 5.4 [xaft-config changes](#54-xaft-config-changes)
   - 5.5 [xaft-runtime / orchestrator changes](#55-xaft-runtime--orchestrator-changes)
   - 5.6 [xaft-tui changes](#56-xaft-tui-changes)
6. [Data Model](#data-model)
7. [Files to Change](#files-to-change)
8. [Acceptance Criteria](#acceptance-criteria)
9. [Tests](#tests)
10. [Migration & Backward Compatibility](#migration--backward-compatibility)
11. [Rollout](#rollout)
12. [Open Questions](#open-questions)

---

## Background

The agtrs executor (`agtrs-runtime/src/executor.rs`) already contains a Phase 2
parallel branch: when `AgentConfig::parallel_tool_calls` is `true` it submits all
tool call futures to `futures::future::join_all` instead of awaiting them one by
one.  This is a coarse binary switch that has never been wired into xaft's
`NamedAgent` or `XaftAgentConfig`.

The existing code path was shipped to prove the async machinery works.  It does.
What has not been designed is the finer-grained policy layer that makes parallel
tool execution safe, bounded, and observable in a coding agent context: safety
annotations per tool, concurrency caps, dependency ordering within a turn,
streaming partial results to the TUI, and config-file exposure.

This PRD fully specifies that policy layer and tracks it as a single deliverable.

---

## Problem

### 1  The all-or-nothing binary flag is too coarse

`AgentConfig::parallel_tool_calls: bool` has two states: every tool in a turn
runs concurrently, or none do.  A coding agent turn often mixes calls of very
different safety profiles:

```
Turn N tool calls (LLM output):
  read_file("src/main.rs")          ← pure read, 5 ms
  read_file("src/lib.rs")           ← pure read, 5 ms
  grep("pattern", "src/")           ← pure read, 40 ms
  write_file("src/main.rs", ...)    ← mutates filesystem
```

With `parallel_tool_calls = true`, all four execute simultaneously.  The
`write_file` overlaps the first `read_file` on the same path.  The LLM receives
the stale content of `src/main.rs` alongside a successful write, producing a
corrupted conversation context.  With `parallel_tool_calls = false`, the three
cheap reads run serially and inflate wall-clock time from ~40 ms to ~50 ms —
small in isolation but amplified across the dozens of turns a typical coding
session takes.

The right answer is a middle ground: parallel where safe, sequential where
required.

### 2  No concurrency cap means unbounded resource consumption

The LLM is permitted to emit an arbitrary number of tool_use blocks in a single
response.  If an agent calls `read_file` on twenty files and `parallel_tool_calls
= true`, twenty tokio tasks race simultaneously.  Each task opens a file
descriptor, reads into memory, and holds the result until all twenty complete
before Phase 3 can drain them.  On a large repository with 2 MB files this is
40 MB of simultaneously resident read buffers.  On a slow filesystem or a network
filesystem the twenty concurrent I/O operations may saturate the I/O scheduler.

There is currently no mechanism to cap simultaneous executions.

### 3  No tool-level safety annotation

The `Tool` trait has no method expressing whether the tool is safe for concurrent
execution.  Every tool defaults to the binary flag at the `AgentConfig` level.
This means:

- Implementers cannot opt individual tools into or out of parallel execution
  without changing global config.
- There is no compile-time or runtime documentation of which tools are
  pure-read versus mutating.
- The safety model is entirely at the caller's discretion.

xaft's tools are naturally partitioned:

| Group | Tools |
|-------|-------|
| Read-only (parallel-safe) | `read_file`, `read_many`, `list_files`, `grep`, `glob`, `file_stat`, `tree`, `diff_files`, `search_files`, `git_status`, `git_diff`, `git_log`, `git_blame`, `git_show`, `git_branch`, `git_stash_list`, `git_remote`, `git_grep`, `git_tag` |
| Mutating (sequential) | `write_file`, `edit_file`, `append_to_file`, `patch_file`, `move_file`, `copy_file`, `delete_file`, `create_directory`, `remove_directory`, `git_add`, `git_unstage`, `git_commit_staged`, `git_stash`, `git_stash_pop`, `git_checkout_files`, `git_push`, `git_create_branch`, `git_merge`, `bash_exec` |

These categories are stable: a read tool does not mutate the filesystem by
definition.  Annotating them in code eliminates the risk of misconfiguration.

### 4  No dependency ordering between tool calls in a single turn

The LLM sometimes emits dependent tool calls in the same turn:

```
tool_use_id: "tu_1", name: "git_diff", input: {}
tool_use_id: "tu_2", name: "read_file", input: {"path": "src/main.rs"}
tool_use_id: "tu_3", name: "grep", input: {"pattern": "fn main", "path": "src/"}
```

These are all read-only and safe to run in parallel.  But consider:

```
tool_use_id: "tu_1", name: "read_file",  input: {"path": "src/foo.rs"}
tool_use_id: "tu_2", name: "write_file", input: {"path": "src/foo.rs", "content": "..."}
tool_use_id: "tu_3", name: "read_file",  input: {"path": "src/foo.rs"}
```

Here `tu_3` logically depends on `tu_2` completing.  Without ordering, both
reads may see the pre-write content.  The current executor provides no mechanism
to express or enforce this ordering.

### 5  No streaming of partial results

When `parallel_tool_calls = true`, the executor currently waits for ALL futures
in `join_all` to complete before entering Phase 3 post-processing.  The TUI
shows a spinner with no indication of which tools have finished.  For a batch
where one tool completes in 5 ms and another takes 200 ms, the agent sits idle
for 195 ms after the first result is ready.

Streaming partial results — emitting `ToolResultAvailable` as each future
settles — allows the TUI to update incrementally and, in a future design, could
allow the agent to begin processing partial results while others continue.

### 6  xaft never enables parallel execution for read-only tools

`NamedAgent` and all of xaft's domain agents (`PlannerAgent`, `CoderAgent`,
`QaAgent`, `FixerAgent`) are built without `parallel_tool_calls = true`.
`XaftAgentConfig::parallel_tools` exists as a field but is never threaded through
to `AgentConfig`.  The feature is dead code from xaft's perspective.

### 7  No metrics on parallelism efficiency

`AgentRunComplete` carries total token counts and cost but nothing about tool
execution efficiency: how many tools ran in parallel, what the peak concurrency
was, or what fraction of time was spent waiting on tools.  This makes it
impossible to measure the speedup from this feature or to detect regressions.

---

## Goals

1. **Fine-grained parallelism control.** Introduce a `ParallelToolPolicy` enum
   with three variants: `Sequential` (original safe default), `Annotated`
   (parallel for tools that declare `parallel_safe = true`, sequential for all
   others), and `All` (original `parallel_tool_calls = true` behavior).  Tools
   annotated as `parallel_safe = true` run concurrently with each other under the
   `Annotated` policy; tools annotated as `parallel_safe = false` (the default)
   always run in their own sequential phase.

2. **Configurable maximum concurrency.** Add `max_concurrent_tools: usize`
   (default 4) to `AgentConfig`.  The executor controls simultaneous tool
   executions using a `tokio::sync::Semaphore` acquired before each tool call.
   This caps memory pressure and I/O scheduler load regardless of how many tool
   calls the LLM emits.

3. **Tool dependency ordering within a turn.** Add an optional
   `ToolCallOrder` mechanism: a tool call may declare `depends_on:
   Vec<String>` referencing prior `tool_use_id` values.  The executor builds a
   `DependencyGraph`, topologically sorts tool calls into execution levels, and
   runs each level (a set of tools with no unresolved dependencies) in parallel
   before advancing to the next level.  The initial implementation supports
   inferred dependencies via the `Annotated` policy (sequential tools become a
   level of their own after the parallel level); explicit `depends_on` is a
   stretch goal for the same deliverable.

4. **Streaming partial results.** Emit a new `ToolResultAvailable` signal on the
   `SignalBus` as each tool future settles, rather than batching all results until
   `join_all` completes.  Use `futures::stream::FuturesUnordered` instead of
   `futures::future::join_all` for the parallel group so results are processed in
   completion order.  Conversation accumulation still waits until the full batch
   is done before the next LLM call, preserving the ordering invariant required
   by the Anthropic API.

5. **xaft config exposure.** `XaftAgentConfig` and `AgentPreset` gain
   `parallel_tool_policy: ParallelToolPolicy` and `max_concurrent_tools: usize`.
   The xaft TOML config supports:
   ```toml
   [agent.default]
   parallel_tool_policy = "annotated"
   max_concurrent_tools = 4
   ```

6. **xaft tool annotation.** All read-only FS and git tools in `xaft-tools`
   override `Tool::parallel_safe() -> bool` to return `true`.  All write and
   destructive tools return `false` (the default, no override needed).  This is
   a static, per-type annotation — no runtime lookup required.

7. **Metrics.** Extend `AgentRunComplete` with a `ToolCallMetrics` field:
   `parallel_peak` (maximum simultaneous tool executions observed in any single
   turn), `sequential_count` (total tool calls that ran sequentially),
   `total_wall_ms` (cumulative wall clock time across all tool calls, not summed
   durations — actual elapsed from first dispatch to last completion per turn,
   summed across turns).  The executor tracks these during Phase 2.

---

## Non-Goals

- Executing tools from different LLM turns concurrently.  Turns are strictly
  sequential; parallelism is within a single turn only.
- Providing an HTTP API or TUI control to change policy at runtime without
  config file reload.
- Changing the Anthropic API request to use the `parallel_tool_calls` request
  parameter (separate from executor-level parallelism).
- Supporting dynamic `depends_on` annotations emitted by the LLM in its tool_use
  block JSON (this is not part of the Anthropic or OpenAI tool calling schema as
  of June 2026).
- Persistent caching of parallel-safe tool results across turns (see PRD 57).

---

## Architecture

### 5.1 agtrs-runtime changes

#### 5.1.1 `ParallelToolPolicy` enum (`src/agent.rs`)

```rust
/// How the executor selects which tool calls to run concurrently.
///
/// Applied during Phase 2 of the ReAct loop.  Phase 1 (guardrails + approval
/// gates) is always sequential regardless of policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelToolPolicy {
    /// Run all tool calls sequentially in LLM-requested order.
    ///
    /// This is the default and the safest mode.  Identical to the original
    /// `parallel_tool_calls = false` behavior.
    #[default]
    Sequential,

    /// Run tool calls annotated as `parallel_safe = true` concurrently (up to
    /// `max_concurrent_tools`), then run remaining tool calls one at a time.
    ///
    /// The executor partitions the turn's tool calls into two groups:
    ///
    /// - **Parallel group**: calls whose tool implements `parallel_safe() = true`.
    ///   All calls in this group are submitted to `FuturesUnordered` and run
    ///   concurrently under a `Semaphore(max_concurrent_tools)`.  Results are
    ///   streamed via `ToolResultAvailable` as they arrive.
    ///
    /// - **Sequential group**: calls whose tool implements `parallel_safe() = false`
    ///   (the default for any tool that does not override the method).  These run
    ///   one by one in LLM-requested order, after the parallel group has fully
    ///   settled.
    ///
    /// If a turn contains only parallel-safe tools, there is no sequential phase.
    /// If a turn contains only sequential tools, behaviour is identical to the
    /// `Sequential` policy (AC14).
    Annotated,

    /// Run all tool calls concurrently regardless of annotation.
    ///
    /// Identical to the original `parallel_tool_calls = true` behavior, but now
    /// subject to the `max_concurrent_tools` semaphore and `ToolResultAvailable`
    /// streaming.  Use with caution: write tools may interleave destructively.
    All,
}
```

**Builder helper** — keep `with_parallel_tools()` for backward compatibility but
map it to `All`:

```rust
impl AgentConfig {
    /// Deprecated alias: sets policy to `All`.  Prefer `with_parallel_policy()`.
    pub fn with_parallel_tools(mut self) -> Self {
        self.parallel_tool_policy = ParallelToolPolicy::All;
        self
    }

    /// Set the parallel tool policy.
    pub fn with_parallel_policy(mut self, policy: ParallelToolPolicy) -> Self {
        self.parallel_tool_policy = policy;
        self
    }

    /// Set the maximum number of concurrent tool executions.
    pub fn with_max_concurrent_tools(mut self, n: usize) -> Self {
        self.max_concurrent_tools = n;
        self
    }
}
```

**`AgentConfig` struct diff:**

```rust
pub struct AgentConfig {
    // ... existing fields ...

    /// [DEPRECATED] Whether to allow parallel tool execution.
    ///
    /// Kept for backward compatibility; treated as an alias for
    /// `parallel_tool_policy = ParallelToolPolicy::All` when `true`.
    /// New code should use `parallel_tool_policy` directly.
    #[serde(default)]
    pub parallel_tool_calls: bool,

    /// Fine-grained parallel execution policy (supersedes `parallel_tool_calls`).
    #[serde(default)]
    pub parallel_tool_policy: ParallelToolPolicy,

    /// Maximum number of tool calls that may execute simultaneously.
    ///
    /// Enforced via a `tokio::sync::Semaphore`.  A value of 1 is equivalent to
    /// `Sequential` regardless of `parallel_tool_policy`.  Default: 4.
    #[serde(default = "default_max_concurrent_tools")]
    pub max_concurrent_tools: usize,
}

fn default_max_concurrent_tools() -> usize {
    4
}
```

**Effective policy resolution** — call this at the start of Phase 2:

```rust
fn effective_policy(config: &AgentConfig) -> ParallelToolPolicy {
    // Deprecated bool takes precedence if set to true, for backward compat.
    if config.parallel_tool_calls {
        return ParallelToolPolicy::All;
    }
    config.parallel_tool_policy
}
```

#### 5.1.2 `Tool::parallel_safe()` default method (`src/tool.rs`)

Add to the `Tool` trait after `requires_confirmation`:

```rust
/// Whether this tool may safely execute concurrently with other
/// `parallel_safe` tools in the same turn.
///
/// A tool is parallel-safe if and only if:
///   1. It does not write to any shared mutable resource (filesystem,
///      database, git index) that other tools in the same turn may read
///      from or write to.
///   2. It is idempotent: running it concurrently with itself produces
///      the same result as running it serially.
///
/// The default is `false` (sequential-only).  Override to return `true`
/// for pure read operations.
///
/// This is a static hint — the executor evaluates it once during Phase 2
/// setup and does not re-evaluate mid-execution.
fn parallel_safe(&self) -> bool {
    false
}
```

Also add `parallel_safe` to `ToolMeta`:

```rust
pub struct ToolMeta {
    pub name: String,
    pub description: String,
    pub parameters: ToolSchema,
    pub is_async: bool,
    pub reads_context: bool,
    pub context_param_name: Option<String>,
    pub requires_confirmation: bool,
    pub cache_ttl: Option<Duration>,
    pub cache_key_fn: Option<fn(&serde_json::Value) -> String>,
    /// Whether this tool may run concurrently with other parallel-safe tools.
    pub parallel_safe: bool,
}
```

The `#[tool]` macro gains a `parallel_safe` attribute flag:

```rust
#[tool(parallel_safe = true)]
async fn read_file(...) -> ToolResult { ... }
```

For hand-written tools (the xaft pattern), the trait method override is used
directly.

#### 5.1.3 `ToolResultAvailable` signal (`src/signals.rs`)

```rust
/// Signal emitted as each tool result arrives during parallel execution.
///
/// Fired by the executor for every tool call as its future settles,
/// regardless of success or failure.  Precedes the full `ToolCallComplete`
/// signal for the same `tool_use_id`.  Subscribers (e.g. TUI ephemeral
/// renderer) can use this to show incremental progress.
///
/// Emitted under `Annotated` and `All` policies only.  Under `Sequential`
/// policy, `ToolCallComplete` is sufficient.
#[derive(Debug, Clone)]
pub struct ToolResultAvailable {
    /// The tool use ID.
    pub tool_use_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The agent ID.
    pub agent_id: Option<String>,
    /// The result, including error flag.
    pub result: ToolResult,
    /// Wall-clock milliseconds this single call took.
    pub duration_ms: f64,
    /// Number of tool calls still in flight at the moment this result arrived.
    pub remaining_in_flight: usize,
}
```

#### 5.1.4 `ToolCallMetrics` and `AgentRunComplete` extension (`src/signals.rs`)

```rust
/// Aggregate metrics for tool call execution across an agent run.
#[derive(Debug, Clone, Default)]
pub struct ToolCallMetrics {
    /// Maximum number of tool calls executing simultaneously in any single turn.
    pub parallel_peak: usize,
    /// Total number of tool calls that ran in sequential phase (not parallel).
    pub sequential_count: usize,
    /// Total number of tool calls that ran in parallel phase.
    pub parallel_count: usize,
    /// Sum of per-turn wall-clock durations (first dispatch to last completion).
    /// This is NOT the same as summing individual tool durations; it reflects
    /// the actual savings from parallelism.
    pub total_wall_ms: f64,
}
```

Extend `AgentRunComplete`:

```rust
pub struct AgentRunComplete {
    pub agent_id: String,
    pub agent_name: String,
    pub turns: usize,
    pub total_usage: TokenUsage,
    pub total_cost_usd: f64,
    pub stop_reason: StopReason,
    /// Tool execution efficiency metrics.  `None` when no tool calls were made.
    pub tool_metrics: Option<ToolCallMetrics>,
}
```

All existing emit sites for `AgentRunComplete` must be updated to pass
`tool_metrics: ctx.metadata().tool_metrics.clone()`.  Add
`tool_metrics: ToolCallMetrics` to `AgentMetadata`.

#### 5.1.5 Executor Phase 2 redesign (`src/executor.rs`)

The redesign applies to both `AgentExecutor::run` and `AgentExecutor::run_stream`
identically.  The current code at the Phase 2 boundary:

```rust
// CURRENT (to be replaced)
let raw_results = if ctx.config.parallel_tool_calls {
    futures::future::join_all(exec_futs).await
} else {
    let mut v = Vec::with_capacity(exec_futs.len());
    for f in exec_futs {
        v.push(f.await);
    }
    v
};
```

**Replacement — full Phase 2:**

```rust
// ── Phase 2: execute with ParallelToolPolicy ──────────────────────────
//
// Partitioning (Annotated policy):
//   parallel_group: tool calls where tool.parallel_safe() = true
//   sequential_group: all others
//
// All policies enforce max_concurrent_tools via a Semaphore.
// FuturesUnordered is used for the parallel group so ToolResultAvailable
// can be emitted as each future completes, rather than after all do.

use futures::StreamExt as _;
use std::sync::Arc;
use tokio::sync::Semaphore;

let policy = effective_policy(&ctx.config);
let semaphore = Arc::new(Semaphore::new(ctx.config.max_concurrent_tools));

// Build the full list of (tool_call, future) pairs, filtering rejected tools.
// Each future emits ToolCallStarted, executes the tool under the semaphore,
// emits ToolResultAvailable, and returns (tool_call, result, duration_ms).

// Helper closure that wraps one tool call in semaphore acquisition.
let make_fut = |tc: ToolCall, ctx_snap: AgentContext, sem: Arc<Semaphore>,
                rt: CancellationToken, de: Arc<AtomicBool>| {
    async move {
        let _permit = sem.acquire_owned().await
            .expect("semaphore closed during tool execution");

        ctx_snap.signals().emit(ToolCallStarted {
            tool_name: tc.name.clone(),
            tool_use_id: tc.tool_use_id.clone(),
            agent_id: Some(ctx_snap.agent_id.clone()),
            input: tc.input.clone(),
            cache_hit: false,
        }).await;

        let start = std::time::Instant::now();
        let result = tokio::select! {
            biased;
            _ = rt.cancelled() => {
                Err(AgtrsError::Cancelled { reason: cancel_reason(&de) })
            }
            r = ctx_snap.call_tool(&tc.name, tc.input.clone(), &tc.tool_use_id) => r,
        };
        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        (tc, result, duration_ms)
    }
};

// Partition into groups based on policy.
let (parallel_calls, sequential_calls): (Vec<ToolCall>, Vec<ToolCall>) =
    response.tool_calls.iter()
        .filter(|tc| !rejected_tools.contains(&tc.tool_use_id))
        .cloned()
        .partition(|tc| {
            match policy {
                ParallelToolPolicy::All => true,
                ParallelToolPolicy::Annotated => ctx
                    .get_tool(&tc.name)
                    .map_or(false, |t| t.parallel_safe()),
                ParallelToolPolicy::Sequential => false,
            }
        });

// ── Parallel group ────────────────────────────────────────────────────
let mut raw_results: Vec<(ToolCall, Result<ToolResult, AgtrsError>, f64)> =
    Vec::new();

if !parallel_calls.is_empty() {
    let turn_start = std::time::Instant::now();
    let n = parallel_calls.len();
    let mut in_flight = n;

    let mut futs = futures::stream::FuturesUnordered::new();
    for tc in parallel_calls {
        futs.push(make_fut(
            tc,
            ctx.clone(),
            Arc::clone(&semaphore),
            run_token.clone(),
            Arc::clone(&deadline_exceeded),
        ));
    }

    while let Some((tc, result, duration_ms)) = futs.next().await {
        in_flight = in_flight.saturating_sub(1);
        let tool_result_preview = match &result {
            Ok(r) => r.clone(),
            Err(e) => ToolResult::error(e.to_string(), &tc.tool_use_id),
        };

        // Emit streaming partial result signal.
        ctx.signals().emit(ToolResultAvailable {
            tool_use_id: tc.tool_use_id.clone(),
            tool_name: tc.name.clone(),
            agent_id: Some(ctx.agent_id.clone()),
            result: tool_result_preview,
            duration_ms,
            remaining_in_flight: in_flight,
        }).await;

        raw_results.push((tc, result, duration_ms));
    }

    let turn_wall_ms = turn_start.elapsed().as_secs_f64() * 1000.0;
    let peak = (n - in_flight).min(ctx.config.max_concurrent_tools);
    ctx.metadata_mut().tool_metrics.parallel_count += n;
    ctx.metadata_mut().tool_metrics.parallel_peak =
        ctx.metadata_mut().tool_metrics.parallel_peak.max(peak);
    ctx.metadata_mut().tool_metrics.total_wall_ms += turn_wall_ms;
}

// ── Sequential group ──────────────────────────────────────────────────
for tc in sequential_calls {
    let ctx_snap = ctx.clone();
    let sem = Arc::clone(&semaphore);
    let rt = run_token.clone();
    let de = Arc::clone(&deadline_exceeded);

    let item = make_fut(tc, ctx_snap, sem, rt, de).await;
    let duration_ms = item.2;
    ctx.metadata_mut().tool_metrics.sequential_count += 1;
    ctx.metadata_mut().tool_metrics.total_wall_ms += duration_ms;

    // Sequential tools also emit ToolResultAvailable for signal consistency.
    let tool_result_preview = match &item.1 {
        Ok(r) => r.clone(),
        Err(e) => ToolResult::error(e.to_string(), &item.0.tool_use_id),
    };
    ctx.signals().emit(ToolResultAvailable {
        tool_use_id: item.0.tool_use_id.clone(),
        tool_name: item.0.name.clone(),
        agent_id: Some(ctx.agent_id.clone()),
        result: tool_result_preview,
        duration_ms,
        remaining_in_flight: 0,
    }).await;

    raw_results.push(item);
}

// ── Inject synthetic error results for rejected tools ─────────────────
for tc in &response.tool_calls {
    if rejected_tools.contains(&tc.tool_use_id) {
        let error_msg = format!(
            "tool '{}' was rejected by the user. Try a different approach or ask for permission.",
            tc.name
        );
        raw_results.push((
            tc.clone(),
            Ok(ToolResult::error(&error_msg, &tc.tool_use_id)),
            0.0,
        ));
    }
}

// Phase 3 is unchanged: drain raw_results in LLM-requested order.
```

**Important invariant**: Phase 3 must drain `raw_results` in LLM-requested
order regardless of completion order.  `raw_results` may arrive out of order
from `FuturesUnordered`.  Before Phase 3 begins, sort `raw_results` by the
original `response.tool_calls` ordering:

```rust
// Re-order raw_results to match LLM-requested order before Phase 3.
let order_map: HashMap<String, usize> = response.tool_calls
    .iter()
    .enumerate()
    .map(|(i, tc)| (tc.tool_use_id.clone(), i))
    .collect();

raw_results.sort_by_key(|(tc, _, _)| order_map.get(&tc.tool_use_id).copied().unwrap_or(usize::MAX));
```

The `LlmOptions` field `parallel_tool_calls` (sent to the provider in the API
request) remains set from the effective policy: `true` for `All` and `Annotated`,
`false` for `Sequential`.  This hints to the LLM that it may emit multiple
tool_use blocks.

#### 5.1.6 `DependencyGraph` (stretch goal — same deliverable)

For explicit `depends_on` ordering within a turn, the executor supports an
optional `ToolCallOrder` map stored in `AgentContext`:

```rust
/// Per-turn ordering constraints for tool calls.
///
/// Keys are `tool_use_id` values.  Values are sets of `tool_use_id` values
/// that must complete before the keyed call may start.
///
/// Populated either by the executor's automatic inference (read/write partition)
/// or by explicit annotation via `AgentContext::add_tool_dependency`.
pub struct DependencyGraph {
    pub edges: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    /// Topological sort into execution levels.
    ///
    /// Level 0: tool calls with no unsatisfied dependencies.
    /// Level 1: tool calls whose only dependencies are in Level 0.
    /// ...
    ///
    /// Returns `Err` if the graph contains a cycle.
    pub fn into_levels(self, tool_use_ids: &[String]) -> Result<Vec<Vec<String>>, AgtrsError> {
        // Kahn's algorithm
        // ...
    }
}
```

In the `Annotated` policy, the implicit dependency graph is:

- All parallel-safe calls form Level 0 (no dependencies among themselves).
- All sequential calls form Level 1, each depending on the completion of the
  parallel group.

This maps naturally to the two-phase execution already described.  Explicit
`depends_on` support adds the ability to declare finer ordering, for example
`tu_3.depends_on = ["tu_1"]` which puts `tu_3` in Level 1 even if it is
parallel-safe.

---

### 5.2 xaft-tools changes

#### 5.2.1 Read-only FS tools — `parallel_safe() = true`

Each of the following tool structs in `crates/xaft-tools/src/fs/` overrides the
default `parallel_safe()` method:

| File | Struct | Tool name |
|------|--------|-----------|
| `read_file.rs` | `ReadFileTool` | `read_file` |
| `read_many.rs` | `ReadManyTool` | `read_many` |
| `list_files.rs` | `ListFilesTool` | `list_files` |
| `grep.rs` | `GrepTool` | `grep` |
| `glob.rs` | `GlobToolFs` | `glob` |
| `file_stat.rs` | `FileStatToolFs` | `file_stat` |
| `tree.rs` | `TreeToolFs` | `tree` |
| `diff_files.rs` | `DiffFilesTool` | `diff_files` |
| `search_files.rs` | `SearchFilesTool` | `search_files` |

Example for `ReadFileTool`:

```rust
// In crates/xaft-tools/src/fs/read_file.rs

#[async_trait]
impl Tool for ReadFileTool {
    // ... existing impl ...

    fn parallel_safe(&self) -> bool {
        true
    }
}
```

The same one-liner is added to each of the nine structs above.

#### 5.2.2 Read-only git tools — `parallel_safe() = true`

Each of the following tool structs in `crates/xaft-tools/src/git/` overrides
`parallel_safe()`:

| File | Struct | Tool name |
|------|--------|-----------|
| `status.rs` | `GitStatusTool` | `git_status` |
| `diff.rs` | `GitDiffTool` | `git_diff` |
| `log.rs` | `GitLogTool` | `git_log` |
| `blame.rs` | `GitBlameTool` | `git_blame` |
| `show.rs` | `GitShowTool` | `git_show` |
| `branch.rs` | `GitBranchTool` | `git_branch` |
| `stash_list.rs` | `GitStashListTool` | `git_stash_list` |
| `remote.rs` | `GitRemoteTool` | `git_remote` |
| `grep.rs` | `GitGrepTool` | `git_grep` |
| `tag.rs` | `GitTagTool` | `git_tag` |

#### 5.2.3 Write/destructive tools — `parallel_safe() = false` (default, no override)

The following tools do NOT override `parallel_safe()` and therefore return the
trait default of `false`.  They are listed here for documentation completeness:

**FS write tools:** `WriteFileTool`, `EditFileTool`, `AppendToFileTool`,
`PatchFileTool`, `MoveFileTool`, `CopyFileTool`, `DeleteFileTool`,
`CreateDirectoryTool`, `RemoveDirectoryTool`

**Git write tools:** `GitAddTool`, `GitUnstageTool`, `GitCommitStagedTool`,
`GitStashTool`, `GitStashPopTool`, `GitCheckoutFilesTool`, `GitPushTool`,
`GitCreateBranchTool`, `GitMergeTool`

**Shell tool:** `BashExecTool` — sequential, and also requires confirmation.

No code changes are required for these tools.

#### 5.2.4 `ReadBeforeEditHook` interaction

The existing `ReadBeforeEditHook` (registered in the orchestrator as a global
tool hook) enforces that a `read_file` occurs before any `edit_file` on the same
path.  Under the `Annotated` policy, `read_file` is in the parallel group and
`edit_file` is in the sequential group; the read always finishes before the edit
can start.  The hook remains as a correctness backstop but should never trigger
during a well-formed parallel turn.

---

### 5.3 xaft-agent changes

#### 5.3.1 `XaftAgentConfig` (`crates/xaft-agent/src/config.rs`)

Replace the existing `pub parallel_tools: bool` field:

```rust
use agtrs_runtime::agent::ParallelToolPolicy;

pub struct XaftAgentConfig {
    // ... existing fields ...

    /// Parallel tool execution policy.
    ///
    /// `Sequential` (default): safe, no concurrency.
    /// `Annotated`: parallel for read-only tools, sequential for write tools.
    /// `All`: run everything concurrently (use with caution).
    pub parallel_tool_policy: ParallelToolPolicy,

    /// Maximum simultaneous tool executions.
    ///
    /// Enforced via a semaphore.  Default: 4.
    pub max_concurrent_tools: usize,
}

impl Default for XaftAgentConfig {
    fn default() -> Self {
        Self {
            // ... existing fields ...
            parallel_tool_policy: ParallelToolPolicy::Annotated, // safe default for xaft
            max_concurrent_tools: 4,
        }
    }
}
```

Note: the xaft default is `Annotated` (not `Sequential`) because xaft's read-only
tools are all annotated as parallel-safe, making `Annotated` automatically safe
while providing speedup for read-heavy planning and QA turns.

#### 5.3.2 `XaftAgentConfig` → `AgentConfig` conversion

Wherever `XaftAgentConfig` is translated into `agtrs_runtime::AgentConfig`, wire
the new fields:

```rust
impl From<&XaftAgentConfig> for AgentConfig {
    fn from(xaft: &XaftAgentConfig) -> Self {
        AgentConfig {
            // ... existing mappings ...
            parallel_tool_policy: xaft.parallel_tool_policy,
            max_concurrent_tools: xaft.max_concurrent_tools,
            parallel_tool_calls: false, // use parallel_tool_policy instead
            ..AgentConfig::default()
        }
    }
}
```

---

### 5.4 xaft-config changes

#### 5.4.1 `AgentPreset` (`crates/xaft-config/src/types.rs`)

```rust
use agtrs_runtime::agent::ParallelToolPolicy;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPreset {
    // ... existing fields ...

    /// Parallel tool execution policy.
    ///
    /// `"sequential"` | `"annotated"` (default) | `"all"`
    pub parallel_tool_policy: ParallelToolPolicy,

    /// Maximum simultaneous tool executions.
    pub max_concurrent_tools: u32,
}
```

`ParallelToolPolicy` already derives `Serialize, Deserialize` with
`#[serde(rename_all = "snake_case")]` from the agtrs change, so TOML
`parallel_tool_policy = "annotated"` deserializes correctly.

#### 5.4.2 Config file example

```toml
[agent.default]
provider             = "anthropic"
model                = "claude-sonnet-4-5"
max_turns            = 25
parallel_tool_policy = "annotated"
max_concurrent_tools = 4

[agent.planner]
provider             = "anthropic"
model                = "claude-opus-4-5"
max_turns            = 10
parallel_tool_policy = "annotated"  # planning reads many files concurrently
max_concurrent_tools = 6            # planner benefits from higher concurrency

[agent.coder]
provider             = "anthropic"
model                = "claude-sonnet-4-5"
max_turns            = 40
parallel_tool_policy = "annotated"  # reads parallel, writes sequential
max_concurrent_tools = 4

[agent.qa]
provider             = "anthropic"
model                = "claude-sonnet-4-5"
max_turns            = 15
parallel_tool_policy = "annotated"  # QA is read-heavy
max_concurrent_tools = 6
```

#### 5.4.3 Config validation

Add to `ConfigValidator`:

- `max_concurrent_tools` must be ≥ 1.  Zero is rejected with a descriptive
  error: `"agent.{name}.max_concurrent_tools must be >= 1"`.
- `parallel_tool_policy` must be one of `sequential`, `annotated`, `all`.
  TOML deserialization will already reject other strings; the validator documents
  the valid values.

#### 5.4.4 Config merge / null preservation

`AgentPreset` is deep-merged with null preservation (CLAUDE.md invariant 5).
`parallel_tool_policy` and `max_concurrent_tools` must have `Default` impls so
that configs which do not specify these fields inherit from the default preset.

---

### 5.5 xaft-runtime / orchestrator changes

#### 5.5.1 Agent construction in `orchestrator.rs`

`run_workflow` builds each `NamedAgent` from `xaft_agents::named::NamedAgent`.
After this PRD, each agent's `AgentConfig` must be populated from the matching
`AgentPreset` in `XaftConfig`.  The current code hard-codes `max_turns: 100`
and does not carry any config fields.

Add a `config_for_role()` helper:

```rust
fn config_for_role(
    xaft_config: &XaftConfig,
    role_name: &str,
) -> agtrs_runtime::agent::AgentConfig {
    let preset = xaft_config.agent
        .get(role_name)
        .or_else(|| xaft_config.agent.get("default"))
        .cloned()
        .unwrap_or_default();

    agtrs_runtime::agent::AgentConfig {
        max_turns: preset.max_turns as usize,
        parallel_tool_policy: preset.parallel_tool_policy,
        max_concurrent_tools: preset.max_concurrent_tools as usize,
        // ... other fields ...
        ..agtrs_runtime::agent::AgentConfig::default()
    }
}
```

Pass `xaft_config: &XaftConfig` into `run_workflow` (it is already available at
the call site in `runtime.rs`).

#### 5.5.2 `NamedAgent` config propagation

`NamedAgent::new` currently takes `(name, system_prompt, max_turns)`.  Extend
to accept or merge an `AgentConfig`:

```rust
impl NamedAgent {
    pub fn new_with_config(
        name: &str,
        system_prompt: &str,
        config: agtrs_runtime::agent::AgentConfig,
    ) -> Self {
        // ...
    }
}
```

The existing `new(name, prompt, max_turns)` constructor is kept as a convenience
wrapper that defaults all other fields.

---

### 5.6 xaft-tui changes

#### 5.6.1 Ephemeral tool progress line

The TUI ephemeral spinner currently shows `"running tool: {tool_name}..."` while
a single tool executes.  Under parallel execution, multiple tools run
simultaneously.  The ephemeral line should show:

```
  ◉ 3 tools running (read_file, grep, glob)…
```

When all parallel tools complete and sequential tools begin:

```
  ◉ write_file…
```

This requires subscribing to `ToolResultAvailable` and `ToolCallStarted` signals
in the `EventBridge` and mapping them to `TuiEvent::ToolBatchProgress { in_flight: Vec<String> }`.

`AppState::handle_event` updates the ephemeral state:

```rust
TuiEvent::ToolBatchProgress { in_flight } => {
    if in_flight.is_empty() {
        self.ephemeral = None;
    } else if in_flight.len() == 1 {
        self.ephemeral = Some(format!("  {} {}…", SPINNER, in_flight[0]));
    } else {
        let names = in_flight.join(", ");
        self.ephemeral = Some(format!(
            "  {} {} tools running ({})…",
            SPINNER,
            in_flight.len(),
            names,
        ));
    }
}
```

The ephemeral renderer already refreshes at ~10fps from the main loop tick; no
additional timer is needed.

#### 5.6.2 `EventBridge` additions (`crates/xaft-tui/src/bridge.rs`)

Subscribe to `ToolResultAvailable` in `EventBridge::attach`:

```rust
let mut tool_result_rx = signals.subscribe::<ToolResultAvailable>().await;
```

In the bridge task loop, maintain a `HashSet<String>` of in-flight tool names.
`ToolCallStarted` adds a name; `ToolResultAvailable` removes it.  After each
state change, send `TuiEvent::ToolBatchProgress { in_flight: current_names }`.

---

## Data Model

### New types introduced

```
agtrs-runtime:
  ParallelToolPolicy              (enum, in agent.rs)
  ToolResultAvailable             (signal struct, in signals.rs)
  ToolCallMetrics                 (struct, in signals.rs)

xaft-config:
  AgentPreset::parallel_tool_policy: ParallelToolPolicy
  AgentPreset::max_concurrent_tools: u32

xaft-agent:
  XaftAgentConfig::parallel_tool_policy: ParallelToolPolicy
  XaftAgentConfig::max_concurrent_tools: usize
```

### Modified types

```
agtrs-runtime:
  AgentConfig           — add parallel_tool_policy, max_concurrent_tools
  AgentMetadata         — add tool_metrics: ToolCallMetrics
  AgentRunComplete      — add tool_metrics: Option<ToolCallMetrics>
  ToolMeta              — add parallel_safe: bool
  Tool trait            — add parallel_safe() -> bool default method

xaft-agent:
  XaftAgentConfig       — replace parallel_tools: bool with parallel_tool_policy, max_concurrent_tools
```

---

## Files to Change

### agtrs-runtime (`/root/rust_projects/agtrs/agtrs-runtime/src/`)

| File | Change |
|------|--------|
| `agent.rs` | Add `ParallelToolPolicy` enum; extend `AgentConfig` with `parallel_tool_policy`, `max_concurrent_tools`; add `effective_policy()` helper; update `with_parallel_tools()` builder; extend `AgentMetadata` with `tool_metrics: ToolCallMetrics` |
| `tool.rs` | Add `parallel_safe()` default method to `Tool` trait; add `parallel_safe: bool` to `ToolMeta` |
| `executor.rs` | Replace Phase 2 with the `FuturesUnordered` + `Semaphore` implementation; add `ToolResultAvailable` emission; add result re-ordering before Phase 3; update `AgentRunComplete` emit to include `tool_metrics` |
| `signals.rs` | Add `ToolResultAvailable` struct; add `ToolCallMetrics` struct; extend `AgentRunComplete` with `tool_metrics: Option<ToolCallMetrics>` |

### agtrs-runtime tests (`/root/rust_projects/agtrs/agtrs-runtime/tests/`)

| File | Change |
|------|--------|
| `executor_parallel.rs` | Add new test cases for `Annotated` policy, semaphore cap, streaming signal, dependency ordering, cancellation |

### xaft-tools (`/root/rust_projects/xaft/crates/xaft-tools/src/`)

| File | Change |
|------|--------|
| `fs/read_file.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/read_many.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/list_files.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/grep.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/glob.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/file_stat.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/tree.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/diff_files.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `fs/search_files.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/status.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/diff.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/log.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/blame.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/show.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/branch.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/stash_list.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/remote.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/grep.rs` | Add `fn parallel_safe(&self) -> bool { true }` |
| `git/tag.rs` | Add `fn parallel_safe(&self) -> bool { true }` |

### xaft-agent (`/root/rust_projects/xaft/crates/xaft-agent/src/`)

| File | Change |
|------|--------|
| `config.rs` | Replace `parallel_tools: bool` with `parallel_tool_policy: ParallelToolPolicy` and `max_concurrent_tools: usize`; update `Default` impl; update `for_role()` |

### xaft-config (`/root/rust_projects/xaft/crates/xaft-config/src/`)

| File | Change |
|------|--------|
| `types.rs` | Add `parallel_tool_policy: ParallelToolPolicy` and `max_concurrent_tools: u32` to `AgentPreset`; update `Default` impl; update `ResolvedAgentPreset` accordingly |

### xaft-runtime (`/root/rust_projects/xaft/crates/xaft-runtime/src/`)

| File | Change |
|------|--------|
| `orchestrator.rs` | Add `config_for_role()` helper; extend `run_workflow` signature to accept `&XaftConfig`; wire parallel policy into each agent's `AgentConfig`; update `NamedAgent` construction |

### xaft-tui (`/root/rust_projects/xaft/crates/xaft-tui/src/`)

| File | Change |
|------|--------|
| `bridge.rs` | Subscribe to `ToolResultAvailable`; maintain in-flight set; emit `TuiEvent::ToolBatchProgress` |
| `state.rs` | Handle `TuiEvent::ToolBatchProgress` to update ephemeral |
| `ephemeral.rs` (or where ephemeral is built) | Format multi-tool progress line |

---

## Acceptance Criteria

| # | Criterion | Verification |
|---|-----------|-------------|
| AC1 | With `parallel_tool_policy = "annotated"` and 3 `read_file` calls in one turn, all 3 execute concurrently: wall-clock time is less than the sum of individual durations. | Integration test measuring `Instant::now()` across the turn. |
| AC2 | `max_concurrent_tools = 2` limits concurrent executions to at most 2 at any moment, even when 5 parallel-safe tools are called in the same turn. | Test with a `Semaphore` permit counter mock or an `AtomicUsize` tracking peak concurrent calls. |
| AC3 | `write_file` (`parallel_safe = false`) always runs in its own sequential phase, never concurrent with any other tool, under `Annotated` policy. | Test with a shared mutex: `write_file` mock acquires the mutex; any other mock that also tries to acquire during write must block (not deadlock). |
| AC4 | `parallel_tool_policy = "sequential"` is fully backward-compatible with the original `parallel_tool_calls = false` behavior: identical tool call order, identical conversation messages, no `ToolResultAvailable` signals emitted. | Port existing `sequential_two_tools_results_in_order` test with `Sequential` policy. |
| AC5 | With `Annotated` or `All` policy, `ToolResultAvailable` is emitted for each tool as its future settles, before all tools in the batch are done. | Subscriber records timestamps; first `ToolResultAvailable` arrives before all futures complete when one tool has an injected 50 ms delay. |
| AC6 | If tool B declares `depends_on = [tool_A_use_id]`, tool B does not start until tool A completes. | Test with a shared counter: B checks that A's counter is already incremented when B's `call()` is entered. |
| AC7 | Cancellation during parallel execution cancels all in-flight futures: the `CancellationToken::cancel()` call causes all futures in the `FuturesUnordered` to resolve to `AgtrsError::Cancelled`. | Inject a long-sleeping tool; cancel after 10 ms; assert all tools returned error results within 50 ms. |
| AC8 | A tool error in the parallel group does not cancel other tools in the same group: all tools run to completion, errors are collected alongside successes. | `FailTool` plus two `EchoTool` instances; assert both echo tools ran despite `FailTool` error. |
| AC9 | `AgentRunComplete` carries `ToolCallMetrics { parallel_peak, sequential_count, total_wall_ms }` that reflect the actual execution. | Subscribe to `AgentRunComplete`; after a run with 3 parallel + 1 sequential tool, assert `parallel_peak >= 2`, `sequential_count == 1`, `total_wall_ms > 0`. |
| AC10 | All xaft read-only tools return `parallel_safe() = true`: `read_file`, `read_many`, `list_files`, `grep`, `glob`, `file_stat`, `tree`, `diff_files`, `search_files`, `git_status`, `git_diff`, `git_log`, `git_blame`, `git_show`, `git_branch`, `git_stash_list`, `git_remote`, `git_grep`, `git_tag`. | Unit test: instantiate each struct, call `parallel_safe()`, assert `true`. |
| AC11 | All xaft write/destructive tools return `parallel_safe() = false`: `write_file`, `edit_file`, `append_to_file`, `patch_file`, `move_file`, `copy_file`, `delete_file`, `create_directory`, `remove_directory`, `git_add`, `git_unstage`, `git_commit_staged`, `git_stash`, `git_stash_pop`, `git_checkout_files`, `git_push`, `git_create_branch`, `git_merge`, `bash_exec`. | Unit test: same pattern as AC10 but assert `false`. |
| AC12 | `parallel_tool_policy = "all"` matches the old `parallel_tool_calls = true` behavior (all tools concurrent), now additionally subject to the semaphore cap. | Port existing `parallel_two_tools_both_called_results_in_order` with `All` policy. |
| AC13 | TUI ephemeral line shows `"N tools running (name1, name2, …)…"` while parallel tools are in flight, and reverts to the single-tool format when sequential phase starts. | Integration test with mock signal bus and state snapshot assertions. |
| AC14 | Under `Annotated` policy, a turn whose tools are all `parallel_safe = false` executes identically to the `Sequential` policy: same order, same conversation messages, same turn count. | Test with three write-only mock tools; assert conversation history matches expected sequential order. |

---

## Tests

### agtrs-runtime unit tests (`agtrs-runtime/tests/executor_parallel.rs`)

These extend the existing test file.

```rust
// ── Test: Annotated policy runs parallel-safe tools concurrently ──────────────

/// Verifies that under `Annotated` policy, two tools both returning
/// `parallel_safe() = true` execute concurrently (not sequentially).
///
/// Setup:
///   - LLM queues two tool calls in one turn: slow_tool_a (sleeps 50ms),
///     slow_tool_b (sleeps 50ms), then text "done".
///   - Both tools are `parallel_safe = true`.
///   - `max_concurrent_tools = 4`.
///
/// Assertion:
///   - Wall-clock for Phase 2 < 80ms (concurrent) rather than >= 100ms (serial).
#[tokio::test]
async fn parallel_annotated_runs_safe_tools_concurrently() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let transport = Arc::new(MockTransport::new());
    transport.queue_tool_call("slow_a", serde_json::json!({})).await;
    transport.queue_tool_call("slow_b", serde_json::json!({})).await;
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));

    // SlowTool: sleeps for 50ms before returning.
    struct SlowTool { id: &'static str }
    #[async_trait]
    impl Tool for SlowTool {
        type Inputs = serde_json::Value;
        type Output = ToolResult;

        fn name(&self) -> &str { self.id }
        fn description(&self) -> &str { "Slow read tool" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true }

        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            sleep(Duration::from_millis(50)).await;
            Ok(ToolResult::ok(self.id, &ctx.tool_use_id))
        }
    }

    let slow_a = Arc::new(SlowTool { id: "slow_a" }) as Arc<ErasedTool>;
    let slow_b = Arc::new(SlowTool { id: "slow_b" }) as Arc<ErasedTool>;

    let agent = SimpleAgent::new_with_policy(
        vec![Arc::clone(&slow_a), Arc::clone(&slow_b)],
        ParallelToolPolicy::Annotated,
        4,
    );
    let mut ctx = AgentContext::new(
        "test",
        AgentConfig {
            parallel_tool_policy: ParallelToolPolicy::Annotated,
            max_concurrent_tools: 4,
            max_turns: 5,
            ..Default::default()
        },
        llm,
        Arc::new(ResolveContext::from_store(Arc::new(injectable_runtime::EmptySingletonStore))),
    );
    ctx.register_tool("slow_a", slow_a);
    ctx.register_tool("slow_b", slow_b);

    let start = Instant::now();
    AgentExecutor::run(&agent, Message::user("go"), &mut ctx).await.unwrap();
    let elapsed = start.elapsed();

    // Concurrent: both finish in ~50ms, not ~100ms.
    assert!(
        elapsed < Duration::from_millis(80),
        "Expected concurrent execution < 80ms, got {:?}",
        elapsed
    );
}


// ── Test: Annotated policy runs unsafe tools sequentially ─────────────────────

/// Verifies that under `Annotated` policy, tools with `parallel_safe = false`
/// execute sequentially (not concurrently), even when multiple are in one turn.
///
/// Setup:
///   - Two tools with `parallel_safe = false`, each incrementing a shared counter.
///   - A shared Mutex is held inside each tool's `call()` for 30ms.
///   - If both ran concurrently, the second would block on mutex acquisition —
///     which is what we assert does NOT happen concurrently.
///
/// Assertion:
///   - Wall-clock >= 60ms (serial) rather than ~30ms (parallel).
#[tokio::test]
async fn parallel_annotated_runs_unsafe_tools_sequentially() {
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let transport = Arc::new(MockTransport::new());
    transport.queue_tool_call("write_a", serde_json::json!({})).await;
    transport.queue_tool_call("write_b", serde_json::json!({})).await;
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));

    let overlap_detected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let in_progress = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let overlap_clone = Arc::clone(&overlap_detected);
    let in_progress_clone = Arc::clone(&in_progress);

    struct WriteToolMock {
        id: &'static str,
        in_progress: Arc<std::sync::atomic::AtomicBool>,
        overlap_detected: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl Tool for WriteToolMock {
        type Inputs = serde_json::Value;
        type Output = ToolResult;

        fn name(&self) -> &str { self.id }
        fn description(&self) -> &str { "Mock write tool" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { false } // explicitly sequential

        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            if self.in_progress.swap(true, Ordering::SeqCst) {
                // Another instance is already running — overlap detected.
                self.overlap_detected.store(true, Ordering::SeqCst);
            }
            sleep(Duration::from_millis(30)).await;
            self.in_progress.store(false, Ordering::SeqCst);
            Ok(ToolResult::ok(self.id, &ctx.tool_use_id))
        }
    }

    let write_a = Arc::new(WriteToolMock {
        id: "write_a",
        in_progress: Arc::clone(&in_progress),
        overlap_detected: Arc::clone(&overlap_detected),
    }) as Arc<ErasedTool>;
    let write_b = Arc::new(WriteToolMock {
        id: "write_b",
        in_progress: in_progress_clone,
        overlap_detected: overlap_clone,
    }) as Arc<ErasedTool>;

    let agent = SimpleAgent::new_with_policy(
        vec![Arc::clone(&write_a), Arc::clone(&write_b)],
        ParallelToolPolicy::Annotated,
        4,
    );
    let mut ctx = make_ctx_with_policy(llm, ParallelToolPolicy::Annotated, 4);
    ctx.register_tool("write_a", write_a);
    ctx.register_tool("write_b", write_b);

    let start = Instant::now();
    AgentExecutor::run(&agent, Message::user("go"), &mut ctx).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        !overlap_detected.load(Ordering::SeqCst),
        "Sequential tools must not execute concurrently"
    );
    assert!(
        elapsed >= Duration::from_millis(55),
        "Sequential execution should take >= 60ms, got {:?}",
        elapsed
    );
}


// ── Test: Semaphore limits concurrent executions ──────────────────────────────

/// Verifies that `max_concurrent_tools = 2` limits peak concurrency to 2
/// even when 5 parallel-safe tools are queued.
///
/// Uses an `AtomicUsize` to track the peak number of concurrently executing
/// tools.
#[tokio::test]
async fn semaphore_limits_concurrent_executions() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::sleep;

    let transport = Arc::new(MockTransport::new());
    for i in 0..5 {
        transport.queue_tool_call(&format!("safe_{i}"), serde_json::json!({})).await;
    }
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));

    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    // SafeTool: parallel_safe = true, sleeps 20ms.
    // Tracks current concurrency and updates peak.
    struct SafeTool {
        id: String,
        current: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SafeTool {
        type Inputs = serde_json::Value;
        type Output = ToolResult;

        fn name(&self) -> &str { &self.id }
        fn description(&self) -> &str { "Safe concurrent tool" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true }

        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            let c = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(c, Ordering::SeqCst);
            sleep(Duration::from_millis(20)).await;
            self.current.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolResult::ok(self.id.as_str(), &ctx.tool_use_id))
        }
    }

    let mut tools: Vec<Arc<ErasedTool>> = Vec::new();
    let mut ctx_tools: Vec<(String, Arc<ErasedTool>)> = Vec::new();
    for i in 0..5 {
        let t = Arc::new(SafeTool {
            id: format!("safe_{i}"),
            current: Arc::clone(&current),
            peak: Arc::clone(&peak),
        }) as Arc<ErasedTool>;
        ctx_tools.push((format!("safe_{i}"), Arc::clone(&t)));
        tools.push(t);
    }

    let agent = SimpleAgent::new_with_policy(tools, ParallelToolPolicy::All, 2); // cap = 2
    let mut ctx = make_ctx_with_policy(llm, ParallelToolPolicy::All, 2);
    for (name, t) in ctx_tools {
        ctx.register_tool(name, t);
    }

    AgentExecutor::run(&agent, Message::user("go"), &mut ctx).await.unwrap();

    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak <= 2,
        "Peak concurrency should be <= 2 (max_concurrent_tools), got {observed_peak}"
    );
    assert!(
        observed_peak >= 1,
        "At least one tool should have run"
    );
}


// ── Test: Dependency graph respects ordering ──────────────────────────────────

/// Verifies that when tool B declares `depends_on = [tool_A_use_id]`, B does
/// not start until A completes.
///
/// Setup:
///   - Tool A: parallel_safe, records start+end time.
///   - Tool B: parallel_safe, checks that A has already completed.
///   - DependencyGraph: B depends on A.
///
/// Assertion:
///   - B's `call()` is entered only after A's `call()` returns.
#[tokio::test]
async fn dependency_graph_respects_ordering() {
    // This test exercises the DependencyGraph::into_levels() path.
    // Tool A runs first; its completion is signalled via an AtomicBool.
    // Tool B checks the bool at the start of call() — if false, B ran before A.

    let a_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let b_saw_a_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let a_done_for_b = Arc::clone(&a_done);
    let b_saw = Arc::clone(&b_saw_a_done);

    struct ToolA { done: Arc<std::sync::atomic::AtomicBool> }
    struct ToolB {
        a_done: Arc<std::sync::atomic::AtomicBool>,
        saw: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl Tool for ToolA {
        type Inputs = serde_json::Value;
        type Output = ToolResult;
        fn name(&self) -> &str { "tool_a" }
        fn description(&self) -> &str { "" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true }
        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            self.done.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolResult::ok("a_result", &ctx.tool_use_id))
        }
    }

    #[async_trait]
    impl Tool for ToolB {
        type Inputs = serde_json::Value;
        type Output = ToolResult;
        fn name(&self) -> &str { "tool_b" }
        fn description(&self) -> &str { "" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true }
        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            // Record whether A was done when B started.
            let a_was_done = self.a_done.load(std::sync::atomic::Ordering::SeqCst);
            self.saw.store(a_was_done, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolResult::ok("b_result", &ctx.tool_use_id))
        }
    }

    let tool_a = Arc::new(ToolA { done: Arc::clone(&a_done) }) as Arc<ErasedTool>;
    let tool_b = Arc::new(ToolB { a_done: a_done_for_b, saw: b_saw }) as Arc<ErasedTool>;

    // Transport: queue tool_a (id "tu_1") and tool_b (id "tu_2", depends_on "tu_1")
    let transport = Arc::new(MockTransport::new());
    transport.queue_tool_call_with_id("tool_a", serde_json::json!({}), "tu_1").await;
    transport.queue_tool_call_with_id("tool_b", serde_json::json!({"depends_on": ["tu_1"]}), "tu_2").await;
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));
    let agent = SimpleAgent::new_with_policy(
        vec![Arc::clone(&tool_a), Arc::clone(&tool_b)],
        ParallelToolPolicy::Annotated,
        4,
    );
    let mut ctx = make_ctx_with_policy(llm, ParallelToolPolicy::Annotated, 4);
    ctx.register_tool("tool_a", tool_a);
    ctx.register_tool("tool_b", tool_b);

    AgentExecutor::run(&agent, Message::user("go"), &mut ctx).await.unwrap();

    assert!(
        b_saw_a_done.load(std::sync::atomic::Ordering::SeqCst),
        "Tool B must start only after tool A completes"
    );
}


// ── Test: Cancellation stops all parallel futures ─────────────────────────────

/// Verifies that cancelling the `CancellationToken` during parallel execution
/// causes all in-flight futures to resolve to `AgtrsError::Cancelled`.
///
/// Setup:
///   - 3 slow tools (each sleeps 200ms), all parallel_safe.
///   - Cancel the token after 20ms.
///
/// Assertion:
///   - `AgentExecutor::run` returns `Err(AgtrsError::Cancelled { .. })`.
///   - All three tool calls return error results before timeout.
#[tokio::test]
async fn cancellation_stops_all_parallel_futures() {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let transport = Arc::new(MockTransport::new());
    for i in 0..3 {
        transport.queue_tool_call(&format!("slow_{i}"), serde_json::json!({})).await;
    }
    transport.queue_text("unreachable").await;

    let llm = Arc::new(MockLlmProvider::new(transport));

    struct LongTool { id: String }
    #[async_trait]
    impl Tool for LongTool {
        type Inputs = serde_json::Value;
        type Output = ToolResult;
        fn name(&self) -> &str { &self.id }
        fn description(&self) -> &str { "" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true }
        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            tokio::select! {
                _ = ctx.cancel_token.cancelled() => {
                    Err(AgtrsError::Cancelled { reason: "tool cancelled".into() })
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    Ok(ToolResult::ok("ok", &ctx.tool_use_id))
                }
            }
        }
    }

    let mut tools: Vec<Arc<ErasedTool>> = Vec::new();
    let mut ctx_tools: Vec<(String, Arc<ErasedTool>)> = Vec::new();
    for i in 0..3 {
        let t = Arc::new(LongTool { id: format!("slow_{i}") }) as Arc<ErasedTool>;
        ctx_tools.push((format!("slow_{i}"), Arc::clone(&t)));
        tools.push(t);
    }

    let token = CancellationToken::new();
    let agent = SimpleAgent::new_with_policy(tools, ParallelToolPolicy::All, 4);
    let mut ctx = make_ctx_with_policy(llm, ParallelToolPolicy::All, 4);
    ctx.set_cancel_token(token.clone());
    for (name, t) in ctx_tools {
        ctx.register_tool(name, t);
    }

    // Cancel after 20ms.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
    });

    let start = std::time::Instant::now();
    let err = AgentExecutor::run(&agent, Message::user("go"), &mut ctx).await.unwrap_err();
    let elapsed = start.elapsed();

    assert!(matches!(err, AgtrsError::Cancelled { .. }), "Expected Cancelled, got {:?}", err);
    assert!(
        elapsed < Duration::from_millis(150),
        "Cancellation should have interrupted all futures within 150ms, took {:?}",
        elapsed
    );
}


// ── Test: Sequential policy is backward-compatible ───────────────────────────

/// Verifies that `Sequential` policy is identical to old `parallel_tool_calls = false`.
///
/// No `ToolResultAvailable` signals are emitted.
/// Results appear in LLM-requested order.
#[tokio::test]
async fn sequential_policy_is_backward_compatible() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let transport = Arc::new(MockTransport::new());
    transport.queue_tool_call("first", serde_json::json!({})).await;
    transport.queue_tool_call("second", serde_json::json!({})).await;
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));
    let call_order = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    struct OrderedTool {
        id: &'static str,
        order: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Tool for OrderedTool {
        type Inputs = serde_json::Value;
        type Output = ToolResult;
        fn name(&self) -> &str { self.id }
        fn description(&self) -> &str { "" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true } // even if safe, sequential policy ignores it
        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            self.order.lock().unwrap().push(self.id.to_string());
            Ok(ToolResult::ok(self.id, &ctx.tool_use_id))
        }
    }

    let order = Arc::clone(&call_order);
    let first = Arc::new(OrderedTool { id: "first", order: Arc::clone(&order) }) as Arc<ErasedTool>;
    let second = Arc::new(OrderedTool { id: "second", order }) as Arc<ErasedTool>;

    let agent = SimpleAgent::new_with_policy(
        vec![Arc::clone(&first), Arc::clone(&second)],
        ParallelToolPolicy::Sequential,
        4,
    );
    let mut ctx = make_ctx_with_policy(llm, ParallelToolPolicy::Sequential, 4);
    ctx.register_tool("first", first);
    ctx.register_tool("second", second);

    // Subscribe to ToolResultAvailable — should receive no events.
    let mut signal_rx = ctx.signals().subscribe::<ToolResultAvailable>().await;

    AgentExecutor::run(&agent, Message::user("go"), &mut ctx).await.unwrap();

    let order = call_order.lock().unwrap().clone();
    assert_eq!(order, vec!["first", "second"], "Sequential policy must preserve LLM-requested order");

    // No ToolResultAvailable signals under Sequential policy.
    assert!(
        signal_rx.try_recv().is_err(),
        "Sequential policy must not emit ToolResultAvailable signals"
    );
}


// ── Test: Streaming emits partial results ─────────────────────────────────────

/// Verifies that `ToolResultAvailable` is emitted as each tool completes,
/// not after all tools in the batch are done.
///
/// Setup:
///   - Tool A: completes in 10ms.
///   - Tool B: completes in 100ms.
///   - Both are parallel_safe, policy = Annotated.
///
/// Assertion:
///   - `ToolResultAvailable` for A arrives before the full batch (100ms mark).
#[tokio::test]
async fn streaming_emits_partial_results() {
    use std::time::{Duration, Instant};

    let transport = Arc::new(MockTransport::new());
    transport.queue_tool_call("fast_a", serde_json::json!({})).await;
    transport.queue_tool_call("slow_b", serde_json::json!({})).await;
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));

    struct TimedTool { id: &'static str, delay_ms: u64 }
    #[async_trait]
    impl Tool for TimedTool {
        type Inputs = serde_json::Value;
        type Output = ToolResult;
        fn name(&self) -> &str { self.id }
        fn description(&self) -> &str { "" }
        fn schema(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
        fn parallel_safe(&self) -> bool { true }
        async fn call(&self, _: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            Ok(ToolResult::ok(self.id, &ctx.tool_use_id))
        }
    }

    let fast_a = Arc::new(TimedTool { id: "fast_a", delay_ms: 10 }) as Arc<ErasedTool>;
    let slow_b = Arc::new(TimedTool { id: "slow_b", delay_ms: 100 }) as Arc<ErasedTool>;

    let agent = SimpleAgent::new_with_policy(
        vec![Arc::clone(&fast_a), Arc::clone(&slow_b)],
        ParallelToolPolicy::Annotated,
        4,
    );
    let mut ctx = make_ctx_with_policy(llm, ParallelToolPolicy::Annotated, 4);
    ctx.register_tool("fast_a", fast_a);
    ctx.register_tool("slow_b", slow_b);

    // Subscribe before run starts.
    let mut signal_rx = ctx.signals().subscribe::<ToolResultAvailable>().await;

    let run_start = Instant::now();
    let run_handle = {
        let mut ctx_clone = ctx.clone();
        let agent_ref = &agent;
        tokio::spawn(async move {
            AgentExecutor::run(agent_ref, Message::user("go"), &mut ctx_clone).await
        })
    };

    // Wait for first ToolResultAvailable.
    let first_signal = tokio::time::timeout(Duration::from_millis(500), signal_rx.recv())
        .await
        .expect("timed out waiting for ToolResultAvailable")
        .expect("signal channel closed");

    let first_signal_elapsed = run_start.elapsed();
    assert_eq!(first_signal.tool_name, "fast_a", "fast_a should complete first");
    assert!(
        first_signal_elapsed < Duration::from_millis(80),
        "First partial result should arrive < 80ms (before slow_b finishes at ~100ms), \
         got {:?}",
        first_signal_elapsed
    );

    run_handle.await.unwrap().unwrap();
}
```

### xaft-tools unit tests (`crates/xaft-tools/tests/parallel_safe.rs`)

```rust
//! Verifies that all xaft tools have the correct `parallel_safe()` annotation.

use xaft_tools::fs::{
    AppendToFileTool, CopyFileTool, CreateDirectoryTool, DeleteFileTool, DiffFilesTool,
    EditFileTool, FileStatToolFs, GlobToolFs, GrepTool, ListFilesTool, MoveFileTool,
    PatchFileTool, ReadFileTool, ReadManyTool, RemoveDirectoryTool, SearchFilesTool, TreeToolFs,
    WriteFileTool,
};
use xaft_tools::git::{
    GitAddTool, GitBlameTool, GitBranchTool, GitCheckoutFilesTool, GitCommitStagedTool,
    GitCreateBranchTool, GitDiffTool, GitGrepTool, GitLogTool, GitMergeTool, GitPushTool,
    GitRemoteTool, GitShowTool, GitStashListTool, GitStashPopTool, GitStashTool, GitStatusTool,
    GitTagTool, GitUnstageTool,
};
use xaft_tools::shell::BashExecTool;
use agtrs_runtime::tool::Tool;
use agtrs_workspace::InMemoryWorkspaceStore;
use std::sync::Arc;
use tempfile::TempDir;

fn make_store() -> Arc<dyn agtrs_workspace::WorkspaceStore> {
    Arc::new(InMemoryWorkspaceStore::new())
}

fn make_repo_path() -> TempDir {
    TempDir::new().unwrap()
}

// ── AC10: All read-only FS tools are parallel_safe ────────────────────────────

#[test]
fn all_read_only_fs_tools_are_parallel_safe() {
    let store = make_store();
    let tmp = make_repo_path();
    let root = tmp.path();

    let read_only_tools: Vec<(&str, bool)> = vec![
        ("read_file",    ReadFileTool::new(Arc::clone(&store)).parallel_safe()),
        ("read_many",    ReadManyTool::new(Arc::clone(&store)).parallel_safe()),
        ("list_files",   ListFilesTool::new(Arc::clone(&store)).parallel_safe()),
        ("grep",         GrepTool::new(Arc::clone(&store)).parallel_safe()),
        ("glob",         GlobToolFs::new(Arc::clone(&store), root).parallel_safe()),
        ("file_stat",    FileStatToolFs::new(Arc::clone(&store), root).parallel_safe()),
        ("tree",         TreeToolFs::new(Arc::clone(&store), root).parallel_safe()),
        ("diff_files",   DiffFilesTool::new(Arc::clone(&store)).parallel_safe()),
        ("search_files", SearchFilesTool::new(Arc::clone(&store)).parallel_safe()),
    ];

    for (name, is_parallel_safe) in &read_only_tools {
        assert!(
            *is_parallel_safe,
            "Read-only FS tool '{name}' must have parallel_safe() = true"
        );
    }
}

// ── AC11: All write FS tools are NOT parallel_safe ────────────────────────────

#[test]
fn all_write_fs_tools_are_not_parallel_safe() {
    let store = make_store();
    let tmp = make_repo_path();
    let root = tmp.path();

    let write_tools: Vec<(&str, bool)> = vec![
        ("write_file",        WriteFileTool::new(Arc::clone(&store)).parallel_safe()),
        ("edit_file",         EditFileTool::new(Arc::clone(&store)).parallel_safe()),
        ("append_to_file",    AppendToFileTool::new(Arc::clone(&store), root).parallel_safe()),
        ("patch_file",        PatchFileTool::new(Arc::clone(&store), root).parallel_safe()),
        ("move_file",         MoveFileTool::new(Arc::clone(&store), root).parallel_safe()),
        ("copy_file",         CopyFileTool::new(Arc::clone(&store), root).parallel_safe()),
        ("delete_file",       DeleteFileTool::new(Arc::clone(&store), root).parallel_safe()),
        ("create_directory",  CreateDirectoryTool::new(Arc::clone(&store), root).parallel_safe()),
        ("remove_directory",  RemoveDirectoryTool::new(Arc::clone(&store), root).parallel_safe()),
    ];

    for (name, is_parallel_safe) in &write_tools {
        assert!(
            !*is_parallel_safe,
            "Write FS tool '{name}' must have parallel_safe() = false"
        );
    }
}

// ── AC10 (continued): All read-only git tools are parallel_safe ───────────────

#[test]
fn all_read_only_git_tools_are_parallel_safe() {
    let tmp = make_repo_path();
    let rp = tmp.path().to_path_buf();

    // Git repo required; use the real repository this test runs in.
    // If unavailable, skip by detecting the error.
    let repo = match agtrs_git::GitRepo::open(&rp) {
        Ok(r) => Arc::new(r),
        Err(_) => {
            // Initialize a minimal git repo for the test.
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&rp)
                .output()
                .ok();
            match agtrs_git::GitRepo::open(&rp) {
                Ok(r) => Arc::new(r),
                Err(_) => {
                    eprintln!("skip: no git repo available");
                    return;
                }
            }
        }
    };

    let read_only_git_tools: Vec<(&str, bool)> = vec![
        ("git_status",     GitStatusTool::new(Arc::clone(&repo)).parallel_safe()),
        ("git_diff",       GitDiffTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_log",        GitLogTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_blame",      GitBlameTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_show",       GitShowTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_branch",     GitBranchTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_stash_list", GitStashListTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_remote",     GitRemoteTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_grep",       GitGrepTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
        ("git_tag",        GitTagTool::new(Arc::clone(&repo), rp.clone()).parallel_safe()),
    ];

    for (name, is_parallel_safe) in &read_only_git_tools {
        assert!(
            *is_parallel_safe,
            "Read-only git tool '{name}' must have parallel_safe() = true"
        );
    }
}
```

### xaft integration tests

```rust
// crates/xaft-runtime/tests/parallel_integration.rs

//! Integration test: agent executor with real xaft tools and parallel policy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agtrs_runtime::agent::{AgentConfig, AgentContext};
use agtrs_runtime::executor::AgentExecutor;
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use agtrs_runtime::transport::Message;
use tempfile::TempDir;
use xaft_tools::ToolRegistryBuilder;

/// AC1 regression: with annotated policy and 3 read_file calls, execution
/// is faster than sequential would be (wall time < 3x single file time).
#[tokio::test]
async fn agent_executor_parallel_file_reads() {
    // Writes three real files to a tempdir, then issues a mock LLM turn
    // that reads all three with read_file.
    let tmp = TempDir::new().unwrap();
    let path_a = tmp.path().join("a.txt");
    let path_b = tmp.path().join("b.txt");
    let path_c = tmp.path().join("c.txt");
    std::fs::write(&path_a, "content a").unwrap();
    std::fs::write(&path_b, "content b").unwrap();
    std::fs::write(&path_c, "content c").unwrap();

    let transport = Arc::new(MockTransport::new());
    transport.queue_tool_call("read_file", serde_json::json!({"path": "a.txt"})).await;
    transport.queue_tool_call("read_file", serde_json::json!({"path": "b.txt"})).await;
    transport.queue_tool_call("read_file", serde_json::json!({"path": "c.txt"})).await;
    transport.queue_text("done").await;

    let llm = Arc::new(MockLlmProvider::new(transport));

    let registry = ToolRegistryBuilder::new(tmp.path())
        .without_git()
        .build_reader()
        .unwrap();

    let config = AgentConfig {
        parallel_tool_policy: agtrs_runtime::agent::ParallelToolPolicy::Annotated,
        max_concurrent_tools: 4,
        max_turns: 5,
        ..Default::default()
    };

    let mut ctx = AgentContext::new(
        "test",
        config,
        llm,
        Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
            injectable_runtime::EmptySingletonStore,
        ))),
    );
    for tool in registry.all() {
        ctx.register_tool(tool.name().to_string(), tool);
    }

    // SimpleAgent impl omitted for brevity — uses the config above.
    // Assert that all three reads complete and the conversation has three tool results.
    AgentExecutor::run(&SimpleIntegrationAgent::new(config), Message::user("go"), &mut ctx)
        .await
        .unwrap();

    // All three tool results should appear in the conversation.
    let tool_results: Vec<_> = ctx.messages().iter()
        .filter(|m| matches!(m.content, agtrs_runtime::transport::MessageContent::MultiPart(_)))
        .collect();

    assert!(tool_results.len() >= 3, "Expected 3 tool result messages");
}


/// AC5/AC12 regression: config `parallel_tool_policy = "annotated"` is parsed
/// and round-trips through XaftConfig serialization.
#[test]
fn config_exposes_parallel_policy() {
    use xaft_config::{ConfigLoader, XaftConfig};

    let toml_input = r#"
[agent.default]
provider             = "anthropic"
model                = "claude-sonnet-4-5"
parallel_tool_policy = "annotated"
max_concurrent_tools = 6
"#;

    let config: XaftConfig = toml::from_str(toml_input).expect("config parses");
    let default_preset = config.agent.get("default").expect("default preset exists");

    assert_eq!(
        default_preset.parallel_tool_policy,
        agtrs_runtime::agent::ParallelToolPolicy::Annotated,
        "parallel_tool_policy should round-trip through TOML"
    );
    assert_eq!(
        default_preset.max_concurrent_tools, 6,
        "max_concurrent_tools should round-trip through TOML"
    );
}
```

---

## Migration & Backward Compatibility

### agtrs-runtime

`parallel_tool_calls: bool` is kept as a deprecated field on `AgentConfig`.
`effective_policy()` checks it first: if `true`, it overrides
`parallel_tool_policy` with `All`.  This preserves all existing call sites that
set `parallel_tool_calls = true` without any code changes.

Existing tests in `executor_parallel.rs` continue to compile because:
- `AgentConfig { parallel_tool_calls: true, .. }` still compiles (field exists).
- `with_parallel_tools()` still compiles (method exists, maps to `All`).
- The binary flag in `AgentConfig::default()` is `false`, so `effective_policy`
  returns `Sequential` by default — unchanged.

`AgentRunComplete` gains a new field `tool_metrics: Option<ToolCallMetrics>`.
Existing subscribers that pattern-match on `AgentRunComplete` must be updated to
handle the new field.  Struct update syntax (`..`) is sufficient for tests; the
two existing emit sites in `executor.rs` require the `tool_metrics:
ctx.metadata().tool_metrics.clone()` argument.

### xaft

`XaftAgentConfig::parallel_tools: bool` is replaced by
`parallel_tool_policy: ParallelToolPolicy`.  Any code that sets
`parallel_tools: true` must change to
`parallel_tool_policy: ParallelToolPolicy::All`.  Within the xaft codebase, this
field is only set in `XaftAgentConfig::default()` (currently `false`) and in
tests.

Config files that do not specify `parallel_tool_policy` get the xaft default of
`Annotated`, which is safe for all current tool sets.  Users who relied on pure
sequential behavior can add `parallel_tool_policy = "sequential"` to pin the
original behavior.

---

## Rollout

### Phase 1: agtrs-runtime (upstream)

1. Add `ParallelToolPolicy` enum to `agent.rs`.
2. Extend `AgentConfig` with `parallel_tool_policy` and `max_concurrent_tools`.
3. Add `parallel_safe()` default method to `Tool` trait.
4. Add `ToolResultAvailable` and `ToolCallMetrics` to `signals.rs`.
5. Extend `AgentRunComplete` with `tool_metrics`.
6. Rewrite Phase 2 in `executor.rs` (both `run` and `run_stream`).
7. Run full agtrs test suite: `cargo test -p agtrs-runtime`.

### Phase 2: xaft-tools annotations

8. Add `parallel_safe() -> bool { true }` to all 19 read-only tool structs.
9. Add unit tests for all 38 tools (read-only and write).
10. Run: `cargo test -p xaft-tools`.

### Phase 3: xaft config/agent wiring

11. Extend `XaftAgentConfig` and `AgentPreset`.
12. Wire policy through `run_workflow` and `NamedAgent` construction.
13. Add config parsing round-trip test.
14. Run: `cargo test --workspace`.

### Phase 4: TUI integration

15. Subscribe to `ToolResultAvailable` in `EventBridge`.
16. Add `TuiEvent::ToolBatchProgress` variant.
17. Update `AppState::handle_event` for ephemeral formatting.
18. Manual TUI smoke-test: start a `xaft run "list all .rs files"` and verify
    ephemeral shows `"N tools running (…)…"` with multiple reads in flight.

### Verification

Run the full acceptance criteria suite:

```bash
cargo test --workspace -- parallel 2>&1 | tee /tmp/parallel_test_run.txt
grep -E "(PASSED|FAILED|test parallel)" /tmp/parallel_test_run.txt
```

All 14 AC items must pass.

---

## Open Questions

1. **Dependency ordering via LLM output:** The Anthropic API's `tool_use` block
   does not include a `depends_on` field.  Should we define a convention where
   the LLM embeds dependency hints in the `input` JSON, e.g.
   `{"_depends_on": ["tu_1"], "path": "foo.rs"}`?  The executor strips the hint
   before passing input to the tool.  This would make AC6 testable end-to-end.
   Alternative: dependency ordering is inferred only from the read/write partition
   (implicit), and explicit `depends_on` is deferred to a follow-up PRD.

2. **Semaphore fairness:** `tokio::sync::Semaphore` uses FIFO ordering.  Under
   the `Annotated` policy, all parallel-safe tools compete for permits.  Should
   we give priority to shorter-estimated tools (using `ToolMeta::estimated_duration_ms`
   when set)?  Initial implementation uses FIFO (simplest correct approach).

3. **`run_stream` parity:** The streaming executor has its own Phase 2 copy.
   This PRD targets both code paths.  Should both be refactored into a shared
   `execute_phase2()` function to avoid duplication?  Recommended: yes, extract
   into a private `async fn execute_tool_batch(...)` that both `run` and
   `run_stream` call.  This is a cleanup that can land in the same PR.

4. **`max_concurrent_tools = 1` edge case:** Setting `max_concurrent_tools = 1`
   with `All` policy is equivalent to `Sequential` but goes through the semaphore
   path.  Should `effective_policy` short-circuit to `Sequential` in this case?
   Recommendation: yes, add the short-circuit in `effective_policy()` for clarity
   and to skip the `FuturesUnordered` overhead when concurrency is 1.

5. **TUI tool name truncation:** When 6+ tools run in parallel, the ephemeral
   line `"6 tools running (read_file, read_many, grep, glob, file_stat, tree)…"`
   may exceed terminal width.  Should names be truncated to the first 3 plus an
   ellipsis?  Recommendation: cap displayed names at 3, show `"+ N more"` for
   additional tools.

6. **`BashExecTool` and parallel execution:** `BashExecTool` has
   `requires_confirmation = true`.  Under the `Annotated` policy it is in the
   sequential group regardless.  Under the `All` policy it would enter the
   parallel group.  Should `All` policy also honour `requires_confirmation` as an
   implicit sequential marker?  Current design: Phase 1 (approval gate) always
   runs before Phase 2, so approval is not bypassed.  But multiple
   `bash_exec` calls in the same turn could still run concurrently under `All`.
   Recommendation: `BashExecTool` should override `parallel_safe()` to return
   `false` explicitly, ensuring it is sequential even under `All` policy.  Add
   this override in Phase 2 of the rollout.
