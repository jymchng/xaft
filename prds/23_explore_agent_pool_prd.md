# ExploreAgent Pool — Parallel Repository Exploration PRD

## Background

The current xaft planner agent reads 1–3 files before deciding whether to hand off
to the coder. For large repositories this means the planner makes decisions with
incomplete context — it misses bugs in files it didn't read, or produces plans that
contradict the actual code structure.

agtrs ships `SubagentPool<T>` (`agtrs-runtime/src/subagent.rs`) that runs many
`SubagentTool<T>` instances concurrently behind a semaphore. Each subagent gets an
isolated `AgentContext` (no parent history), executes to completion, and returns a
typed result. The pool collects all results in input order.

The pattern is demonstrated in `agtrs-examples/src/bin/05_isolated_subagents.rs`:
- `SubagentTool::<T>::builder()...build()` wires up an agent with tools + return mode
- `SubagentPool::new(tool, max_concurrent)` bounds parallel execution  
- `pool.run_all(tasks)` returns `Vec<Result<T, AgtrsError>>` in task order

---

## Goal

Implement `ExploreAgentPool`: a pool of agents where each agent is assigned one file,
all agents run in parallel, and the aggregated results give the planner full-repository
context before it decides what to do.

The pool is exposed to the planner as a single tool: `explore_repository`. The planner
calls it once, the pool fans out, and the planner receives a structured report covering
every file.

---

## Architecture

```text
Planner
  ↓ calls explore_repository({ task, files })
  ┌────────────────────────────────────────────────────────┐
  │  ExploreRepositoryTool (wraps SubagentPool<FileSummary>) │
  │                                                        │
  │  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐  │
  │  │ExploreAgent 1│ │ExploreAgent 2│ │ExploreAgent N│  │
  │  │ file: a.py   │ │ file: b.py   │ │ file: c.py   │  │
  │  └──────┬───────┘ └──────┬───────┘ └──────┬───────┘  │
  │         └────────────────┴────────────────┘           │
  │                  Semaphore(max_concurrent=8)           │
  └────────────────────────────────────────────────────────┘
  ↓ returns RepositoryReport { files: Vec<FileSummary>, relevant: Vec<String> }
Planner decides: answer inline OR handoff_to_agent("coder", plan)
```

---

## Types

### `FileSummary` (new struct in `xaft-agents` or `xaft-runtime`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileSummary {
    /// Workspace-relative file path.
    pub path: String,
    /// One-sentence description of what the file does.
    pub description: String,
    /// Key symbols: public functions, classes, constants.
    pub key_symbols: Vec<String>,
    /// Whether this file is relevant to the user's task.
    pub relevant_to_task: bool,
    /// Specific issues or bugs observed (empty if none).
    pub issues: Vec<String>,
}
```

### `RepositoryReport` (returned to planner)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryReport {
    /// Summary of every explored file.
    pub files: Vec<FileSummary>,
    /// Paths of files relevant to the task (convenience subset).
    pub relevant_files: Vec<String>,
    /// Overall assessment for the planner.
    pub assessment: String,
}
```

---

## ExploreAgent prompt

Each agent receives one file path and the user's task. System prompt:

```
You are a code explorer. Read the file assigned to you and produce a structured summary.

TASK CONTEXT: {task}
FILE: {path}
WORKING DIRECTORY: {working_dir}

INSTRUCTIONS:
1. Call `read_file` with path="{path}" to read the file.
2. Analyse its contents relative to the task.
3. Output ONLY this JSON (no markdown, no preamble):
{
  "path": "{path}",
  "description": "one sentence",
  "key_symbols": ["fn1", "Class2"],
  "relevant_to_task": true,
  "issues": ["bug in line 42: division by zero"]
}
```

- Tools: `read_file` only (no write access, no list_files)
- `max_turns`: 2 (one turn to call read_file, one turn to produce JSON)
- `ReturnMode::DirectJson` (single JSON object, no extra LLM call)

---

## ExploreRepositoryTool

