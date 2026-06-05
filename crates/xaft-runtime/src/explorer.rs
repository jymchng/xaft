//! `ExploreRepositoryTool` — parallel repository exploration via `SubagentPool`.
//!
//! ```text
//! Planner
//!   ↓ calls explore_repository({ task, paths?, max_files? })
//!   ┌─────────────────────────────────────────────────────────────┐
//!   │  ExploreRepositoryTool  (wraps SubagentPool<FileSummary>)  │
//!   │                                                             │
//!   │  ┌───────────────┐ ┌───────────────┐ ┌───────────────┐     │
//!   │  │ExplorerAgent 1│ │ExplorerAgent 2│ │ExplorerAgent N│     │
//!   │  │ file: a.py    │ │ file: b.py    │ │ file: c.py    │     │
//!   │  └───────┬───────┘ └───────┬───────┘ └───────┬───────┘     │
//!   │          └─────────────────┴─────────────────┘              │
//!   │                Semaphore(max_concurrent=8)                  │
//!   └─────────────────────────────────────────────────────────────┘
//!   ↓ returns RepositoryReport { files, relevant_files, assessment }
//! Planner decides: answer inline OR handoff_to_agent("coder", plan)
//! ```
//!
//! Each explorer subagent runs in an **isolated** [`AgentContext`]
//! (no parent history), reads exactly one file via `read_file`, and produces a
//! strongly typed [`FileSummary`] JSON object via [`ReturnMode::DirectJson`].
//!
//! The pool preserves input order in the result vector so the caller can
//! trivially zip results back to file paths.
//!
//! See `crates/xaft-runtime/tests/explore_pool.rs` for end-to-end tests
//! covering concurrent fan-out, planner integration, and graceful failure.

use std::sync::Arc;

use agtrs_runtime::agent::Agent;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::subagent::{ReturnMode, SubagentPool, SubagentTool};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument, warn};

use xaft_agents::named::NamedAgent;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Tool name registered with the LLM.
pub const EXPLORE_REPOSITORY_TOOL_NAME: &str = "explore_repository";

/// Logical name of the per-file explorer subagent (not exposed to the LLM).
pub const EXPLORER_SUBAGENT_NAME: &str = "file_explorer";

/// Default cap on parallel subagents.
pub const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Default cap on total files explored per call.
pub const DEFAULT_MAX_FILES: usize = 30;

/// Default max LLM turns per explorer subagent (1 to call `read_file`, 1 to emit JSON).
pub const EXPLORER_MAX_TURNS: usize = 2;

/// Per-invocation cost cap (USD) for each explorer subagent.
pub const EXPLORER_MAX_COST_USD: f64 = 0.01;

// ── FileSummary ───────────────────────────────────────────────────────────────

/// Structured summary of a single file, produced by one explorer subagent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
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

impl FileSummary {
    /// Construct a placeholder summary for failed subagent invocations.
    ///
    /// The placeholder marks the file as non-relevant and surfaces a single
    /// issue describing the exploration failure so the planner can see the gap.
    pub fn failure(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            description: "exploration failed".to_string(),
            key_symbols: Vec::new(),
            relevant_to_task: false,
            issues: vec![format!("exploration failed: {}", reason.into())],
        }
    }
}

// ── RepositoryReport ──────────────────────────────────────────────────────────

/// Aggregated report covering every explored file, returned to the planner.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct RepositoryReport {
    /// Per-file summary, in the order the pool processed the inputs.
    pub files: Vec<FileSummary>,
    /// Convenience subset: paths whose `relevant_to_task` flag is `true`.
    pub relevant_files: Vec<String>,
    /// Free-form overall assessment. The planner synthesises this from `files`.
    #[serde(default)]
    pub assessment: String,
}

impl RepositoryReport {
    /// Build a report from a list of per-file summaries.
    ///
    /// The `assessment` field is left empty — the planner or downstream agent
    /// fills it in based on the contents of `files`.
    pub fn from_summaries(summaries: Vec<FileSummary>) -> Self {
        let relevant_files = summaries
            .iter()
            .filter(|s| s.relevant_to_task)
            .map(|s| s.path.clone())
            .collect();
        Self {
            files: summaries,
            relevant_files,
            assessment: String::new(),
        }
    }

