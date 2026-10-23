//! `parallel_workers` — parallel_code_plan's coordinator tools
//! (agenthicc's `workflows/parallel_code_plan/phase_tools.py`).
//!
//! The group drives one orchestration through the shared store in
//! [`xaft_tools::worktrees`] — the manifests `xaft worktrees list|show|
//! integrate|recover` read — and offers the tools of the orchestration's
//! current **stage**, exactly the tool set agenthicc gives each phase:
//!
//! | stage | tools |
//! |---|---|
//! | decompose | `add_parallel_task(task_id, description, dependencies)`, `finalize_task_graph(summary)` |
//! | dispatch | `dispatch_parallel_workers(summary)` |
//! | collect | `collect_parallel_workers(summary)` |
//! | integrate | `integrate_parallel_worker(task_id, summary)` |
//! | verify | `mark_parallel_complete(summary)` |
//!
//! Each tool but `add_parallel_task` is a group transition: it ends the
//! phase when it succeeds (`integrate_parallel_worker` once every task is
//! integrated) and advances the stage, which is checkpointed with the run.
//!
//! # Workers
//!
//! `dispatch_parallel_workers` allocates one Git branch and worktree per
//! ready task and runs one sub-agent in it — a `NamedAgent` over a coder tool
//! registry rooted at the worktree, under the run's approval gate — at most
//! `[execution] max_parallel_tasks` at a time, in dependency waves until no
//! task is ready. A worker gets agenthicc's `[PARALLEL WORKER CONTRACT]` and,
//! while its worktree is uncommitted, up to two follow-up turns. Completion
//! is decided from Git alone: commits on the worker branch and a clean tree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agtrs_runtime::agent::Agent;
use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::team::{HandoffAgentStore, HandoffOrchestrator, HandoffRunParams};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use futures::StreamExt;
use serde_json::{Value, json};
use tracing::{info, warn};

use xaft_agents::named::NamedAgent;
use xaft_tools::ToolRegistryBuilder;
use xaft_tools::worktrees::{
    ManifestStore, OrchestrationStatus, ParallelCoordinator, ParallelManifest, TaskStatus,
    WorktreeManager, WorktreeRecord,
};

use super::LlmForModel;
use super::groups::{GroupTools, RunData};
use super::prompt::role_prompt;
use super::tools::{Edge, Outcome, PhaseDecision, STOP_NOTE, TransitionEnv};

/// RunData key holding `{orchestration_id, stage}`.
const KEY: &str = "parallel";

/// LLM sub-turns per worker turn when `worker_max_turns` is unset.
const DEFAULT_WORKER_MAX_TURNS: u64 = 40;

/// Follow-up turns a worker gets while its worktree is uncommitted.
const WORKER_FOLLOW_UPS: usize = 2;

/// Everything the group needs from the engine.
#[derive(Clone)]
pub struct ParallelContext {
    /// The coordinator repository (the run's working directory).
    pub working_dir: PathBuf,
    /// The owning session — the orchestration's parent session.
    pub session_id: String,
    /// The workflow run id, recorded on the manifest.
    pub run_id: String,
    /// The run's conversation id; workers get derived ids.
    pub conversation_id: String,
    /// The run's default provider (workers use it unless `worker_model`).
    pub llm: Arc<dyn LlmProvider>,
    /// Provider factory for `worker_model`.
    pub llm_for_model: Option<LlmForModel>,
    /// The run's signal bus.
    pub signals: Arc<SignalBus>,
    /// DI context for tools.
    pub resolve_ctx: Arc<injectable_rs_runtime::ResolveContext>,
    /// Where worker conversations are kept.
    pub conv_store: Arc<dyn ConversationStore>,
    /// The run's approval gate; workers' tool calls go through it.
    pub approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// At most this many workers run at once (`[execution] max_parallel_tasks`).
    pub max_parallel: usize,
    /// AGENTS.md and skills context, given to every worker.
    pub project_context: Vec<String>,
}

/// Where an orchestration is in agenthicc's phase order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Decompose,
    Dispatch,
    Collect,
    Integrate,
    Verify,
    Done,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Decompose => "decompose",
            Self::Dispatch => "dispatch",
            Self::Collect => "collect",
            Self::Integrate => "integrate",
            Self::Verify => "verify",
            Self::Done => "done",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "dispatch" => Self::Dispatch,
            "collect" => Self::Collect,
            "integrate" => Self::Integrate,
            "verify" => Self::Verify,
            "done" => Self::Done,
            _ => Self::Decompose,
        }
    }
}

fn stage(data: &RunData) -> Stage {
    data.get(KEY)
        .and_then(|v| v.get("stage").and_then(Value::as_str).map(Stage::parse))
        .unwrap_or(Stage::Decompose)
}