A tool given to the planner. When called, it:
1. Lists files (or uses the provided list)
2. Fans out to `SubagentPool<FileSummary>`
3. Aggregates results into `RepositoryReport`
4. Returns the report as a JSON ToolResult

Input schema:
```json
{
  "type": "object",
  "properties": {
    "task":  { "type": "string",  "description": "User's task for relevance filtering" },
    "paths": { "type": "array", "items": { "type": "string" },
               "description": "Files to explore; omit to explore all files" },
    "max_files": { "type": "integer", "default": 30,
                   "description": "Cap on files explored (cost guard)" }
  },
  "required": ["task"]
}
```

Implementation sketch (`xaft-runtime/src/orchestrator.rs` or new `xaft-agents/src/explorer.rs`):

```rust
pub struct ExploreRepositoryTool {
    pool: SubagentPool<FileSummary>,
    read_tools: Vec<Arc<ErasedTool>>,
    working_dir: String,
    max_files: usize,
}

impl Tool for ExploreRepositoryTool {
    fn name(&self) -> &str { "explore_repository" }
    fn description(&self) -> &str {
        "Explore all files in the repository in parallel. Returns a structured report \
         covering every file's purpose and any observed issues. Call this before \
         deciding whether to answer inline or hand off to the coder."
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let task    = input["task"].as_str().unwrap_or("");
        let cap     = input["max_files"].as_u64().unwrap_or(30) as usize;

        // If caller didn't supply paths, list them.
        let paths: Vec<String> = match input.get("paths") {
            Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => self.list_all_files().await?,
        };
        let paths: Vec<String> = paths.into_iter().take(cap).collect();

        // Build one task per file.
        let tasks: Vec<Value> = paths.iter().map(|p| serde_json::json!({
            "task": format!("File: {p}\nUser task: {task}")
        })).collect();

        // Run in parallel.
        let results = self.pool.run_all(tasks).await;

        // Aggregate.
        let summaries: Vec<FileSummary> = results.into_iter().zip(paths.iter())
            .map(|(r, path)| r.unwrap_or_else(|_| FileSummary {
                path: path.clone(),
                description: "exploration failed".into(),
                ..Default::default()
            }))
            .collect();

        let relevant: Vec<String> = summaries.iter()
            .filter(|s| s.relevant_to_task)
            .map(|s| s.path.clone())
            .collect();

        let report = RepositoryReport {
            files: summaries,
            relevant_files: relevant,
            assessment: String::new(), // planner synthesises
        };

        Ok(ToolResult::ok(serde_json::to_string(&report)?, &ctx.tool_use_id))
    }
}
```

---

## Integration with HandoffOrchestrator

In `run_workflow()` (orchestrator.rs), wire `ExploreRepositoryTool` into the planner:

```rust
let explore_tool = Arc::new(ExploreRepositoryTool::new(
    wd.clone(),
    read_tools.clone(),
    llm.clone(),
    resolve_ctx.clone(),
    max_concurrent = 8,
    max_files = 30,
)) as Arc<ErasedTool>;

let mut planner_tools: Vec<Arc<ErasedTool>> = read_tools.clone();
planner_tools.push(explore_tool);              // ← NEW: full-repo scan tool
planner_tools.push(Arc::new(HandoffTool::new_with_flag(
    Arc::clone(&handoff_store), vec!["coder".into()], Arc::clone(&planner_stop)
)));
```

Updated planner system prompt addition:

```
TOOLS:
- list_files: discover what files exist
- read_file: read a specific file
- explore_repository(task, paths?): PARALLEL full-repository scan — returns a
  RepositoryReport covering all files. Use this for tasks touching multiple files
  or when you need full context before deciding what to do.
- handoff_to_agent(target_agent="coder", reason=plan): hand off to coder
```

The planner now has the choice to either:
- Call `explore_repository` for a comprehensive scan (multi-file tasks, bugs, refactors)
- Call `read_file` for a quick targeted read (simple one-file tasks)
- Answer directly inline (informational tasks)

---

## `ExploreRepositoryTool::new` — construction

