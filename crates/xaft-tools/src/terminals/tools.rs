//! The four agent-visible background-terminal tools.
//!
//! Parity rows 36 (`inspect_terminal`), 43 (`stop_terminal`), 44
//! (`wait_terminal`) and 45 (`wait_terminal_ready`). The process machinery is
//! [`super::TerminalManager`]; this file is only the agent-facing surface over
//! it, and it is deliberately thin — every security property lives in the
//! manager, not here.
//!
//! # Why each tool may hold no manager
//!
//! agenthicc reads its manager out of the tool *context*
//! (`context.get("terminal_manager")`) and returns
//! `{"ok": false, "state": "rejected", "error": "terminal manager unavailable"}`
//! when it is absent, rather than pretending to have started something. xaft's
//! [`ToolContext`] has no such slot, so the manager is injected at construction
//! instead — and the tools are **registered unconditionally**, holding an
//! `Option<Arc<TerminalManager>>`.
//!
//! That is the same choice already made for `run_python` in
//! [`crate::shell::RunPythonTool`]: a tool that is absent teaches the agent
//! nothing, while a tool that is present and refuses with a named reason gives
//! it something actionable. Registering unconditionally is also what keeps
//! `every_tool_the_matrix_marks_done_is_registered` honest — that guard builds a
//! default registry, so a manager-gated registration would make rows which are
//! genuinely implemented read as missing.
//!
//! # Why `stop_terminal` cannot signal a persisted record
//!
//! See [`super::manager`]'s module docs: a PID read from disk is not proof of
//! ownership. [`TerminalManager::stop`] already returns `false` for an id it did
//! not spawn, and this tool surfaces that as `stop_requested: false` rather than
//! inventing a success. The manager test
//! `an_orphaned_terminal_cannot_be_stopped` is the pin; the tool-level test
//! `an_orphaned_terminal_cannot_be_stopped_through_the_tool` in
//! `tests/terminal_tools.rs` is this file's half of the same property, and it
//! plants a record carrying *this process's own* pid so that a regression
//! signals something that is demonstrably still alive.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use super::manager::TerminalManager;

/// Seconds since the Unix epoch, or 0.0 before it.
fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// agenthicc's rejection when no terminal manager is wired up.
fn no_manager(name: &str, tool_use_id: &str) -> ToolResult {
    ToolResult::error(
        serde_json::to_string_pretty(&serde_json::json!({
            "ok": false,
            "state": "rejected",
            "error": "terminal manager unavailable",
            "tool": name,
        }))
        .expect("a static object always serialises"),
        tool_use_id,
    )
}

/// Render an envelope as the tool's text content, marking `ok: false` as an
/// error so the approval and streaming layers see the same signal agenthicc's
/// dict carries.
fn envelope(result: Value, tool_use_id: &str) -> ToolResult {
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let body = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
    if ok {
        ToolResult::ok(body, tool_use_id)
    } else {
        ToolResult::error(body, tool_use_id)
    }
}

/// `terminal_id` is required and must be a string, in all four tools.
fn terminal_id(input: &Value, name: &str) -> Result<String, AgtrsError> {
    input
        .get("terminal_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AgtrsError::ToolCallFailed {
            tool_name: name.to_string(),
            reason: "required field 'terminal_id' is missing or not a string".into(),
        })
}

/// The shared inner field of all four tools.
///
/// Private, so the only ways to build one are `new` on the concrete type — which
/// is what keeps the registry and the tests going through the same path.
#[derive(Clone)]
struct ManagerHolder {
    manager: Option<Arc<TerminalManager>>,
}

impl ManagerHolder {
    fn new(manager: Option<Arc<TerminalManager>>) -> Self {
        Self { manager }
    }

    /// Resolve the manager, or agenthicc's refusal.
    fn resolve(&self, name: &str, ctx: &ToolContext) -> Result<Arc<TerminalManager>, ToolResult> {
        self.manager
            .clone()
            .ok_or_else(|| no_manager(name, &ctx.tool_use_id))
    }
}

