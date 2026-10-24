//! `spawn_subagents` — the session-bound tool every agent turn receives
//! (agenthicc's `subagents/tool.py`, injected by `agent_turn.py`).
//!
//! The tool validates a list of typed task requests, answers from the
//! session journal when the same task set (under the same parent policy)
//! already completed, and otherwise runs a [`SubagentPool`] and returns the
//! labelled aggregate. If a worker asks the parent a question before the pool
//! finishes, the call returns `status: awaiting_clarification` with the open
//! questions and leaves the pool running; the parent answers with
//! `answer_subagent` and collects with `collect_subagent_results`.
//!
//! The result is agenthicc's dict as JSON: `ok`, `pool_id`, `total`,
//! `succeeded`, `failed`, `error`, `results`. A malformed request is a tool
//! error carrying `{"ok": false, "error": …}` with agenthicc's messages.

use std::sync::Arc;
use std::time::Duration;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::SubagentServices;
use super::communication::{AgentMessageBroker, PoolHandle, shared_pool};
use super::policy::SubagentPolicy;
use super::pool::{PoolRun, SubagentPool, SubagentTask};
use super::types::DEFAULT_SUBAGENT_TIMEOUT_S;

/// The tool's name.
pub const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";

const DESCRIPTION: &str = "Spawn multiple specialized subagents concurrently and return their \
aggregated results.\n\n\
Each subagent runs in isolation with its own memory and a filtered tool set. \
Results are returned as a labelled plain-text digest you can reason over.\n\n\
Available agent types: explorer, planner, implementer, executor, tester, \
reviewer, documenter, verifier, researcher.\n\n\
Args:\n    tasks: List of task objects. Each must have:\n           \
- type (str): Agent type name.\n           \
- task (str): Description of what this subagent should do.\n           \
- context (str, optional): Additional background context.\n    \
max_concurrent: Maximum number of subagents running at once (default 4).\n    \
timeout_s: Maximum wall-clock seconds for each worker in this call\n               \
(default 3600, or one hour).";

/// `spawn_subagents`, bound to one parent turn's tools and policy.
pub struct SpawnSubagentsTool {
    services: SubagentServices,
    pool_tools: Vec<Arc<ErasedTool>>,
    policy: SubagentPolicy,
}

impl std::fmt::Debug for SpawnSubagentsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnSubagentsTool")
            .field("pool_tools", &self.pool_tools.len())
            .field("policy", &self.policy.mode_name())
            .finish()
    }
}

impl SpawnSubagentsTool {
    /// Bind the tool to the parent turn's visible `pool_tools` and `policy`.
    pub fn new(
        services: SubagentServices,
        pool_tools: Vec<Arc<ErasedTool>>,
        policy: SubagentPolicy,
    ) -> Self {
        Self {
            services,
            pool_tools,
            policy,
        }
    }
}

/// agenthicc's `_validate_timeout_s`.
fn validate_timeout(value: Option<&Value>) -> Result<f64, String> {
    const MESSAGE: &str = "timeout_s must be a finite number greater than zero";
    let seconds = match value {
        None | Some(Value::Null) => return Ok(DEFAULT_SUBAGENT_TIMEOUT_S),
        Some(Value::Number(n)) => n.as_f64().ok_or(MESSAGE)?,
        Some(Value::String(s)) => s.trim().parse::<f64>().map_err(|_| MESSAGE)?,
        Some(_) => return Err(MESSAGE.into()),
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(MESSAGE.into());
    }
    Ok(seconds)
}

fn python_repr(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn text_of(task: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|k| task.get(*k))
        .find_map(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Null => None,
            Value::String(_) => None,
            other => Some(other.to_string()),
        })
        .unwrap_or_default()
}