fn orchestration_id(data: &RunData) -> Option<String> {
    data.get(KEY).and_then(|v| {
        v.get("orchestration_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn set_parallel(data: &RunData, orchestration: Option<&str>, stage: Stage) {
    let mut record = match data.get(KEY) {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    if let Some(id) = orchestration {
        record.insert("orchestration_id".into(), json!(id));
    }
    record.insert("stage".into(), json!(stage.as_str()));
    data.set(KEY, Value::Object(record));
}

/// A param path, relative to the working directory when not absolute.
fn param_path(data: &RunData, key: &str, working_dir: &Path) -> Option<PathBuf> {
    let raw = data.param(key)?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    Some(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
}

/// The coordinator for this run: the shared default store and worktree root
/// unless `orchestration_dir` / `worktree_root` relocate them.
fn open_coordinator(ctx: &ParallelContext, data: &RunData) -> Result<ParallelCoordinator, String> {
    let manager = match param_path(data, "worktree_root", &ctx.working_dir) {
        Some(root) => WorktreeManager::with_root(&ctx.working_dir, &root),
        None => WorktreeManager::new(&ctx.working_dir),
    }
    .map_err(|e| e.to_string())?;
    let store = match param_path(data, "orchestration_dir", &ctx.working_dir) {
        Some(dir) => ManifestStore::new(dir),
        None => ManifestStore::open_default().map_err(|e| e.to_string())?,
    };
    Ok(ParallelCoordinator::new(manager, store, ctx.session_id.clone()).with_run_id(&ctx.run_id))
}

/// Run blocking coordinator work off the async executor.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("coordinator task failed: {e}"))?
}

/// The task graph, one line per task, for prompts (`{state.parallel_tasks}`).
fn render_tasks(manifest: &ParallelManifest) -> String {
    if manifest.tasks.is_empty() {
        return "(no tasks yet)".to_string();
    }
    manifest
        .tasks
        .iter()
        .map(|t| {
            let deps = if t.dependencies.is_empty() {
                String::new()
            } else {
                format!(" (after {})", t.dependencies.join(", "))
            };
            let branch = manifest
                .worktree(&t.worktree_id)
                .map(|w| format!(" — branch {}", w.branch))
                .unwrap_or_default();
            let error = if t.error.is_empty() {
                String::new()
            } else {
                format!(" — {}", t.error)
            };
            format!(
                "- {} [{}] {}{deps}{branch}{error}",
                t.task_id, t.status, t.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mirror the orchestration into the `state` map prompts read.
fn publish_state(data: &RunData, manifest: &ParallelManifest) {
    data.set_state_values([
        ("orchestration_id", manifest.orchestration_id.clone()),
        ("parallel_tasks", render_tasks(manifest)),
    ]);
}

fn failure(message: impl std::fmt::Display, fix: &str) -> String {
    format!("{message} Fix: {fix}")
}

/// The group's tools for the orchestration's current stage.
pub(crate) fn parallel_tools(
    ctx: ParallelContext,
    data: &RunData,
    env: &TransitionEnv,
) -> GroupTools {
    let make = |kind: Kind| {
        Arc::new(ParallelTool {
            kind,
            ctx: ctx.clone(),
            data: data.clone(),
            env: env.clone(),
        }) as Arc<ErasedTool>
    };
    let (tools, waits): (Vec<Kind>, Vec<Kind>) = match stage(data) {
        Stage::Decompose => (vec![Kind::AddTask, Kind::Finalize], vec![Kind::Finalize]),
        Stage::Dispatch => (vec![Kind::Dispatch], vec![Kind::Dispatch]),
        Stage::Collect => (vec![Kind::Collect], vec![Kind::Collect]),
        Stage::Integrate => (vec![Kind::Integrate], vec![Kind::Integrate]),
        Stage::Verify => (vec![Kind::Complete], vec![Kind::Complete]),
        Stage::Done => (Vec::new(), Vec::new()),
    };
    GroupTools {
        tools: tools.into_iter().map(make).collect(),
        waits_for: waits.into_iter().map(|k| k.name().to_string()).collect(),
        transitions: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    AddTask,
    Finalize,
    Dispatch,
    Collect,
    Integrate,
    Complete,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::AddTask => "add_parallel_task",
            Self::Finalize => "finalize_task_graph",
            Self::Dispatch => "dispatch_parallel_workers",
            Self::Collect => "collect_parallel_workers",
            Self::Integrate => "integrate_parallel_worker",
            Self::Complete => "mark_parallel_complete",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::AddTask => {
                "Add one independently implementable task to the durable task graph. Does not \
                 change phase."
            }
            Self::Finalize => "Finish decomposition after every worker task has been added.",
            Self::Dispatch => {
                "Create isolated branches and launch all currently ready workers; returns once \
                 they have finished."
            }
            Self::Collect => "Refresh completion evidence for all dispatched workers.",
            Self::Integrate => "Merge one completed worker branch into the coordinator branch.",
            Self::Complete => "Complete the workflow after every task is integrated and verified.",
        }
    }

    fn schema(self) -> Value {
        let summary = |what: &str| json!({ "type": "string", "description": what });
        match self {
            Self::AddTask => json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "A stable, short task id." },
                    "description": {
                        "type": "string",
                        "description": "The concrete worker task, with its exact scope."
                    },
                    "dependencies": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Ids of already-added tasks this one must wait for."
                    }
                },
                "required": ["task_id", "description", "dependencies"],
                "additionalProperties": false
            }),
            Self::Integrate => json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "A completed task to integrate." },
                    "summary": summary("The integration decision.")
                },
                "required": ["task_id", "summary"],
                "additionalProperties": false
            }),
            other => {
                let what = match other {
                    Kind::Finalize => "A summary of the task graph.",
                    Kind::Dispatch => "What is being dispatched.",
                    Kind::Collect => "The observed worker state.",
                    _ => "Final verification evidence.",
                };
                json!({
                    "type": "object",
                    "properties": { "summary": summary(what) },
                    "required": ["summary"],
                    "additionalProperties": false
                })
            }
        }
    }
}