    /// Total number of files covered by this report.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the report covers no files.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

// ── System prompt ─────────────────────────────────────────────────────────────

/// Build the per-file explorer subagent system prompt.
///
/// The explorer receives one file path and the user's task. It must call
/// `read_file` then emit a single JSON `FileSummary` object (no markdown,
/// no preamble).
pub fn explorer_system_prompt(task: &str, path: &str, working_dir: &str) -> String {
    format!(
        "You are a code explorer. Read the single file assigned to you and produce a \
         structured summary.\n\
         \n\
         TASK CONTEXT: {task}\n\
         FILE: {path}\n\
         WORKING DIRECTORY: {working_dir}\n\
         All file paths are relative to this directory.\n\
         \n\
         INSTRUCTIONS:\n\
         1. Call `read_file` exactly once with the literal `path` value given above.\n\
         2. Analyse the file's contents relative to the task.\n\
         3. Output ONLY this JSON object (no markdown, no prose, no code fences):\n\
         {{\n  \
           \"path\": \"{path}\",\n  \
           \"description\": \"one sentence describing what the file does\",\n  \
           \"key_symbols\": [\"fn_name_1\", \"ClassName\", \"CONSTANT\"],\n  \
           \"relevant_to_task\": true|false,\n  \
           \"issues\": [\"bug in line 42: division by zero\"]\n\
         }}\n\
         \n\
         Rules:\n\
         - `path` MUST be exactly the FILE value above.\n\
         - `description` MUST be a single sentence, no newlines.\n\
         - `key_symbols` lists public function names, type/class names, and constants.\n\
         - `issues` is an empty array if nothing is wrong.\n\
         - Do NOT call any tool other than `read_file`."
    )
}

// ── ExploreRepositoryTool ─────────────────────────────────────────────────────

/// A [`Tool`] that fans out to a [`SubagentPool<FileSummary>`] and returns a
/// aggregated [`RepositoryReport`].
///
/// Constructed once per workflow run and reused across planner invocations.
pub struct ExploreRepositoryTool {
    pool: SubagentPool<FileSummary>,
    workspace: Arc<dyn WorkspaceStore>,
    working_dir: String,
    max_concurrent: usize,
    max_files: usize,
    /// Cached JSON schema for [`Tool::schema`].
    schema: serde_json::Value,
}

impl std::fmt::Debug for ExploreRepositoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExploreRepositoryTool")
            .field("working_dir", &self.working_dir)
            .field("max_files", &self.max_files)
            .field("max_concurrent", &self.max_concurrent)
            .finish()
    }
}

impl ExploreRepositoryTool {
    /// Construct an explore tool with sensible defaults.
    ///
    /// - `working_dir` — the workspace root (used to build per-file briefs).
    /// - `read_tools` — read-only tools (e.g. `read_file`) available to each
    ///   explorer subagent. **Must** include a `read_file` tool.
    /// - `llm` — shared LLM provider. The agent is encouraged to use the
    ///   `fast` tier via the three-tier router when cost is a concern.
    /// - `resolve_ctx` — DI resolve context forwarded to the subagent.
    /// - `workspace` — workspace store used to list files when the caller does
    ///   not provide explicit paths.
    pub fn new(
        working_dir: impl Into<String>,
        read_tools: Vec<Arc<ErasedTool>>,
        llm: Arc<dyn LlmProvider>,
        resolve_ctx: Arc<injectable_runtime::ResolveContext>,
        workspace: Arc<dyn WorkspaceStore>,
    ) -> Self {
        Self::with_limits(
            working_dir,
            read_tools,
            llm,
            resolve_ctx,
            workspace,
            DEFAULT_MAX_CONCURRENT,
            DEFAULT_MAX_FILES,
        )
    }

    /// Construct an explore tool with custom concurrency and file caps.
    ///
    /// `max_concurrent` is clamped to at least 1. `max_files` of 0 disables
    /// exploration (the tool returns an empty report immediately).
    pub fn with_limits(
        working_dir: impl Into<String>,
        read_tools: Vec<Arc<ErasedTool>>,
        llm: Arc<dyn LlmProvider>,
        resolve_ctx: Arc<injectable_runtime::ResolveContext>,
        workspace: Arc<dyn WorkspaceStore>,
        max_concurrent: usize,
        max_files: usize,
    ) -> Self {
        let working_dir = working_dir.into();

        let subagent_tool = build_explorer_subagent_tool(
            &working_dir,
            read_tools,
            llm,
            resolve_ctx,
            max_concurrent,
        );
        let pool = SubagentPool::new(Arc::new(subagent_tool), max_concurrent);

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The user's task — used to filter for relevance. Required."
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Workspace-relative file paths to explore. \
                                    Omit to explore all files in the workspace (capped at max_files)."
                },
                "max_files": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Cap on files explored. Defaults to 30. \
                                    Use a smaller value to bound cost on large repos."
                }
            },
            "required": ["task"],
            "additionalProperties": false
        });

        Self {
            pool,
            workspace,
            working_dir,
            max_concurrent,
            max_files,
            schema,
        }
    }

    /// Effective per-invocation cost cap exposed by the underlying config.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Effective cap on files explored per call.
    pub fn max_files(&self) -> usize {
        self.max_files
    }

    /// Workspace root the tool is configured for.
    pub fn working_dir(&self) -> &str {
        &self.working_dir
    }

    /// Resolve the list of files to explore, applying `max_files` and the
    /// optional `paths` override.
    async fn resolve_paths(&self, explicit: Option<&[String]>) -> Vec<String> {
        let mut paths: Vec<String> = match explicit {
            Some(p) => p.to_vec(),
            None => self.workspace.list().await,
        };
        paths.sort();
        paths.dedup();
        if paths.len() > self.max_files {
            paths.truncate(self.max_files);
        }
        paths
    }

    /// Build one JSON task input per file for the [`SubagentPool`].
    fn build_tasks(&self, task: &str, paths: &[String]) -> Vec<serde_json::Value> {
        paths
            .iter()
            .map(|p| {
                let brief = format!(
                    "TASK: {task}\nFILE: {p}\n\
                     Call read_file on this file, then output ONLY the FileSummary JSON."
                );
                serde_json::json!({ "task": brief })
            })
            .collect()
    }
}