impl std::fmt::Debug for ManagerHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerHolder")
            .field("has_manager", &self.manager.is_some())
            .finish()
    }
}

/// Inspect one owned terminal without waiting or stopping it.
pub struct InspectTerminalTool {
    holder: ManagerHolder,
}

impl InspectTerminalTool {
    /// Build over an optional manager. `None` yields agenthicc's
    /// `terminal manager unavailable` refusal rather than a fake result.
    pub fn new(manager: Option<Arc<TerminalManager>>) -> Self {
        Self {
            holder: ManagerHolder::new(manager),
        }
    }
}

#[async_trait]
impl Tool for InspectTerminalTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "inspect_terminal"
    }

    fn description(&self) -> &str {
        "Inspect the state and bounded output of an owned background terminal. \
         Read-only: it neither waits nor stops. A terminal this session does not own \
         is reported as `rejected` and never inspected."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" }
            },
            "required": ["terminal_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "inspect_terminal", skip(self, ctx, input), fields(terminal_id))]
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let manager = match self.holder.resolve(self.name(), ctx) {
            Ok(m) => m,
            Err(refusal) => return Ok(refusal),
        };
        let id = terminal_id(&input, self.name())?;
        tracing::Span::current().record("terminal_id", id.as_str());

        // The manager's `get` is session-agnostic, so the session check is made
        // here, where agenthicc makes it. A record belonging to another session
        // must not be observable at all.
        let Some(record) = manager.get(&id) else {
            return Ok(envelope(
                serde_json::json!({
                    "ok": false,
                    "state": "unknown",
                    "terminal_id": id,
                }),
                &ctx.tool_use_id,
            ));
        };
        if record.session_id != manager.config().session_id {
            return Ok(envelope(
                serde_json::json!({
                    "ok": false,
                    "state": "rejected",
                    "terminal_id": id,
                    "error": "terminal belongs to another session",
                }),
                &ctx.tool_use_id,
            ));
        }
        Ok(envelope(record.result(epoch_now()), &ctx.tool_use_id))
    }
}

/// Wait for an owned terminal to finish, bounding the WAIT only.
pub struct WaitTerminalTool {
    holder: ManagerHolder,
}

impl WaitTerminalTool {
    /// Build over an optional manager.
    pub fn new(manager: Option<Arc<TerminalManager>>) -> Self {
        Self {
            holder: ManagerHolder::new(manager),
        }
    }
}

#[async_trait]
impl Tool for WaitTerminalTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "wait_terminal"
    }

    fn description(&self) -> &str {
        "Wait for an owned background terminal to finish and return its output. \
         `timeout` bounds the WAIT ONLY — it never kills the process, so a timeout \
         means 'still running', not 'stopped'. Use `stop_terminal` to end it."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "timeout": {
                    "type": "number",
                    "default": 0.0,
                    "description": "Seconds to observe for. 0 returns the current state without waiting."
                }
            },
            "required": ["terminal_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        false
    }

    #[instrument(name = "wait_terminal", skip(self, ctx, input), fields(terminal_id))]
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let manager = match self.holder.resolve(self.name(), ctx) {
            Ok(m) => m,
            Err(refusal) => return Ok(refusal),
        };
        let id = terminal_id(&input, self.name())?;
        tracing::Span::current().record("terminal_id", id.as_str());

        // agenthicc defaults this to 0.0, meaning "report the state now".
        let timeout = input.get("timeout").and_then(Value::as_f64).unwrap_or(0.0);
        Ok(envelope(manager.wait(&id, timeout).await, &ctx.tool_use_id))
    }
}

/// Wait for an owned service's readiness probe, without stopping it.
pub struct WaitTerminalReadyTool {
    holder: ManagerHolder,
}

impl WaitTerminalReadyTool {
    /// Build over an optional manager.
    pub fn new(manager: Option<Arc<TerminalManager>>) -> Self {
        Self {
            holder: ManagerHolder::new(manager),
        }
    }
}