struct ParallelTool {
    kind: Kind,
    ctx: ParallelContext,
    data: RunData,
    env: TransitionEnv,
}

fn text_arg(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

impl ParallelTool {
    /// The orchestration, created from the clean coordinator on first use.
    async fn ensure_orchestration(&self) -> Result<String, String> {
        if let Some(id) = orchestration_id(&self.data) {
            return Ok(id);
        }
        let coordinator = open_coordinator(&self.ctx, &self.data)?;
        let manifest = blocking(move || coordinator.create(None).map_err(|e| e.to_string()))
            .await
            .map_err(|e| {
                failure(
                    format!("The orchestration could not be created: {e}"),
                    "Run parallel_code_plan in a Git repository whose working tree is clean \
                     (commit or stash your changes first).",
                )
            })?;
        set_parallel(
            &self.data,
            Some(&manifest.orchestration_id),
            stage(&self.data),
        );
        publish_state(&self.data, &manifest);
        info!(orchestration = %manifest.orchestration_id, "parallel_code_plan: orchestration created");
        Ok(manifest.orchestration_id)
    }

    async fn manifest(&self, id: &str) -> Result<ParallelManifest, String> {
        let coordinator = open_coordinator(&self.ctx, &self.data)?;
        let id = id.to_string();
        blocking(move || coordinator.manifest(&id).map_err(|e| e.to_string())).await
    }

    fn decide(&self, text: String, args: Value, next: Stage) -> bool {
        let decided = self.env.decide_group_transition(PhaseDecision {
            tool: self.kind.name().to_string(),
            outcome: Outcome::Approved,
            edge: Edge::Default,
            text,
            args,
        });
        if decided {
            set_parallel(&self.data, None, next);
        }
        decided
    }

    async fn add_task(&self, input: &Value) -> Result<String, String> {
        let task_id = text_arg(input, "task_id");
        let description = text_arg(input, "description");
        if task_id.is_empty() {
            return Err(failure(
                "task_id must be non-empty",
                "Provide a stable short task id.",
            ));
        }
        if description.is_empty() {
            return Err(failure(
                "description must be non-empty",
                "Describe the concrete worker task.",
            ));
        }
        let dependencies: Vec<String> = match input.get("dependencies") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        let id = self.ensure_orchestration().await?;
        let coordinator = open_coordinator(&self.ctx, &self.data)?;
        let (task, desc) = (task_id.clone(), description.clone());
        let manifest = blocking(move || {
            coordinator
                .add_task(&id, &task, &desc, &dependencies)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| failure(e, "Correct the task graph input and try again."))?;
        publish_state(&self.data, &manifest);
        Ok(json!({
            "ok": true,
            "task_id": task_id,
            "task_count": manifest.tasks.len(),
            "message": "Task added. Add the remaining tasks, then finalize the graph."
        })
        .to_string())
    }

    async fn finalize(&self, summary: &str) -> Result<String, String> {
        let Some(id) = orchestration_id(&self.data) else {
            return Err(failure(
                "task graph is empty",
                "Add at least one worker task first.",
            ));
        };
        let manifest = self.manifest(&id).await?;
        if manifest.tasks.is_empty() {
            return Err(failure(
                "task graph is empty",
                "Add at least one worker task first.",
            ));
        }
        publish_state(&self.data, &manifest);
        if !self.decide(
            summary.to_string(),
            json!({ "summary": summary }),
            Stage::Dispatch,
        ) {
            return Err("This phase already transitioned.".into());
        }
        Ok(format!(
            "Task graph finalized: {} task(s).\n{STOP_NOTE}",
            manifest.tasks.len()
        ))
    }

    async fn dispatch(
        &self,
        summary: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<String, String> {
        let Some(id) = orchestration_id(&self.data) else {
            return Err(failure(
                "no task graph exists",
                "Finalize the task graph first.",
            ));
        };
        // A worker that was in flight when a previous process stopped has no
        // live session: put it back in the queue, keeping its worktree.
        {
            let coordinator = open_coordinator(&self.ctx, &self.data)?;
            let id = id.clone();
            blocking(move || {
                let store = coordinator_store(&coordinator);
                store
                    .update(&id, |m| {
                        for task in &mut m.tasks {
                            if matches!(
                                task.status,
                                TaskStatus::Running | TaskStatus::Dispatching | TaskStatus::Queued
                            ) {
                                task.status = TaskStatus::Pending;
                            }
                        }
                    })
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await?;
        }

        let mut outcomes: Vec<String> = Vec::new();
        loop {
            if cancel.is_cancelled() {
                return Err("dispatch_parallel_workers was cancelled".into());
            }
            let coordinator = open_coordinator(&self.ctx, &self.data)?;
            let oid = id.clone();
            let ready =
                blocking(move || coordinator.ready_tasks(&oid).map_err(|e| e.to_string())).await?;
            if ready.is_empty() {
                break;
            }
            // Allocate every worktree of the wave first, then run its workers.
            let mut wave: Vec<(String, String, WorktreeRecord)> = Vec::new();
            for task in ready {
                let coordinator = open_coordinator(&self.ctx, &self.data)?;
                let (oid, tid) = (id.clone(), task.task_id.clone());
                let (running, manifest) = blocking(move || {
                    let running = coordinator
                        .dispatch(&oid, &tid)
                        .map_err(|e| e.to_string())?;
                    let manifest = coordinator.manifest(&oid).map_err(|e| e.to_string())?;
                    Ok((running, manifest))
                })
                .await
                .map_err(|e| failure(e, "Resolve the repository or dependency issue and retry."))?;
                let record = manifest
                    .worktree(&running.worktree_id)
                    .cloned()
                    .ok_or_else(|| format!("task {} has no worktree record", running.task_id))?;
                publish_state(&self.data, &manifest);
                wave.push((running.task_id, running.description, record));
            }
            let max = self.ctx.max_parallel.max(1);
            let results: Vec<(String, WorkerOutcome)> = futures::stream::iter(wave)
                .map(|(task_id, description, record)| async move {
                    let outcome =
                        run_worker(&self.ctx, &self.data, &task_id, &description, &record).await;
                    (task_id, outcome)
                })
                .buffer_unordered(max)
                .collect()
                .await;
            for (task_id, outcome) in results {
                let coordinator = open_coordinator(&self.ctx, &self.data)?;
                let (oid, tid) = (id.clone(), task_id.clone());
                let error = outcome.error.clone();
                let status =
                    blocking(move || record_worker(&coordinator, &oid, &tid, error)).await?;
                outcomes.push(format!("- {task_id}: {status}"));
            }
        }
        let manifest = self.manifest(&id).await?;
        publish_state(&self.data, &manifest);
        let report = if outcomes.is_empty() {
            "No task was ready to dispatch.".to_string()
        } else {
            format!("Worker outcomes:\n{}", outcomes.join("\n"))
        };
        if !self.decide(
            format!("{summary}\n\n{report}"),
            json!({ "summary": summary }),
            Stage::Collect,
        ) {
            return Err("This phase already transitioned.".into());
        }
        Ok(format!(
            "Workers dispatched and finished. {report}\n{STOP_NOTE}"
        ))
    }

    async fn collect(&self, summary: &str) -> Result<String, String> {
        let Some(id) = orchestration_id(&self.data) else {
            return Err(failure(
                "no task graph exists",
                "Finalize the task graph first.",
            ));
        };
        let coordinator = open_coordinator(&self.ctx, &self.data)?;
        let oid = id.clone();
        let (results, refreshed) = blocking(move || {
            let manifest = coordinator.manifest(&oid).map_err(|e| e.to_string())?;
            let mut results = Vec::new();
            for task in &manifest.tasks {
                // A failed worker is re-inspected too: if its worktree has since
                // been committed cleanly, the evidence says so.
                if matches!(
                    task.status,
                    TaskStatus::Queued
                        | TaskStatus::Running
                        | TaskStatus::Completed
                        | TaskStatus::Conflict
                        | TaskStatus::Failed
                ) && !task.worktree_id.is_empty()
                {
                    let result = coordinator.inspect_task(&oid, &task.task_id).map_err(|e| {
                        failure(
                            e,
                            &format!("Worker {} is not ready; retry later.", task.task_id),
                        )
                    })?;
                    results.push(serde_json::to_value(&result).unwrap_or(Value::Null));
                }
            }
            let refreshed = coordinator.manifest(&oid).map_err(|e| e.to_string())?;
            Ok((results, refreshed))
        })
        .await?;
        publish_state(&self.data, &refreshed);
        let failed: Vec<&str> = refreshed
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Cancelled))
            .map(|t| t.task_id.as_str())
            .collect();
        if !failed.is_empty() {
            return Err(failure(
                format!("Worker tasks need attention: {}", failed.join(", ")),
                "Inspect the failed workers' worktrees (xaft worktrees show) and complete or \
                 revise the work; a cleanly committed worktree is accepted on the next collect.",
            ));
        }
        let pending: Vec<&str> = refreshed
            .tasks
            .iter()
            .filter(|t| !matches!(t.status, TaskStatus::Completed | TaskStatus::Integrated))
            .map(|t| t.task_id.as_str())
            .collect();
        if !pending.is_empty() {
            return Err(json!({
                "ok": false,
                "pending": pending,
                "results": results,
                "message": "Some workers are not complete; wait and call collect_parallel_workers again."
            })
            .to_string());
        }
        if !self.decide(
            summary.to_string(),
            json!({ "summary": summary }),
            Stage::Integrate,
        ) {
            return Err("This phase already transitioned.".into());
        }
        Ok(format!(
            "{}\n{STOP_NOTE}",
            json!({
                "ok": true,
                "results": results,
                "message": "All workers have completion evidence."
            })
        ))
    }

    async fn integrate(&self, task_id: &str, summary: &str) -> Result<String, String> {
        let Some(id) = orchestration_id(&self.data) else {
            return Err(failure(
                "no task graph exists",
                "Finalize the task graph first.",
            ));
        };
        if task_id.is_empty() {
            return Err(failure(
                "task_id must be non-empty",
                "Choose a completed task.",
            ));
        }
        let coordinator = open_coordinator(&self.ctx, &self.data)?;
        let (oid, tid) = (id.clone(), task_id.to_string());
        let (result, manifest) = blocking(move || {
            let result = coordinator
                .integrate_task(&oid, &tid)
                .map_err(|e| e.to_string())?;
            let manifest = coordinator.manifest(&oid).map_err(|e| e.to_string())?;
            Ok((result, manifest))
        })
        .await
        .map_err(|e| failure(e, "Review the worker evidence and retry integration."))?;
        publish_state(&self.data, &manifest);
        if !result.ok {
            return Err(json!({
                "ok": false,
                "conflict": true,
                "conflict_paths": result.conflict_paths,
                "message": result.message,
            })
            .to_string());
        }
        let remaining: Vec<&str> = manifest
            .tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Integrated)
            .map(|t| t.task_id.as_str())
            .collect();
        if remaining.is_empty() {
            if !self.decide(
                summary.to_string(),
                json!({ "task_id": task_id, "summary": summary }),
                Stage::Verify,
            ) {
                return Err("This phase already transitioned.".into());
            }
            return Ok(format!(
                "{}\nEvery task is integrated.\n{STOP_NOTE}",
                json!({ "ok": true, "task_id": task_id, "head_commit": result.head_commit })
            ));
        }
        Ok(json!({
            "ok": true,
            "task_id": task_id,
            "head_commit": result.head_commit,
            "remaining": remaining,
        })
        .to_string())
    }

    async fn complete(&self, summary: &str) -> Result<String, String> {
        let Some(id) = orchestration_id(&self.data) else {
            return Err(failure(
                "no task graph exists",
                "Finalize the task graph first.",
            ));
        };
        let manifest = self.manifest(&id).await?;
        if manifest
            .tasks
            .iter()
            .any(|t| t.status != TaskStatus::Integrated)
        {
            return Err(failure(
                "not every worker is integrated",
                "Integrate every completed task before marking the workflow complete.",
            ));
        }
        let coordinator = open_coordinator(&self.ctx, &self.data)?;
        let (oid, evidence) = (id.clone(), summary.to_string());
        let manifest = blocking(move || {
            coordinator_store(&coordinator)
                .update(&oid, |m| {
                    m.status = OrchestrationStatus::Complete;
                    m.verification.push(evidence);
                })
                .map_err(|e| e.to_string())
        })
        .await?;
        publish_state(&self.data, &manifest);
        if !self.decide(
            format!(
                "Parallel orchestration {} completed. Worker branches were reviewed, integrated, \
                 and verified.\n\n{summary}",
                manifest.orchestration_id
            ),
            json!({ "summary": summary }),
            Stage::Done,
        ) {
            return Err("This phase already transitioned.".into());
        }
        Ok(format!("Parallel implementation completed.\n{STOP_NOTE}"))
    }
}

/// The manifest store behind a coordinator.
fn coordinator_store(coordinator: &ParallelCoordinator) -> ManifestStore {
    coordinator.store().clone()
}

/// Record what a finished worker left: completion evidence from Git, or the
/// failure agenthicc reports for a worker session that ended without it.
fn record_worker(
    coordinator: &ParallelCoordinator,
    orchestration: &str,
    task_id: &str,
    error: Option<String>,
) -> Result<String, String> {
    let evidence = coordinator.inspect_task(orchestration, task_id);
    let complete = matches!(&evidence, Ok(result) if result.status == "complete");
    if complete {
        return Ok("completed (clean, committed)".into());
    }
    let reason = match (error, evidence) {
        (Some(e), _) => format!("worker failed: {e}"),
        (None, Ok(result)) if result.status == "dirty" => {
            "Worker completed without a clean committed result (uncommitted changes)".to_string()
        }
        (None, Ok(_)) => "Worker completed without a clean committed result".to_string(),
        (None, Err(e)) => format!("worker evidence unavailable: {e}"),
    };
    coordinator_store(coordinator)
        .update(orchestration, |m| {
            if let Some(task) = m.task_mut(task_id) {
                task.status = TaskStatus::Failed;
                task.error = reason.clone();
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(format!("failed — {reason}"))
}

/// agenthicc's `worker_prompt`: the immutable worker contract.
pub(crate) fn worker_prompt(description: &str, worktree_path: &str, base_commit: &str) -> String {
    format!(
        "[PARALLEL WORKER CONTRACT]\n\
         You are an isolated worker. Work only on the assigned task below.\n\
         Assigned worktree: {worktree_path}\n\
         Assigned base commit: {base_commit}\n\
         Do not access, modify, or switch to the coordinator worktree or another \
         worker worktree. Do not change branches. Inspect and edit only the \
         assigned worktree. Implement the task, run relevant validation, and \
         commit the completed changes on the assigned branch. At completion, \
         report the commit(s), tests, remaining risks, and a concise summary.\n\n\
         [ASSIGNED TASK]\n{description}"
    )
}

/// How a worker ended.
struct WorkerOutcome {
    /// Why it could not run to the end, if it could not.
    error: Option<String>,
}

/// The worker's provider: `worker_model` when set, else the run's.
fn worker_llm(ctx: &ParallelContext, data: &RunData) -> Result<Arc<dyn LlmProvider>, String> {
    let model = data
        .param("worker_model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|m| !m.is_empty());
    match (model, &ctx.llm_for_model) {
        (Some(model), Some(factory)) => factory(model).map_err(|e| e.to_string()),
        _ => Ok(Arc::clone(&ctx.llm)),
    }
}

/// Run one worker sub-agent in its worktree until its work is committed or
/// its turns are spent.
async fn run_worker(
    ctx: &ParallelContext,
    data: &RunData,
    task_id: &str,
    description: &str,
    record: &WorktreeRecord,
) -> WorkerOutcome {
    let fail = |e: String| {
        warn!(task = %task_id, error = %e, "parallel worker failed");
        WorkerOutcome { error: Some(e) }
    };
    let worktree = PathBuf::from(&record.path);
    let tools = match ToolRegistryBuilder::new(&worktree)
        .with_shell()
        .build_coder()
    {
        Ok(registry) => registry.all(),
        Err(e) => return fail(format!("cannot build the worker's tools: {e}")),
    };
    let llm = match worker_llm(ctx, data) {
        Ok(llm) => llm,
        Err(e) => return fail(e),
    };
    let max_turns = data.param_positive("worker_max_turns", DEFAULT_WORKER_MAX_TURNS) as usize;
    let mut system = vec![
        role_prompt("executor").to_string(),
        format!(
            "[WORKFLOW]\nYou are parallel worker '{task_id}' of a parallel_code_plan run. \
             Working directory: {} (your assigned Git worktree, on branch {}).",
            record.path, record.branch
        ),
    ];
    system.extend(
        ctx.project_context
            .iter()
            .filter(|b| !b.trim().is_empty())
            .cloned(),
    );
    let agent_key = format!("worker::{task_id}");
    let agent = Arc::new(
        NamedAgent::new(agent_key.as_str(), system.join("\n\n"), max_turns)
            .with_tools(tools)
            .with_signals(Arc::clone(&ctx.signals)),
    );
    let read_before_edit = Arc::new(xaft_tools::ReadBeforeEditHook::new())
        as Arc<dyn agtrs_runtime::tool_hooks::ToolHook>;
    let orchestrator = HandoffOrchestrator::builder()
        .agent(agent_key.as_str(), agent as Arc<dyn Agent>)
        .conv_store(Arc::clone(&ctx.conv_store))
        .agent_store(Arc::new(HandoffAgentStore::new()))
        .max_handoffs(1)
        .llm(llm)
        .resolve_ctx(Arc::clone(&ctx.resolve_ctx))
        .with_approval_gate_opt(ctx.approval_gate.clone())
        .with_global_tool_hook(read_before_edit)
        .build();
    let conversation_id = format!("{}::worker::{task_id}", ctx.conversation_id);
    let manager = match WorktreeManager::with_root(
        Path::new(&record.repository),
        worktree.parent().unwrap_or(&worktree),
    ) {
        Ok(manager) => manager,
        Err(e) => return fail(e.to_string()),
    };
    let mut message = worker_prompt(description, &record.path, &record.base_commit);
    for attempt in 0..=WORKER_FOLLOW_UPS {
        let mut context_state = std::collections::HashMap::new();
        context_state.insert("conversation_id".to_string(), json!(conversation_id));
        let params = HandoffRunParams {
            message: message.clone(),
            user_parts: None,
            conversation_id: conversation_id.clone(),
            initial_agent: agent_key.clone(),
            context_state,
            signals: Some(Arc::clone(&ctx.signals)),
            max_handoffs_override: None,
            dynamic_registry: None,
        };
        match orchestrator.run(params).await {
            Ok(_) | Err(AgtrsError::MaxTurnsExceeded) => {}
            Err(e) => return fail(format!("{e}")),
        }
        let evidence = manager.complete(record, "");
        let state = match &evidence {
            Ok(result) if result.status == "complete" => return WorkerOutcome { error: None },
            Ok(result) if result.status == "dirty" => "has uncommitted changes",
            Ok(_) => "has no commit on the assigned branch yet",
            Err(e) => return fail(format!("cannot inspect the worktree: {e}")),
        };
        info!(task = %task_id, attempt, state, "parallel worker not committed yet");
        message = format!(
            "Your assigned worktree {} {state}. Finish the assigned task, run relevant \
             validation, and commit the completed changes on the assigned branch ({}); leave \
             nothing uncommitted. Then report the commit(s).",
            record.path, record.branch
        );
    }
    WorkerOutcome { error: None }
}

#[async_trait::async_trait]
impl Tool for ParallelTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.kind.name()
    }

    fn description(&self) -> &str {
        self.kind.description()
    }

    fn schema(&self) -> Value {
        self.kind.schema()
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if let Some(tool) = self.env.already_decided() {
            return Ok(ToolResult::error(
                format!(
                    "This phase already transitioned via {tool}(). Stop now; the runner takes over."
                ),
                &ctx.tool_use_id,
            ));
        }
        let summary = text_arg(&input, "summary");
        if self.kind != Kind::AddTask && summary.is_empty() {
            let fix = match self.kind {
                Kind::Finalize => "Summarize the task graph.",
                Kind::Dispatch => "Explain what is being dispatched.",
                Kind::Collect => "Summarize the observed worker state.",
                Kind::Integrate => "Explain the integration decision.",
                _ => "Provide final verification evidence.",
            };
            return Ok(ToolResult::error(
                failure("summary must be non-empty", fix),
                &ctx.tool_use_id,
            ));
        }
        let outcome = match self.kind {
            Kind::AddTask => self.add_task(&input).await,
            Kind::Finalize => self.finalize(&summary).await,
            Kind::Dispatch => self.dispatch(&summary, &ctx.cancel_token).await,
            Kind::Collect => self.collect(&summary).await,
            Kind::Integrate => self.integrate(&text_arg(&input, "task_id"), &summary).await,
            Kind::Complete => self.complete(&summary).await,
        };
        Ok(match outcome {
            Ok(text) => ToolResult::ok(text, &ctx.tool_use_id),
            Err(text) => ToolResult::error(text, &ctx.tool_use_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn task_statuses(manifest: &ParallelManifest) -> BTreeMap<String, TaskStatus> {
        manifest
            .tasks
            .iter()
            .map(|t| (t.task_id.clone(), t.status))
            .collect()
    }
    use crate::phased::tools::PhaseControl;
    use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
    use std::process::Command;
    use std::sync::Mutex;

    pub(super) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "xaft-test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "xaft-test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("git must be installed for parallel tests");
        assert!(
            out.status.success(),
            "git {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A clean repository on `main`, plus private store and worktree roots.
    pub(super) struct Fixture {
        pub _dir: tempfile::TempDir,
        pub repo: PathBuf,
        pub data: RunData,
        pub ctx: ParallelContext,
        pub transport: Arc<MockTransport>,
    }

    pub(super) fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "xaft-test"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let repo = repo.canonicalize().unwrap();
        let mut params = BTreeMap::new();
        params.insert(
            "orchestration_dir".to_string(),
            toml::Value::String(dir.path().join("orchestrations").display().to_string()),
        );
        params.insert(
            "worktree_root".to_string(),
            toml::Value::String(dir.path().join("worktrees").display().to_string()),
        );
        let data = RunData::new(BTreeMap::new(), &repo).with_run("run-1", &params);
        let transport = Arc::new(MockTransport::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(Arc::clone(&transport)));
        let ctx = ParallelContext {
            working_dir: repo.clone(),
            session_id: "sess".into(),
            run_id: "run-1".into(),
            conversation_id: "sess::workflow::run-1".into(),
            llm,
            llm_for_model: None,
            signals: Arc::new(SignalBus::new()),
            resolve_ctx: Arc::new(injectable_rs_runtime::ResolveContext::from_store(Arc::new(
                injectable_rs_runtime::EmptySingletonStore,
            ))),
            conv_store: Arc::new(agtrs_runtime::memory::InMemoryConversationStore::new()),
            approval_gate: None,
            max_parallel: 1,
            project_context: Vec::new(),
        };
        Fixture {
            _dir: dir,
            repo,
            data,
            ctx,
            transport,
        }
    }

    fn env(data: &RunData) -> TransitionEnv {
        TransitionEnv {
            control: Arc::new(Mutex::new(PhaseControl::default())),
            data: data.clone(),
            approval_gate: None,
            signals: None,
        }
    }

    async fn call(f: &Fixture, env: &TransitionEnv, tool: &str, input: Value) -> ToolResult {
        let group = parallel_tools(f.ctx.clone(), &f.data, env);
        let t = group
            .tools
            .iter()
            .find(|t| t.name() == tool)
            .unwrap_or_else(|| panic!("{tool} is not offered at stage {:?}", stage(&f.data)));
        t.call(input, &ToolContext::new("t")).await.unwrap()
    }

    fn names(f: &Fixture) -> Vec<String> {
        let e = env(&f.data);
        parallel_tools(f.ctx.clone(), &f.data, &e)
            .tools
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    fn manifest(f: &Fixture) -> ParallelManifest {
        open_coordinator(&f.ctx, &f.data)
            .unwrap()
            .manifest(&orchestration_id(&f.data).unwrap())
            .unwrap()
    }

    #[tokio::test]
    async fn decompose_builds_a_validated_task_graph_then_finalizes_it() {
        let f = fixture();
        assert_eq!(names(&f), ["add_parallel_task", "finalize_task_graph"]);
        let e = env(&f.data);
        let r = call(&f, &e, "finalize_task_graph", json!({"summary": "s"})).await;
        assert!(
            r.is_error && r.content.contains("task graph is empty"),
            "{}",
            r.content
        );

        let r = call(
            &f,
            &e,
            "add_parallel_task",
            json!({"task_id": "api", "description": "add the endpoint", "dependencies": []}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let r = call(
            &f,
            &e,
            "add_parallel_task",
            json!({"task_id": "docs", "description": "document it", "dependencies": ["ghost"]}),
        )
        .await;
        assert!(
            r.content.contains("unknown task dependencies: ghost"),
            "{}",
            r.content
        );
        let r = call(
            &f,
            &e,
            "add_parallel_task",
            json!({"task_id": "docs", "description": "document it", "dependencies": ["api"]}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            e.already_decided().is_none(),
            "adding a task is not a transition"
        );

        let r = call(
            &f,
            &e,
            "finalize_task_graph",
            json!({"summary": "two tasks"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(e.already_decided().as_deref(), Some("finalize_task_graph"));
        assert_eq!(stage(&f.data), Stage::Dispatch);
        assert_eq!(names(&f), ["dispatch_parallel_workers"]);
        let m = manifest(&f);
        assert_eq!(m.run_id, "run-1");
        assert_eq!(m.parent_session_id, "sess");
        assert!(
            f.data
                .state_value("parallel_tasks")
                .contains("docs [pending] document it (after api)")
        );
    }

    #[tokio::test]
    async fn a_dirty_coordinator_cannot_start_an_orchestration() {
        let f = fixture();
        std::fs::write(f.repo.join("scratch.txt"), "uncommitted").unwrap();
        let e = env(&f.data);
        let r = call(
            &f,
            &e,
            "add_parallel_task",
            json!({"task_id": "a", "description": "x", "dependencies": []}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.content.contains("working tree is clean"), "{}", r.content);
    }

    /// The worker commits its file through the real bash tool in its worktree.
    async fn queue_committing_worker(f: &Fixture, file: &str) {
        f.transport
            .queue_tool_call(
                "bash_exec",
                json!({"command": format!(
                    "printf 'done\\n' > {file} && git add {file} && git -c user.name=w -c user.email=w@example.invalid commit -qm 'work {file}'"
                )}),
            )
            .await;
        f.transport.queue_text("committed").await;
    }

    #[tokio::test]
    async fn workers_run_in_their_worktrees_then_integrate_and_complete() {
        let f = fixture();
        let e = env(&f.data);
        for (task, deps) in [("one", json!([])), ("two", json!(["one"]))] {
            let r = call(
                &f,
                &e,
                "add_parallel_task",
                json!({"task_id": task, "description": format!("write {task}.txt"), "dependencies": deps}),
            )
            .await;
            assert!(!r.is_error, "{}", r.content);
        }
        call(&f, &e, "finalize_task_graph", json!({"summary": "s"})).await;

        // Wave 1 runs "one"; wave 2 then runs "two", which waited on it.
        queue_committing_worker(&f, "one.txt").await;
        queue_committing_worker(&f, "two.txt").await;
        let e = env(&f.data);
        let r = call(
            &f,
            &e,
            "dispatch_parallel_workers",
            json!({"summary": "go"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("- one: completed"), "{}", r.content);
        assert!(r.content.contains("- two: completed"), "{}", r.content);
        let m = manifest(&f);
        assert_eq!(
            task_statuses(&m),
            BTreeMap::from([
                ("one".to_string(), TaskStatus::Completed),
                ("two".to_string(), TaskStatus::Completed)
            ])
        );
        assert!(
            !f.repo.join("one.txt").exists(),
            "work stays on the worker branches"
        );

        let e = env(&f.data);
        let r = call(
            &f,
            &e,
            "collect_parallel_workers",
            json!({"summary": "both done"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(stage(&f.data), Stage::Integrate);

        let e = env(&f.data);
        let r = call(
            &f,
            &e,
            "integrate_parallel_worker",
            json!({"task_id": "one", "summary": "merge"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            e.already_decided().is_none(),
            "one task is still unintegrated"
        );
        let r = call(
            &f,
            &e,
            "integrate_parallel_worker",
            json!({"task_id": "two", "summary": "merge"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            e.already_decided().as_deref(),
            Some("integrate_parallel_worker")
        );
        assert!(f.repo.join("one.txt").is_file() && f.repo.join("two.txt").is_file());

        let e = env(&f.data);
        let r = call(
            &f,
            &e,
            "mark_parallel_complete",
            json!({"summary": "tests pass"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        let m = manifest(&f);
        assert_eq!(m.status, OrchestrationStatus::Complete);
        assert_eq!(m.verification, ["tests pass"]);
        assert_eq!(stage(&f.data), Stage::Done);
        assert!(names(&f).is_empty());
    }

    #[tokio::test]
    async fn a_worker_that_never_commits_fails_and_collect_reports_it() {
        let f = fixture();
        let e = env(&f.data);
        call(
            &f,
            &e,
            "add_parallel_task",
            json!({"task_id": "lazy", "description": "x", "dependencies": []}),
        )
        .await;
        call(&f, &e, "finalize_task_graph", json!({"summary": "s"})).await;
        for _ in 0..=WORKER_FOLLOW_UPS {
            f.transport.queue_text("I thought about it").await;
        }
        let e = env(&f.data);
        let r = call(
            &f,
            &e,
            "dispatch_parallel_workers",
            json!({"summary": "go"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            r.content
                .contains("lazy: failed — Worker completed without a clean committed result"),
            "{}",
            r.content
        );
        let e = env(&f.data);
        let r = call(&f, &e, "collect_parallel_workers", json!({"summary": "s"})).await;
        assert!(r.is_error);
        assert!(
            r.content.contains("Worker tasks need attention: lazy"),
            "{}",
            r.content
        );
        assert!(e.already_decided().is_none());

        // Completing the work by hand in the worker's worktree is accepted.
        let m = manifest(&f);
        let path = PathBuf::from(&m.worktrees[0].path);
        std::fs::write(path.join("fix.txt"), "x").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-q", "-m", "manual"]);
        let r = call(&f, &e, "collect_parallel_workers", json!({"summary": "s"})).await;
        assert!(!r.is_error, "{}", r.content);
    }

    #[test]
    fn the_worker_contract_is_agenthiccs() {
        let p = worker_prompt("write the thing", "/w/t", "abc123");
        assert!(p.starts_with("[PARALLEL WORKER CONTRACT]\nYou are an isolated worker."));
        assert!(p.contains("Assigned worktree: /w/t\nAssigned base commit: abc123\n"));
        assert!(p.ends_with("[ASSIGNED TASK]\nwrite the thing"));
    }
}