impl SpawnSubagentsTool {
    /// Parse and validate a call; agenthicc's messages on failure.
    fn parse(&self, input: &Value) -> Result<(Vec<SubagentTask>, usize, f64), String> {
        let timeout = validate_timeout(input.get("timeout_s"))?;
        let Some(tasks) = input.get("tasks").and_then(Value::as_array) else {
            return Err("tasks must be a list of task objects".into());
        };
        let max_concurrent = match input.get("max_concurrent") {
            None | Some(Value::Null) => self.services.max_concurrent,
            Some(Value::Number(n)) if n.as_u64().is_some() => n.as_u64().unwrap_or(1) as usize,
            Some(Value::Number(n)) if n.as_i64().is_some() => 1,
            Some(_) => return Err("max_concurrent must be an integer".into()),
        };
        let registry = &self.services.registry;
        let mut parsed = Vec::with_capacity(tasks.len());
        for (i, raw) in tasks.iter().enumerate() {
            let Some(task) = raw.as_object() else {
                return Err(format!(
                    "tasks[{i}] must be a dict with 'type' and 'task' keys"
                ));
            };
            let agent_type = text_of(task, &["type", "agent_type"]).trim().to_string();
            let description = text_of(task, &["task", "task_description"]);
            let context = text_of(task, &["context"]);
            if agent_type.is_empty() {
                return Err(format!("tasks[{i}] missing 'type' field"));
            }
            let agent_type = registry.resolve(&agent_type).to_string();
            if description.is_empty() {
                return Err(format!("tasks[{i}] missing 'task' field"));
            }
            if !registry.contains(&agent_type) {
                return Err(format!(
                    "Unknown agent type {}. Known types: {}",
                    python_repr(&agent_type),
                    registry.known_type_names().join(", ")
                ));
            }
            parsed.push(SubagentTask {
                task_id: format!("task-{i}"),
                agent_type,
                task_description: description,
                context,
            });
        }
        if parsed.is_empty() {
            return Err("tasks list is empty".into());
        }
        Ok((parsed, max_concurrent.max(1), timeout))
    }

    /// agenthicc's `_tasks_fingerprint`: order-insensitive over type, task
    /// and context, bound to the parent policy.
    fn fingerprint(&self, tasks: &[SubagentTask]) -> String {
        let mut entries: Vec<(String, String, String)> = tasks
            .iter()
            .map(|t| {
                (
                    t.agent_type.clone(),
                    t.task_description.clone(),
                    t.context.clone(),
                )
            })
            .collect();
        entries.sort();
        let mut list: Vec<Value> = entries
            .into_iter()
            .map(|(agent_type, task, context)| {
                json!({"type": agent_type, "task": task, "context": context})
            })
            .collect();
        list.push(json!({"policy_revision": self.policy.revision()}));
        super::sha256_hex(&xaft_mcp::pyjson::dumps(&Value::Array(list)))[..16].to_string()
    }
}

fn outcome_json(result: &super::AggregatedResult) -> Value {
    json!({
        "ok": result.failed == 0,
        "pool_id": result.pool_id,
        "total": result.total,
        "succeeded": result.succeeded,
        "failed": result.failed,
        "error": if result.failed == 0 {
            String::new()
        } else {
            format!("{} subagent(s) failed", result.failed)
        },
        "results": result.text,
    })
}