#[async_trait]
impl Tool for WaitTerminalReadyTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "wait_terminal_ready"
    }

    fn description(&self) -> &str {
        "Wait for an owned service terminal's readiness probe without stopping it. \
         TCP probes are refused unless the terminal was started with \
         `allow_external`, so a probe can never be used to reach off-host ports."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "readiness": {
                    "type": "object",
                    "description": "Optional probe overriding the one recorded at start."
                }
            },
            "required": ["terminal_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        false
    }

    #[instrument(
        name = "wait_terminal_ready",
        skip(self, ctx, input),
        fields(terminal_id)
    )]
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let manager = match self.holder.resolve(self.name(), ctx) {
            Ok(m) => m,
            Err(refusal) => return Ok(refusal),
        };
        let id = terminal_id(&input, self.name())?;
        tracing::Span::current().record("terminal_id", id.as_str());

        // A non-object `readiness` is rejected rather than coerced, exactly as
        // agenthicc does: it is a malformed call, not a value to guess at.
        let readiness = match input.get("readiness") {
            None | Some(Value::Null) => None,
            Some(v @ Value::Object(_)) => Some(v.clone()),
            Some(_) => {
                return Ok(envelope(
                    serde_json::json!({
                        "ok": false,
                        "state": "rejected",
                        "error": "readiness must be an object",
                    }),
                    &ctx.tool_use_id,
                ));
            }
        };

        Ok(envelope(
            manager.wait_readiness(&id, readiness.as_ref()).await,
            &ctx.tool_use_id,
        ))
    }
}

/// Stop one owned terminal, signalling its whole process group.
pub struct StopTerminalTool {
    holder: ManagerHolder,
}

impl StopTerminalTool {
    /// Build over an optional manager.
    pub fn new(manager: Option<Arc<TerminalManager>>) -> Self {
        Self {
            holder: ManagerHolder::new(manager),
        }
    }
}

#[async_trait]
impl Tool for StopTerminalTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "stop_terminal"
    }

    fn description(&self) -> &str {
        "Stop one owned background terminal gracefully, escalating to SIGKILL only \
         if it ignores the graceful signal. The whole PROCESS GROUP is signalled, so \
         backgrounded grandchildren die too. Only a terminal this manager spawned is \
         eligible: a record merely read back from disk is reported as not stopped \
         rather than signalled, because its PID may since have been recycled."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminal_id": { "type": "string" },
                "force": {
                    "type": "boolean",
                    "default": false,
                    "description": "Skip the grace period and escalate immediately."
                }
            },
            "required": ["terminal_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    fn parallel_safe(&self) -> bool {
        false
    }

    #[instrument(name = "stop_terminal", skip(self, ctx, input), fields(terminal_id))]
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let manager = match self.holder.resolve(self.name(), ctx) {
            Ok(m) => m,
            Err(refusal) => return Ok(refusal),
        };
        let id = terminal_id(&input, self.name())?;
        tracing::Span::current().record("terminal_id", id.as_str());

        let force = input.get("force").and_then(Value::as_bool).unwrap_or(false);
        // `stop` returns false for unknown ids, other sessions' ids, and records
        // with no live child. All three mean "no proof of ownership", and none of
        // them results in a signal.
        let stopped = manager.stop(&id, force, "tool stop").await;

        let Some(record) = manager.get(&id) else {
            return Ok(envelope(
                serde_json::json!({
                    "ok": false,
                    "state": "unknown",
                    "terminal_id": id,
                    "stop_requested": stopped,
                }),
                &ctx.tool_use_id,
            ));
        };
        let mut result = record.result(epoch_now());
        if let Some(obj) = result.as_object_mut() {
            obj.insert("stop_requested".into(), Value::Bool(stopped));
        }
        // A terminal stopped after the agent asked is not the success it asked
        // for, so `ok` follows the record while `stop_requested` carries the
        // separate question of whether a signal was warranted.
        Ok(envelope(result, &ctx.tool_use_id))
    }
}