```rust
impl ExploreRepositoryTool {
    pub fn new(
        working_dir: impl Into<String>,
        read_tools: Vec<Arc<ErasedTool>>,
        llm: Arc<dyn LlmProvider>,
        resolve_ctx: Arc<ResolveContext>,
        max_concurrent: usize,
        max_files: usize,
    ) -> Self {
        let explore_agent = Arc::new(
            NamedAgent::new("explorer", &explorer_system_prompt(&working_dir), 2)
                .with_tools(read_tools.clone())
                // No signals — explorers run silently in parallel
        );

        let subagent_tool = Arc::new(
            SubagentTool::<FileSummary>::builder()
                .name("file_explorer")
                .description("Explore a single file")
                .subagent(Arc::clone(&explore_agent) as Arc<dyn Agent>)
                .llm(Arc::clone(&llm))
                .resolve_ctx(Arc::clone(&resolve_ctx))
                .system_prompt(&explorer_system_prompt(&working_dir))
                .max_turns(2)
                .return_mode(ReturnMode::DirectJson)
                .build()
        );

        let pool = SubagentPool::new(subagent_tool, max_concurrent);

        Self { pool, read_tools, working_dir: working_dir.into(), max_files }
    }
}
```

---

## Cost guardrails

| Control | Value | Purpose |
|---------|-------|---------|
| `max_concurrent` | 8 | Bound parallel LLM calls |
| `max_files` | 30 | Cap total files explored per task |
| `max_turns` per agent | 2 | 1 tool call + 1 response only |
| `max_cost_usd` per agent | `0.01` | Hard abort per-agent if model is expensive |
| `FileSummary` uses cheap model | via CostedProvider | Explorers route to fast model tier |

---

## Files to create / modify

| File | Change |
|---|---|
| `xaft-runtime/src/orchestrator.rs` | Add `ExploreRepositoryTool`, `FileSummary`, `RepositoryReport`; wire into planner tool list; update planner prompt |
| OR `xaft-agents/src/explorer.rs` | New crate if xaft-agents is implemented per PRD 21 |
| `xaft-runtime/src/orchestrator.rs` | Update `planner_prompt()` to mention `explore_repository` tool |
| `xaft-runtime/tests/explore_pool.rs` | **New** — pool tests (see below) |

---

## Tests

### Unit tests

```
explore_repository_tool_returns_report_for_all_files
  // 3 mock files, 3 queued LLM responses (one FileSummary JSON each)
  // Assert: RepositoryReport.files.len() == 3

explore_tool_caps_at_max_files
  // 50 files listed, max_files=10
  // Assert: only 10 explore tasks launched

explore_tool_handles_single_agent_failure_gracefully
  // 3 tasks, middle one errors
  // Assert: report has 3 entries, failed one has description="exploration failed"

file_summary_direct_json_parses_correctly
  // Queue: valid FileSummary JSON
  // Assert: FileSummary struct deserialized correctly
```

### Integration tests

```
explore_pool_runs_agents_concurrently
  // 4 tasks with max_concurrent=2
  // Assert: at most 2 agents run simultaneously (via SubagentCompleted signal timestamps)

planner_uses_explore_tool_for_multi_file_task
  // Queue: planner calls explore_repository → gets report → calls handoff_to_agent("coder")
  // Assert: HandoffResult.agent_name == "coder"
  // Assert: plan includes file names from the report

planner_answers_directly_after_explore_for_info_task
  // Queue: planner calls explore_repository → outputs answer inline
  // Assert: HandoffResult.agent_name == "planner"
```

---

## Acceptance Criteria

1. `ExploreRepositoryTool::call()` fans out to `SubagentPool<FileSummary>` and returns
   a valid `RepositoryReport`.
2. Each explorer agent has isolated context (no parent history).
3. `max_concurrent` semaphore is respected — at most N agents run simultaneously.
4. `max_files` cap prevents unbounded cost on large repos.
5. Planner receives `explore_repository` as a tool and uses it for multi-file tasks.
6. All existing session_e2e.rs tests continue to pass.
7. All new pool tests pass.