#[async_trait]
impl Tool for SpawnSubagentsTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        SPAWN_SUBAGENTS_TOOL_NAME
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": {
                                "type": "string",
                                "enum": self.services.registry.known_type_names(),
                            },
                            "task": {"type": "string"},
                            "context": {"type": ["string", "null"]},
                        },
                        "required": ["type", "task"],
                    },
                },
                "max_concurrent": {"type": "integer", "default": self.services.max_concurrent},
                "timeout_s": {"type": "number", "default": DEFAULT_SUBAGENT_TIMEOUT_S},
            },
            "required": ["tasks"],
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: "spawn_subagents cancelled".into(),
            });
        }
        let (tasks, max_concurrent, timeout) = match self.parse(&input) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Ok(ToolResult::error(
                    json!({"ok": false, "error": error}).to_string(),
                    &ctx.tool_use_id,
                ));
            }
        };
        let total = tasks.len();
        let fingerprint = self.fingerprint(&tasks);
        let journal = self.services.journal.clone();

        // Resume: the same complete task set under the same policy is
        // answered from the journal instead of being run again.
        if let Some(text) = journal
            .as_ref()
            .and_then(|j| j.cached_pool_text(&fingerprint))
        {
            tracing::info!(
                tasks = total,
                "spawn_subagents: using cached subagent results"
            );
            return Ok(ToolResult::ok(
                json!({
                    "ok": true,
                    "pool_id": "cached",
                    "total": total,
                    "succeeded": total,
                    "failed": 0,
                    "error": "",
                    "results": text,
                })
                .to_string(),
                &ctx.tool_use_id,
            ));
        }

        let pool_id = uuid::Uuid::new_v4().simple().to_string();
        let broker = match AgentMessageBroker::new(
            &self.services.session_id,
            &self.services.parent_run_id,
            &pool_id,
            &self.policy.revision(),
            journal.clone(),
        ) {
            Ok(broker) => Some(Arc::new(broker)),
            Err(error) => {
                tracing::debug!(%error, "spawn_subagents: running without communication");
                None
            }
        };
        let pool = SubagentPool {
            run: PoolRun {
                services: self.services.clone(),
                pool_tools: self.pool_tools.clone(),
                policy: self.policy.clone(),
                registry: Arc::clone(&self.services.registry),
                broker: broker.clone(),
                timeout: Duration::from_secs_f64(timeout.min(1e9)),
                cancel: ctx.cancel_token.child_token(),
                pool_id: pool_id.clone(),
                fingerprint: fingerprint.clone(),
            },
            tasks,
            max_concurrent,
        };
        let task = tokio::spawn(pool.run());
        let abort = task.abort_handle();
        let result = shared_pool(task);

        let finished = match &broker {
            Some(broker) => {
                tokio::select! {
                    biased;
                    _ = ctx.cancel_token.cancelled() => {
                        abort.abort();
                        return Err(AgtrsError::Cancelled {
                            reason: "spawn_subagents cancelled".into(),
                        });
                    }
                    finished = result.clone() => finished,
                    pending = broker.wait_for_pending() => match pending {
                        Ok(questions) => {
                            self.services.continuations.register(PoolHandle {
                                pool_id: pool_id.clone(),
                                conversation_id: self.services.session_id.clone(),
                                broker: Arc::clone(broker),
                                result,
                                abort,
                            });
                            return Ok(ToolResult::ok(
                                json!({
                                    "ok": false,
                                    "status": "awaiting_clarification",
                                    "pool_id": pool_id,
                                    "pending_questions": questions,
                                    "total": total,
                                    "succeeded": 0,
                                    "failed": 0,
                                    "results": "A subagent needs clarification. Answer it with \
                                                answer_subagent, then collect_subagent_results.",
                                })
                                .to_string(),
                                &ctx.tool_use_id,
                            ));
                        }
                        Err(_) => result.await,
                    },
                }
            }
            None => {
                tokio::select! {
                    biased;
                    _ = ctx.cancel_token.cancelled() => {
                        abort.abort();
                        return Err(AgtrsError::Cancelled {
                            reason: "spawn_subagents cancelled".into(),
                        });
                    }
                    finished = result => finished,
                }
            }
        };
        let aggregate = match finished {
            Ok(aggregate) => aggregate,
            Err(error) => {
                return Ok(ToolResult::error(
                    json!({"ok": false, "error": error}).to_string(),
                    &ctx.tool_use_id,
                ));
            }
        };
        // Only a completely successful pool is a safe resume entry.
        if aggregate.failed == 0 {
            if let Some(journal) = &journal {
                journal.pool_result(&fingerprint, &aggregate);
            }
        }
        Ok(ToolResult::ok(
            outcome_json(&aggregate).to_string(),
            &ctx.tool_use_id,
        ))
    }

    fn parallel_safe(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_are_validated_like_agenthicc() {
        assert_eq!(validate_timeout(None).unwrap(), DEFAULT_SUBAGENT_TIMEOUT_S);
        assert_eq!(validate_timeout(Some(&json!(30))).unwrap(), 30.0);
        assert_eq!(validate_timeout(Some(&json!("2.5"))).unwrap(), 2.5);
        for bad in [json!(0), json!(-1), json!(true), json!("soon"), json!([1])] {
            assert_eq!(
                validate_timeout(Some(&bad)).unwrap_err(),
                "timeout_s must be a finite number greater than zero",
                "{bad}"
            );
        }
    }

    #[test]
    fn names_are_quoted_like_python_repr() {
        assert_eq!(python_repr("bogus"), "'bogus'");
        assert_eq!(python_repr("it's"), "'it\\'s'");
    }
}