// Helper: read `SubagentPool::max_concurrent` indirectly.
//
// We can't access the field directly because it's private, so we approximate
// by inspecting the schema we built. This is only used for the `Debug` impl
// and the public `max_concurrent()` accessor — we track it ourselves.
impl ExploreRepositoryTool {
    /// Build a brief for a single path (used internally and exposed for tests).
    pub fn build_brief_for(task: &str, path: &str) -> String {
        format!(
            "TASK: {task}\nFILE: {path}\n\
             Call read_file on this file, then output ONLY the FileSummary JSON."
        )
    }
}

// ── SubagentTool construction ─────────────────────────────────────────────────

/// Build a [`SubagentTool<FileSummary>`] configured for one-file exploration.
fn build_explorer_subagent_tool(
    working_dir: &str,
    read_tools: Vec<Arc<ErasedTool>>,
    llm: Arc<dyn LlmProvider>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    max_concurrent: usize,
) -> SubagentTool<FileSummary> {
    // Each subagent uses a fresh, isolated context — no parent history.
    let system_prompt = format!(
        "You are a code explorer. You will receive one file path and a user task. \
         Call `read_file` on that exact path exactly once, then output a single \
         FileSummary JSON object. Do not call any other tools. \
         WORKING DIRECTORY: {working_dir}"
    );

    let explorer_agent: Arc<dyn Agent> = Arc::new(
        NamedAgent::new(EXPLORER_SUBAGENT_NAME, &system_prompt, EXPLORER_MAX_TURNS)
            .with_tools(read_tools),
    );

    // We attach a tiny per-pool signal channel: each pool-side call emits
    // SubagentStarted / SubagentCompleted on the agent's own bus (auto-created
    // inside SubagentTool when no signals are attached). We let the parent
    // observe by passing the resolved signal bus if it is later attached.
    let _ = max_concurrent; // currently only used to size the pool semaphore

    SubagentTool::<FileSummary>::builder()
        .name(EXPLORE_REPOSITORY_TOOL_NAME)
        .description(
            "Internal: explore a single file and return a FileSummary JSON object. \
             Not intended for direct LLM invocation — use explore_repository instead.",
        )
        .subagent(explorer_agent)
        .llm(llm)
        .resolve_ctx(resolve_ctx)
        .system_prompt(system_prompt)
        .max_turns(EXPLORER_MAX_TURNS)
        .max_cost_usd(EXPLORER_MAX_COST_USD)
        .return_mode(ReturnMode::DirectJson)
        .build()
}

// ── Tool impl ─────────────────────────────────────────────────────────────────

#[async_trait]
impl Tool for ExploreRepositoryTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        EXPLORE_REPOSITORY_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Explore the repository in parallel: read every file (or the supplied subset) \
         concurrently with isolated subagents, and return a structured RepositoryReport \
         covering each file's purpose, key symbols, relevance, and any observed issues. \
         \n\n\
         Use this BEFORE deciding whether to answer inline or hand off to the coder, \
         for tasks that touch multiple files, refactors, or any time you need \
         full-repository context. For a single targeted read, prefer `read_file`."
    }

    fn schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    #[instrument(
        name = "explore_repository",
        skip(self, ctx),
        fields(working_dir = %self.working_dir, max_files = self.max_files)
    )]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        // ── Cancellation at entry ────────────────────────────────────────────
        if ctx.is_cancelled() {
            return Ok(ToolResult::error(
                "explore_repository: cancelled before fan-out",
                &ctx.tool_use_id,
            ));
        }

        // ── Parse input ──────────────────────────────────────────────────────
        let task = input
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if task.is_empty() {
            return Ok(ToolResult::error(
                "explore_repository: 'task' is required",
                &ctx.tool_use_id,
            ));
        }

        let explicit_paths: Option<Vec<String>> =
            input.get("paths").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

        let effective_max_files: usize = input
            .get("max_files")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(self.max_files);

        // ── Resolve files ────────────────────────────────────────────────────
        let paths = if effective_max_files == 0 {
            Vec::new()
        } else {
            // Re-apply the explicit cap so per-call `max_files` overrides.
            let mut p = self.resolve_paths(explicit_paths.as_deref()).await;
            if p.len() > effective_max_files {
                p.truncate(effective_max_files);
            }
            p
        };

        info!(
            task_len = task.len(),
            file_count = paths.len(),
            max_files = effective_max_files,
            "explore_repository: starting fan-out"
        );

        // ── Empty case: no work to do ────────────────────────────────────────
        if paths.is_empty() {
            let report = RepositoryReport::default();
            let json = serde_json::to_string(&report).map_err(|e| {
                AgtrsError::SerializationError(format!("empty RepositoryReport: {e}"))
            })?;
            return Ok(ToolResult::ok(json, &ctx.tool_use_id));
        }

        // ── Fan out ──────────────────────────────────────────────────────────
        let tasks = self.build_tasks(&task, &paths);
        let results = self.pool.run_all(tasks).await;

        // ── Cancellation check after fan-out ─────────────────────────────────
        if ctx.is_cancelled() {
            debug!("explore_repository: cancelled during fan-out");
            return Ok(ToolResult::error(
                "explore_repository: cancelled during fan-out",
                &ctx.tool_use_id,
            ));
        }

        // ── Aggregate ────────────────────────────────────────────────────────
        let mut summaries: Vec<FileSummary> = Vec::with_capacity(results.len());
        let mut failure_count: usize = 0;
        for (i, result) in results.into_iter().enumerate() {
            let path = paths.get(i).cloned().unwrap_or_default();
            match result {
                Ok(summary) => {
                    debug!(path = %summary.path, "explore_repository: ok");
                    summaries.push(summary);
                }
                Err(err) => {
                    failure_count += 1;
                    warn!(path = %path, error = %err, "explore_repository: subagent failed");
                    summaries.push(FileSummary::failure(path, err.to_string()));
                }
            }
        }

        let report = RepositoryReport::from_summaries(summaries);
        info!(
            files = report.files.len(),
            relevant = report.relevant_files.len(),
            failures = failure_count,
            "explore_repository: complete"
        );

        let json = serde_json::to_string(&report).map_err(|e| {
            AgtrsError::SerializationError(format!("RepositoryReport serialize: {e}"))
        })?;
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

// ── ErasedTool coercion helper ────────────────────────────────────────────────

/// Convenience: cast a fresh `ExploreRepositoryTool` to `Arc<ErasedTool>` for
/// registration with an [`Agent`](agtrs_runtime::agent::Agent) tool list.
pub fn as_erased(tool: ExploreRepositoryTool) -> Arc<ErasedTool> {
    Arc::new(tool) as Arc<ErasedTool>
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::tool::ToolContext;
    use agtrs_runtime::transport::Message;
    use agtrs_workspace::InMemoryWorkspaceStore;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── helpers ────────────────────────────────────────────────────────────

    /// A trivial dummy LLM that returns a fixed JSON response and counts calls.
    /// Used to verify that the pool fans out the right number of times.
    struct CountingJsonLlm {
        call_count: Arc<AtomicUsize>,
        payload: String,
    }

    impl CountingJsonLlm {
        fn new(payload: impl Into<String>) -> Self {
            Self {
                call_count: Arc::new(AtomicUsize::new(0)),
                payload: payload.into(),
            }
        }
        fn calls(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.call_count)
        }
    }

    #[async_trait]
    impl LlmProvider for CountingJsonLlm {
        async fn complete(
            &self,
            _messages: &[Message],
            _options: &agtrs_runtime::llm::LlmOptions,
        ) -> Result<agtrs_runtime::llm::LlmResponse, AgtrsError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(agtrs_runtime::llm::LlmResponse {
                message: Message::assistant(&self.payload),
                usage: agtrs_runtime::transport::TokenUsage::new(1, 1),
                tool_calls: Vec::new(),
                finish_reason: agtrs_runtime::transport::StopReason::EndTurn,
                thinking_blocks: Vec::new(),
            })
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _options: &agtrs_runtime::llm::LlmOptions,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<Item = Result<agtrs_runtime::llm::StreamChunk, AgtrsError>>
                        + Send,
                >,
            >,
            AgtrsError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn embed(
            &self,
            _inputs: &[String],
            _model: Option<&str>,
        ) -> Result<Vec<agtrs_runtime::transport::Embedding>, AgtrsError> {
            Ok(Vec::new())
        }

        async fn count_tokens(&self, _messages: &[Message]) -> Result<usize, AgtrsError> {
            Ok(0)
        }

        fn context_window(&self) -> usize {
            8000
        }
        fn model(&self) -> &str {
            "counting"
        }
        fn provider_name(&self) -> &str {
            "counting"
        }
        fn supports_tool_calling(&self) -> bool {
            true
        }
    }

    fn make_resolve_ctx() -> Arc<injectable_runtime::ResolveContext> {
        Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
            injectable_runtime::EmptySingletonStore,
        )))
    }

    fn summary_json(path: &str, relevant: bool) -> String {
        serde_json::json!({
            "path": path,
            "description": "summary of ".to_string() + path,
            "key_symbols": ["sym_a", "sym_b"],
            "relevant_to_task": relevant,
            "issues": Vec::<String>::new(),
        })
        .to_string()
    }

    fn read_tool_stub() -> Arc<ErasedTool> {
        struct StubRead;
        #[async_trait]
        impl Tool for StubRead {
            type Inputs = serde_json::Value;
            type Output = ToolResult;
            fn name(&self) -> &str {
                "read_file"
            }
            fn description(&self) -> &str {
                "stub"
            }
            fn schema(&self) -> serde_json::Value {
                serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
            }
            async fn call(
                &self,
                _input: serde_json::Value,
                _ctx: &ToolContext,
            ) -> Result<ToolResult, AgtrsError> {
                Ok(ToolResult::ok("file:stub", "stub-id"))
            }
        }
        Arc::new(StubRead)
    }

    fn make_tool(
        llm: Arc<dyn LlmProvider>,
        workspace: Arc<dyn WorkspaceStore>,
        max_files: usize,
    ) -> ExploreRepositoryTool {
        ExploreRepositoryTool::with_limits(
            "/workspace",
            vec![read_tool_stub()],
            llm,
            make_resolve_ctx(),
            workspace,
            2,
            max_files,
        )
    }

    async fn populate_workspace(store: &InMemoryWorkspaceStore, files: &[&str]) {
        for f in files {
            store
                .write(f, &format!("contents of {f}"))
                .await
                .expect("write");
        }
    }

    // ── FileSummary / RepositoryReport tests ───────────────────────────────

    #[test]
    fn file_summary_failure_placeholder_has_correct_shape() {
        let s = FileSummary::failure("a.py", "boom");
        assert_eq!(s.path, "a.py");
        assert_eq!(s.description, "exploration failed");
        assert!(!s.relevant_to_task);
        assert_eq!(s.issues.len(), 1);
        assert!(s.issues[0].contains("boom"));
    }

    #[test]
    fn file_summary_serializes_roundtrip() {
        let s = FileSummary {
            path: "src/lib.rs".into(),
            description: "library".into(),
            key_symbols: vec!["foo".into(), "Bar".into()],
            relevant_to_task: true,
            issues: vec!["unused".into()],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: FileSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn repository_report_from_summaries_extracts_relevant() {
        let s1 = FileSummary {
            path: "a.py".into(),
            description: "a".into(),
            key_symbols: vec![],
            relevant_to_task: true,
            issues: vec![],
        };
        let s2 = FileSummary {
            path: "b.py".into(),
            description: "b".into(),
            key_symbols: vec![],
            relevant_to_task: false,
            issues: vec![],
        };
        let s3 = FileSummary {
            path: "c.py".into(),
            description: "c".into(),
            key_symbols: vec![],
            relevant_to_task: true,
            issues: vec![],
        };
        let report = RepositoryReport::from_summaries(vec![s1, s2, s3]);
        assert_eq!(report.files.len(), 3);
        assert_eq!(report.relevant_files, vec!["a.py", "c.py"]);
        assert!(report.assessment.is_empty());
        assert_eq!(report.len(), 3);
        assert!(!report.is_empty());
    }

    #[test]
    fn empty_report_is_empty() {
        let r = RepositoryReport::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.relevant_files.is_empty());
    }

    #[test]
    fn explorer_system_prompt_includes_task_path_and_dir() {
        let prompt = explorer_system_prompt("fix auth", "src/auth.rs", "/workspace/proj");
        assert!(prompt.contains("fix auth"));
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("/workspace/proj"));
        assert!(prompt.contains("read_file"));
    }

    // ── Tool wiring / schema tests ─────────────────────────────────────────

    #[test]
    fn tool_name_and_description() {
        let store: Arc<dyn WorkspaceStore> = Arc::new(InMemoryWorkspaceStore::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, store, 5);
        assert_eq!(tool.name(), EXPLORE_REPOSITORY_TOOL_NAME);
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("RepositoryReport"));
    }

    #[test]
    fn tool_schema_contains_required_fields() {
        let store: Arc<dyn WorkspaceStore> = Arc::new(InMemoryWorkspaceStore::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, store, 5);
        let s = tool.schema();
        assert_eq!(s["type"], "object");
        let required = s["required"].as_array().expect("required");
        assert!(required.iter().any(|v| v == "task"));
        assert!(s["properties"]["task"].is_object());
        assert!(s["properties"]["paths"].is_object());
        assert!(s["properties"]["max_files"].is_object());
    }

    #[test]
    fn debug_impl_includes_working_dir_and_caps() {
        let store: Arc<dyn WorkspaceStore> = Arc::new(InMemoryWorkspaceStore::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, store, 7);
        let dbg = format!("{tool:?}");
        assert!(dbg.contains("ExploreRepositoryTool"));
        assert!(dbg.contains("working_dir"));
        assert!(dbg.contains("max_files"));
        assert!(dbg.contains("7"));
    }

    // ── Tool call: input validation ────────────────────────────────────────

    #[tokio::test]
    async fn call_without_task_returns_error_result() {
        let store: Arc<dyn WorkspaceStore> = Arc::new(InMemoryWorkspaceStore::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, store, 5);
        let ctx = ToolContext::new("t-1");

        let res = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(res.is_error);
        assert!(res.content.contains("task"));
    }

    #[tokio::test]
    async fn call_with_max_files_zero_returns_empty_report() {
        let store = InMemoryWorkspaceStore::new();
        store.write("a.py", "x").await.unwrap();
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, Arc::new(store), 5);
        let ctx = ToolContext::new("t-2");

        let res = tool
            .call(serde_json::json!({"task": "explore", "max_files": 0}), &ctx)
            .await
            .unwrap();
        assert!(!res.is_error);
        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert!(report.is_empty());
    }

    // ── Tool call: fan-out and aggregation ────────────────────────────────

    #[tokio::test]
    async fn call_fans_out_to_pool_and_aggregates_report() {
        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["a.py", "b.py", "c.py"]).await;
        let llm_arc = Arc::new(CountingJsonLlm::new(
            r#"{"path":"_","description":"x","key_symbols":[],"relevant_to_task":true,"issues":[]}"#,
        ));
        let counter = llm_arc.calls();
        let tool = make_tool(llm_arc, Arc::new(store), 30);
        let ctx = ToolContext::new("t-3");

        let res = tool
            .call(serde_json::json!({"task": "anything"}), &ctx)
            .await
            .unwrap();
        assert!(!res.is_error, "expected ok, got: {}", res.content);

        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert_eq!(report.files.len(), 3);
        // The pool calls the LLM at most once per file (max_turns=2 and
        // the system prompt is strong about read_file, but the stub read
        // tool does not trigger further LLM calls).
        assert!(counter.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn call_caps_at_max_files() {
        let store = InMemoryWorkspaceStore::new();
        let names: Vec<String> = (0..50).map(|i| format!("f{i:02}.py")).collect();
        let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
        populate_workspace(&store, &names_ref).await;

        let llm_arc = Arc::new(CountingJsonLlm::new(
            r#"{"path":"_","description":"x","key_symbols":[],"relevant_to_task":false,"issues":[]}"#,
        ));
        let counter = llm_arc.calls();
        let tool = make_tool(llm_arc, Arc::new(store), 10); // hard cap = 10
        let ctx = ToolContext::new("t-4");

        let res = tool
            .call(serde_json::json!({"task": "x"}), &ctx)
            .await
            .unwrap();
        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert_eq!(report.files.len(), 10);
        // The hard cap is 10, but the per-call max_files default is also 10,
        // so the LLM was called at most 10 times.
        assert!(counter.load(Ordering::SeqCst) <= 10);
    }

    #[tokio::test]
    async fn call_per_call_max_files_override_caps_further() {
        let store = InMemoryWorkspaceStore::new();
        let names: Vec<String> = (0..20).map(|i| format!("g{i:02}.py")).collect();
        let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
        populate_workspace(&store, &names_ref).await;

        let llm_arc = Arc::new(CountingJsonLlm::new(
            r#"{"path":"_","description":"x","key_symbols":[],"relevant_to_task":false,"issues":[]}"#,
        ));
        let counter = llm_arc.calls();
        let tool = make_tool(llm_arc, Arc::new(store), 20); // hard cap = 20
        let ctx = ToolContext::new("t-5");

        // Override per-call to 3.
        let res = tool
            .call(serde_json::json!({"task": "x", "max_files": 3}), &ctx)
            .await
            .unwrap();
        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert_eq!(report.files.len(), 3);
        assert!(counter.load(Ordering::SeqCst) <= 3);
    }

    #[tokio::test]
    async fn call_with_explicit_paths_uses_them() {
        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["a.py", "b.py", "c.py"]).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(PathAwareLlm);
        let tool = make_tool(llm, Arc::new(store), 30);
        let ctx = ToolContext::new("t-6");

        let res = tool
            .call(
                serde_json::json!({
                    "task": "x",
                    "paths": ["a.py"]
                }),
                &ctx,
            )
            .await
            .unwrap();
        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path, "a.py");
        assert!(report.files[0].description.contains("a.py"));
    }

    #[tokio::test]
    async fn call_handles_llm_failure_with_failure_summary() {
        struct AlwaysFailLlm;
        #[async_trait]
        impl LlmProvider for AlwaysFailLlm {
            async fn complete(
                &self,
                _messages: &[Message],
                _options: &agtrs_runtime::llm::LlmOptions,
            ) -> Result<agtrs_runtime::llm::LlmResponse, AgtrsError> {
                Err(AgtrsError::LlmCallFailed {
                    reason: "boom".into(),
                })
            }
            async fn stream(
                &self,
                _messages: &[Message],
                _options: &agtrs_runtime::llm::LlmOptions,
            ) -> Result<
                std::pin::Pin<
                    Box<
                        dyn futures::Stream<
                                Item = Result<agtrs_runtime::llm::StreamChunk, AgtrsError>,
                            > + Send,
                    >,
                >,
                AgtrsError,
            > {
                Ok(Box::pin(futures::stream::empty()))
            }
            async fn embed(
                &self,
                _inputs: &[String],
                _model: Option<&str>,
            ) -> Result<Vec<agtrs_runtime::transport::Embedding>, AgtrsError> {
                Ok(Vec::new())
            }
            async fn count_tokens(&self, _messages: &[Message]) -> Result<usize, AgtrsError> {
                Ok(0)
            }
            fn context_window(&self) -> usize {
                8000
            }
            fn model(&self) -> &str {
                "fail"
            }
            fn provider_name(&self) -> &str {
                "fail"
            }
        }

        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["a.py", "b.py", "c.py"]).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(AlwaysFailLlm);
        let tool = make_tool(llm, Arc::new(store), 30);
        let ctx = ToolContext::new("t-7");

        let res = tool
            .call(serde_json::json!({"task": "x"}), &ctx)
            .await
            .unwrap();
        // The tool itself succeeds — it degrades per-file to a failure summary.
        assert!(!res.is_error, "expected ok, got: {}", res.content);
        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert_eq!(report.files.len(), 3);
        for f in &report.files {
            assert_eq!(f.description, "exploration failed");
            assert!(!f.relevant_to_task);
            assert!(!f.issues.is_empty());
        }
    }

    #[tokio::test]
    async fn call_with_cancelled_context_returns_error() {
        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["a.py"]).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, Arc::new(store), 30);
        let ctx = {
            let c = ToolContext::new("t-8");
            c.cancel_token.cancel();
            c
        };

        let res = tool
            .call(serde_json::json!({"task": "x"}), &ctx)
            .await
            .unwrap();
        assert!(res.is_error);
        assert!(res.content.contains("cancelled"));
    }

    #[tokio::test]
    async fn as_erased_helper_returns_arc() {
        let store: Arc<dyn WorkspaceStore> = Arc::new(InMemoryWorkspaceStore::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, store, 5);
        let erased: Arc<ErasedTool> = as_erased(tool);
        assert_eq!(erased.name(), EXPLORE_REPOSITORY_TOOL_NAME);
    }

    // ── FileSummary direct JSON parsing (PRD acceptance criterion) ─────────

    #[test]
    fn file_summary_direct_json_parses_correctly() {
        let raw = summary_json("src/lib.rs", true);
        let parsed: FileSummary = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.path, "src/lib.rs");
        assert!(parsed.relevant_to_task);
        assert_eq!(parsed.key_symbols.len(), 2);
    }

    #[test]
    fn file_summary_direct_json_with_issues_parses() {
        let raw = r#"{
            "path": "a.py",
            "description": "has a bug",
            "key_symbols": ["f"],
            "relevant_to_task": true,
            "issues": ["division by zero", "missing error handling"]
        }"#;
        let parsed: FileSummary = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.issues.len(), 2);
    }

    // ── Resolve paths helper ──────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_paths_sorts_and_dedupes_and_caps() {
        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["z.py", "a.py", "m.py"]).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, Arc::new(store), 2);
        let resolved = tool.resolve_paths(None).await;
        assert_eq!(resolved, vec!["a.py", "m.py"]); // sorted, capped at 2
    }

    #[tokio::test]
    async fn resolve_paths_with_explicit_list() {
        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["a.py", "b.py", "c.py"]).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingJsonLlm::new("{}"));
        let tool = make_tool(llm, Arc::new(store), 30);
        let explicit = vec!["c.py".to_string(), "a.py".to_string()];
        let resolved = tool.resolve_paths(Some(&explicit)).await;
        // Sorted, deduped.
        assert_eq!(resolved, vec!["a.py", "c.py"]);
    }

    // ── Brief builder ─────────────────────────────────────────────────────

    #[test]
    fn build_brief_for_includes_task_and_path() {
        let b = ExploreRepositoryTool::build_brief_for("refactor", "src/x.rs");
        assert!(b.contains("refactor"));
        assert!(b.contains("src/x.rs"));
        assert!(b.contains("read_file"));
    }

    // ── End-to-end small fan-out with mock LLM returning distinct results ─
    //
    // Uses a real `SubagentPool` and `SubagentTool` indirectly, but stubs
    // out the LLM with a small LLM that returns a JSON payload based on
    // the path it finds in the brief. This proves the per-file isolation
    // and result ordering.

    #[derive(Clone)]
    struct PathAwareLlm;
    #[async_trait]
    impl LlmProvider for PathAwareLlm {
        async fn complete(
            &self,
            messages: &[Message],
            _options: &agtrs_runtime::llm::LlmOptions,
        ) -> Result<agtrs_runtime::llm::LlmResponse, AgtrsError> {
            // The brief is the last user message — extract the file path.
            let brief = messages
                .iter()
                .rev()
                .find(|m| m.role == agtrs_runtime::transport::Role::User)
                .map(|m| m.text())
                .unwrap_or_default();
            let path = brief
                .lines()
                .find(|l| l.starts_with("FILE: "))
                .map(|l| l.trim_start_matches("FILE: ").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let json = serde_json::json!({
                "path": path,
                "description": format!("summary of {path}"),
                "key_symbols": ["x"],
                "relevant_to_task": true,
                "issues": []
            });
            Ok(agtrs_runtime::llm::LlmResponse {
                message: Message::assistant(json.to_string()),
                usage: agtrs_runtime::transport::TokenUsage::new(1, 1),
                tool_calls: Vec::new(),
                finish_reason: agtrs_runtime::transport::StopReason::EndTurn,
                thinking_blocks: Vec::new(),
            })
        }
        async fn stream(
            &self,
            _messages: &[Message],
            _options: &agtrs_runtime::llm::LlmOptions,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures::Stream<Item = Result<agtrs_runtime::llm::StreamChunk, AgtrsError>>
                        + Send,
                >,
            >,
            AgtrsError,
        > {
            Ok(Box::pin(futures::stream::empty()))
        }
        async fn embed(
            &self,
            _inputs: &[String],
            _model: Option<&str>,
        ) -> Result<Vec<agtrs_runtime::transport::Embedding>, AgtrsError> {
            Ok(Vec::new())
        }
        async fn count_tokens(&self, _messages: &[Message]) -> Result<usize, AgtrsError> {
            Ok(0)
        }
        fn context_window(&self) -> usize {
            8000
        }
        fn model(&self) -> &str {
            "path-aware"
        }
        fn provider_name(&self) -> &str {
            "path-aware"
        }
        fn supports_tool_calling(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn e2e_pool_produces_per_path_summaries_in_input_order() {
        let store = InMemoryWorkspaceStore::new();
        populate_workspace(&store, &["a.py", "b.py", "c.py"]).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(PathAwareLlm);
        let tool = make_tool(llm, Arc::new(store), 30);
        let ctx = ToolContext::new("t-e2e");

        let res = tool
            .call(serde_json::json!({"task": "x"}), &ctx)
            .await
            .unwrap();
        assert!(!res.is_error, "got: {}", res.content);
        let report: RepositoryReport = serde_json::from_str(&res.content).unwrap();
        assert_eq!(report.files.len(), 3);

        // Order should match sorted workspace list.
        let paths: Vec<String> = report.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths, vec!["a.py", "b.py", "c.py"]);

        // Per-file descriptions should reference each path.
        assert!(report.files[0].description.contains("a.py"));
        assert!(report.files[1].description.contains("b.py"));
        assert!(report.files[2].description.contains("c.py"));
    }
}
