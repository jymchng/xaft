//! goal_flow's goal semantics — agenthicc's `GoalContext` / `GoalRecord`
//! state machine (`workflows/goal_flow/runner.py`) on top of the `goal_list`
//! group.
//!
//! The goal list itself is [`super::groups::Goal`] records in [`RunData`].
//! This module adds what agenthicc's runner does in Python:
//!
//! * the **active goal** cursor (`active_goal_id`): the goal implement/verify
//!   work on. It survives inserts and reorders because it is an id, not an
//!   index; when it points at nothing open, the next schedulable goal is
//!   active — the first pending one, else the first postponed one, promoted;
//! * the transition effects `finalize_goals`, `goal_implemented` and
//!   `goal_verdict`, which validate their arguments exactly as agenthicc's
//!   tools do and move the cursor;
//! * the phase controls `postpone_phase`, `skip_phase`,
//!   `bring_forward_phase` and `delete_phase` (`goal_list` group tools): a
//!   control of another goal changes the plan immediately; a control of the
//!   **active** goal applies at this safe boundary and ends the phase through
//!   its `on_reject` edge (goal_flow re-enters `implement_goal`, which is
//!   skipped for `summarize` once `goals_complete` holds);
//! * the `{goal.<field>}` and `{goals.<view>}` prompt placeholders;
//! * bounded mutation receipts (`goal_mutation_receipts`), agenthicc's audit
//!   trail of plan changes, and its configurable limits (`max_goals`,
//!   `max_goal_text_chars`, `max_phase_control_reason_chars`,
//!   `max_phase_mutation_receipts`).
//!
//! A goal's stable id (agenthicc's `phase_id`) is its number in the list.

use std::collections::BTreeSet;
use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Value, json};

use super::groups::{Goal, GroupTools, RunData, goal_is_open};
use super::tools::{Edge, Outcome, PhaseDecision, STOP_NOTE, TransitionEnv};

/// RunData key: the active goal's id.
pub(crate) const ACTIVE_GOAL_KEY: &str = "active_goal_id";
/// RunData key: the goal list's mutation revision.
const REVISION_KEY: &str = "goal_list_revision";
/// RunData key: the bounded mutation receipts.
const RECEIPTS_KEY: &str = "goal_mutation_receipts";

const DEFAULT_MAX_GOALS: u64 = 1_000;
const MAX_CONFIGURED_GOALS: u64 = 10_000;
const DEFAULT_MAX_GOAL_TEXT_CHARS: u64 = 4_096;
const MAX_CONFIGURED_GOAL_TEXT_CHARS: u64 = 65_536;
const DEFAULT_MAX_REASON_CHARS: u64 = 2_048;
const MAX_CONFIGURED_REASON_CHARS: u64 = 16_384;
const DEFAULT_MAX_RECEIPTS: u64 = 128;
const MAX_CONFIGURED_RECEIPTS: u64 = 1_024;
const MAX_RECORD_TEXT_CHARS: usize = 16_384;
const MAX_RECORD_FILES: usize = 256;
const MAX_RECORD_FILE_PATH_CHARS: usize = 1_024;

/// agenthicc's `GoalFlowParams` limits, read from the workflow params.
struct Limits {
    max_goals: usize,
    max_text: usize,
    max_reason: usize,
    max_receipts: usize,
}

fn limits(data: &RunData) -> Limits {
    Limits {
        max_goals: data
            .param_positive("max_goals", DEFAULT_MAX_GOALS)
            .min(MAX_CONFIGURED_GOALS) as usize,
        max_text: data
            .param_positive("max_goal_text_chars", DEFAULT_MAX_GOAL_TEXT_CHARS)
            .min(MAX_CONFIGURED_GOAL_TEXT_CHARS) as usize,
        max_reason: data
            .param_positive("max_phase_control_reason_chars", DEFAULT_MAX_REASON_CHARS)
            .min(MAX_CONFIGURED_REASON_CHARS) as usize,
        max_receipts: data
            .param_positive("max_phase_mutation_receipts", DEFAULT_MAX_RECEIPTS)
            .min(MAX_CONFIGURED_RECEIPTS) as usize,
    }
}

// ── The active goal ────────────────────────────────────────────────────────

fn active_id(data: &RunData) -> Option<u32> {
    data.get(ACTIVE_GOAL_KEY)
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
}

fn set_active(data: &RunData, id: Option<u32>) {
    data.set(
        ACTIVE_GOAL_KEY,
        id.map(|id| json!(id)).unwrap_or(Value::Null),
    );
}

/// The index of the goal being worked on: the recorded active goal while it
/// is pending, else the first pending goal, else the first postponed one.
pub(crate) fn active_index(goals: &[Goal], data: &RunData) -> Option<usize> {
    if let Some(id) = active_id(data) {
        if let Some(i) = goals
            .iter()
            .position(|g| g.id == id && g.status == "pending")
        {
            return Some(i);
        }
    }
    goals
        .iter()
        .position(|g| g.status == "pending")
        .or_else(|| goals.iter().position(|g| g.status == "postponed"))
}

/// agenthicc's `next_schedulable`: the first pending goal other than
/// `exclude`, else the first postponed one, which is promoted to pending.
fn next_schedulable(goals: &mut [Goal], exclude: Option<u32>) -> Option<u32> {
    if let Some(g) = goals
        .iter()
        .find(|g| Some(g.id) != exclude && g.status == "pending")
    {
        return Some(g.id);
    }
    let g = goals
        .iter_mut()
        .find(|g| Some(g.id) != exclude && g.status == "postponed")?;
    g.status = "pending".into();
    Some(g.id)
}

/// Move the cursor off `finished` onto the next schedulable goal.
fn advance(data: &RunData, goals: &mut [Goal], finished: u32) -> Option<u32> {
    let next = next_schedulable(goals, Some(finished));
    set_active(data, next);
    next
}

/// How a status reads in agenthicc's goal list: `done` is `verified`, the
/// active goal is `active`.
fn display_status(goal: &Goal, active: Option<u32>) -> &str {
    match goal.status.as_str() {
        "done" => "verified",
        "pending" if Some(goal.id) == active => "active",
        other => other,
    }
}

// ── Placeholders ───────────────────────────────────────────────────────────

/// `{goal.<field>}`: a field of the active goal; empty when there is none,
/// `None` (left as typed) for an unknown field.
pub(crate) fn active_goal_field(data: &RunData, field: &str) -> Option<String> {
    let goals = data.goals();
    let active = active_index(&goals, data);
    let goal = active.map(|i| &goals[i]);
    let value = match field {
        "id" => goal.map(|g| g.id.to_string()),
        "text" | "title" => goal.map(|g| g.title.clone()),
        "position" => active.map(|i| (i + 1).to_string()),
        "count" => Some(goals.len().to_string()),
        "attempts" => goal.map(|g| g.attempts.to_string()),
        "summary" => goal.map(|g| g.summary.clone()),
        "evidence" => goal.map(|g| g.evidence.clone()),
        "files" => goal.map(|g| g.files.join(", ")),
        "status" => goal.map(|g| g.status.clone()),
        _ => return None,
    };
    Some(value.unwrap_or_default())
}

/// `{goals.<view>}`: `list` (agenthicc's ordered goal list with phase ids),
/// `dispositions` (the summary's view) or `files` (every recorded file).
pub(crate) fn goals_view(data: &RunData, view: &str) -> Option<String> {
    let goals = data.goals();
    let active = active_index(&goals, data).map(|i| goals[i].id);
    let text = match view {
        "list" => goals
            .iter()
            .enumerate()
            .map(|(i, g)| {
                format!(
                    "{}. [phase_id={}] [{}] {}",
                    i + 1,
                    g.id,
                    display_status(g, active),
                    g.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "dispositions" => goals
            .iter()
            .map(|g| {
                format!(
                    "- {}  [phase_id={}] [disposition={}] [attempts={}]",
                    g.title,
                    g.id,
                    display_status(g, active),
                    g.attempts
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "files" => {
            let mut seen = BTreeSet::new();
            let files: Vec<&str> = goals
                .iter()
                .flat_map(|g| g.files.iter())
                .filter(|f| seen.insert(f.as_str()))
                .map(String::as_str)
                .collect();
            if files.is_empty() {
                "(none recorded)".to_string()
            } else {
                files.join(", ")
            }
        }
        _ => return None,
    };
    Some(if text.is_empty() {
        "(no goals recorded)".to_string()
    } else {
        text
    })
}

// ── Receipts ───────────────────────────────────────────────────────────────

/// One committed mutation, as agenthicc's `GoalMutationReceipt`.
struct Receipt<'a> {
    operation: &'a str,
    goal_id: u32,
    index: usize,
    previous_status: Option<&'a str>,
    new_status: Option<&'a str>,
    reason: &'a str,
}

/// Append a receipt, bumping the revision and keeping the newest
/// `max_phase_mutation_receipts`.
pub(crate) fn record_receipt(
    data: &RunData,
    operation: &str,
    goal_id: u32,
    index: usize,
    previous_status: Option<&str>,
    new_status: Option<&str>,
    reason: &str,
) {
    write_receipt(
        data,
        Receipt {
            operation,
            goal_id,
            index,
            previous_status,
            new_status,
            reason,
        },
    );
}

fn write_receipt(data: &RunData, receipt: Receipt<'_>) {
    let revision = data.get(REVISION_KEY).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
    let mut receipts = match data.get(RECEIPTS_KEY) {
        Some(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    receipts.push(json!({
        "revision": revision,
        "operation": receipt.operation,
        "goal_id": receipt.goal_id,
        "phase_id": receipt.goal_id,
        "index": receipt.index,
        "active_goal_id": active_id(data),
        "previous_status": receipt.previous_status,
        "new_status": receipt.new_status,
        "reason": receipt.reason,
    }));
    let keep = limits(data).max_receipts;
    if receipts.len() > keep {
        let cut = receipts.len() - keep;
        receipts.drain(..cut);
    }
    data.set(REVISION_KEY, json!(revision));
    data.set(RECEIPTS_KEY, Value::Array(receipts));
}

/// agenthicc's checks for a newly discovered goal (`append_goal` /
/// `insert_goal`): non-empty, within `max_goal_text_chars`, and room in a list
/// limited to `max_goals`.
pub(crate) fn check_new_goal(data: &RunData, goals: &[Goal], text: &str) -> Result<(), String> {
    let limits = limits(data);
    if text.trim().is_empty() {
        return Err("The goal was rejected: goal must not be empty.".into());
    }
    if text.trim().chars().count() > limits.max_text {
        return Err(format!(
            "The goal was rejected: goal exceeds {} characters. Shorten the goal while \
             retaining its testable outcome.",
            limits.max_text
        ));
    }
    if goals.len() >= limits.max_goals {
        return Err(format!(
            "The goal was rejected: the list is limited to {} goals. Combine related work \
             into an existing goal or complete pending goals first.",
            limits.max_goals
        ));
    }
    Ok(())
}

// ── Effects ────────────────────────────────────────────────────────────────

/// Run one of goal_flow's transition effects.
pub(crate) fn run_effect(effect: &str, data: &RunData, args: &Value) -> Result<String, String> {
    match effect {
        "finalize_goals" => finalize_goals(data, args),
        "goal_implemented" => goal_implemented(data, args),
        "goal_verdict" => goal_verdict(data, args),
        other => Err(format!("unknown effect '{other}'")),
    }
}

/// agenthicc's `finalize_goals`: strings, or objects with a string
/// `text` / `goal` / `description`; blank items dropped; at least one goal.
fn finalize_goals(data: &RunData, args: &Value) -> Result<String, String> {
    let Some(items) = args.get("goals").and_then(Value::as_array) else {
        return Err("The goals were rejected: goals must be a JSON array. Call \
                    finalize_goals(goals) with one or more concrete goals."
            .into());
    };
    let mut cleaned: Vec<String> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            Value::String(s) => {
                if !s.trim().is_empty() {
                    cleaned.push(s.trim().to_string());
                }
            }
            Value::Object(obj) => {
                let candidates: Vec<&str> = ["text", "goal", "description"]
                    .iter()
                    .filter_map(|k| obj.get(*k).and_then(Value::as_str))
                    .collect();
                if candidates.is_empty() {
                    invalid.push(index.to_string());
                    continue;
                }
                if let Some(text) = candidates.iter().map(|c| c.trim()).find(|c| !c.is_empty()) {
                    cleaned.push(text.to_string());
                }
            }
            _ => invalid.push(index.to_string()),
        }
    }
    if !invalid.is_empty() {
        return Err(format!(
            "The goals were rejected: goal items at index {} must be strings or objects with a \
             string text field. Use finalize_goals(goals) with strings or {{'text': '...'}} objects.",
            invalid.join(", ")
        ));
    }
    if cleaned.is_empty() {
        return Err(
            "The goals were rejected: at least one non-empty goal is required. Call \
                    finalize_goals(goals) with one or more concrete goals."
                .into(),
        );
    }
    let goals: Vec<Goal> = cleaned
        .iter()
        .enumerate()
        .map(|(i, title)| Goal {
            id: i as u32 + 1,
            title: title.clone(),
            status: "pending".into(),
            ..Default::default()
        })
        .collect();
    data.set_goals(&goals);
    set_active(data, Some(1));
    data.set(REVISION_KEY, json!(0));
    data.set(RECEIPTS_KEY, json!([]));
    Ok(format!(
        "{} goals recorded. Implementation starts with goal #1.",
        goals.len()
    ))
}

/// agenthicc's `_clean_files`: a JSON array of at most 256 path strings, each
/// at most 1024 characters; blank entries are dropped.
fn clean_files(value: Option<&Value>) -> Result<Vec<String>, String> {
    let items = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err("files must be a JSON array of paths".into()),
    };
    if items.len() > MAX_RECORD_FILES {
        return Err(format!(
            "files must contain at most {MAX_RECORD_FILES} paths"
        ));
    }
    let mut cleaned = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(path) = item.as_str() else {
            return Err(format!("files[{index}] must be a string path"));
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if path.chars().count() > MAX_RECORD_FILE_PATH_CHARS {
            return Err(format!(
                "files[{index}] exceeds {MAX_RECORD_FILE_PATH_CHARS} characters"
            ));
        }
        cleaned.push(path.to_string());
    }
    Ok(cleaned)
}

fn bounded_text(args: &Value, key: &str, what: &str) -> Result<String, String> {
    let text = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(format!("The {what} was rejected: it must not be empty."));
    }
    if text.chars().count() > MAX_RECORD_TEXT_CHARS {
        return Err(format!(
            "The {what} was rejected: it exceeds {MAX_RECORD_TEXT_CHARS} characters."
        ));
    }
    Ok(text)
}

/// agenthicc's `goal_implemented`: record the summary and files on the
/// active goal and count the attempt.
fn goal_implemented(data: &RunData, args: &Value) -> Result<String, String> {
    let summary = bounded_text(args, "summary", "summary")?;
    let files = clean_files(args.get("files")).map_err(|e| {
        format!(
            "The files were rejected: {e}. Call goal_implemented(summary, files) with a list \
             of path strings."
        )
    })?;
    let mut goals = data.goals();
    let index = active_index(&goals, data).ok_or("the implement phase has no active goal")?;
    let goal = &mut goals[index];
    goal.attempts += 1;
    goal.summary = summary;
    goal.files = files;
    if goal.status == "postponed" {
        goal.status = "pending".into();
    }
    let id = goal.id;
    data.set_goals(&goals);
    set_active(data, Some(id));
    Ok(format!(
        "Implementation of goal #{id} recorded. Verification starts next."
    ))
}

/// agenthicc's `verify_goal` verdict: a satisfied goal is verified and the
/// cursor moves on; an unsatisfied one stays active for another attempt.
fn goal_verdict(data: &RunData, args: &Value) -> Result<String, String> {
    let evidence = bounded_text(args, "evidence", "evidence")?;
    let satisfied = args
        .get("satisfied")
        .and_then(Value::as_bool)
        .ok_or("satisfied must be true or false")?;
    let mut goals = data.goals();
    let index = active_index(&goals, data).ok_or("the verify phase has no active goal")?;
    let id = goals[index].id;
    goals[index].evidence = evidence.clone();
    if !satisfied {
        data.set_goals(&goals);
        set_active(data, Some(id));
        return Ok(format!(
            "Goal #{id} is not satisfied yet; implementation of the same goal continues."
        ));
    }
    goals[index].status = "done".into();
    goals[index].note = evidence;
    let next = advance(data, &mut goals, id);
    data.set_goals(&goals);
    Ok(match next {
        Some(next) => format!("Goal #{id} verified. Goal #{next} is next."),
        None => format!("Goal #{id} verified. Every goal is resolved; the summary starts next."),
    })
}

// ── Phase controls ─────────────────────────────────────────────────────────

/// The four phase-control operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Postpone,
    Skip,
    BringForward,
    Delete,
}

impl Control {
    const ALL: [Control; 4] = [
        Control::Postpone,
        Control::Skip,
        Control::BringForward,
        Control::Delete,
    ];

    fn tool(self) -> &'static str {
        match self {
            Self::Postpone => "postpone_phase",
            Self::Skip => "skip_phase",
            Self::BringForward => "bring_forward_phase",
            Self::Delete => "delete_phase",
        }
    }

    fn operation(self) -> &'static str {
        match self {
            Self::Postpone => "postpone",
            Self::Skip => "skip",
            Self::BringForward => "bring_forward",
            Self::Delete => "delete",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Postpone => {
                "Defer a planned phase (goal) until the other eligible phases have run. On the \
                 active goal it ends this phase at a safe boundary."
            }
            Self::Skip => {
                "Skip a planned phase (goal) without claiming that it was verified. On the \
                 active goal it ends this phase at a safe boundary."
            }
            Self::BringForward => {
                "Move a pending or postponed phase (goal) to the front of eligible work."
            }
            Self::Delete => {
                "Logically delete a planned phase (goal) while retaining its history. On the \
                 active goal it ends this phase at a safe boundary."
            }
        }
    }

    fn reason_hint(self) -> &'static str {
        match self {
            Self::Postpone => "Why this phase should be deferred.",
            Self::Skip => "Why this phase is not required or cannot be run.",
            Self::BringForward => "Why this phase should be handled earlier.",
            Self::Delete => "Why this phase should be removed from future work.",
        }
    }

    /// The status the control leaves a goal in.
    fn status(self) -> &'static str {
        match self {
            Self::Postpone => "postponed",
            Self::Skip => "skipped",
            Self::BringForward => "pending",
            Self::Delete => "deleted",
        }
    }
}

/// The `goal_list` phase controls, bound to a phase run.
///
/// `allow_active` is whether the phase has an `on_reject` edge to leave
/// through when the active goal is controlled.
pub(crate) fn control_tools(data: &RunData, env: &TransitionEnv, allow_active: bool) -> GroupTools {
    let tools: Vec<Arc<ErasedTool>> = Control::ALL
        .iter()
        .map(|c| {
            Arc::new(PhaseControlTool {
                control: *c,
                data: data.clone(),
                env: env.clone(),
                allow_active,
            }) as Arc<ErasedTool>
        })
        .collect();
    let transitions = if allow_active {
        Control::ALL
            .iter()
            .filter(|c| **c != Control::BringForward)
            .map(|c| c.tool().to_string())
            .collect()
    } else {
        Vec::new()
    };
    GroupTools { tools, transitions }
}

struct PhaseControlTool {
    control: Control,
    data: RunData,
    env: TransitionEnv,
    allow_active: bool,
}

/// The result of applying a control.
enum Applied {
    /// The plan changed (or already was so); the phase continues.
    Plan(String),
    /// The active goal was controlled; the phase ends.
    Active(String),
}

impl PhaseControlTool {
    fn reject(&self, message: &str, fix: &str) -> Result<Applied, String> {
        Err(format!("{message} Fix: {fix}"))
    }

    fn apply(&self, phase_id: &str, reason: &str) -> Result<Applied, String> {
        let control = self.control;
        let reason = reason.trim();
        if reason.is_empty() {
            return self.reject(
                "The phase control was rejected: reason must not be empty.",
                "Explain briefly why this phase should be changed.",
            );
        }
        let max_reason = limits(&self.data).max_reason;
        if reason.chars().count() > max_reason {
            return self.reject(
                &format!("The phase control was rejected: reason exceeds {max_reason} characters."),
                "Provide a concise reason for the phase-plan change.",
            );
        }
        let Ok(id) = phase_id.trim().trim_start_matches('#').parse::<u32>() else {
            return self.reject(
                "The phase was rejected: phase_id must be a goal's stable id.",
                "Copy phase_id exactly from the current ordered phase list.",
            );
        };
        let mut goals = self.data.goals();
        let Some(index) = goals.iter().position(|g| g.id == id) else {
            return self.reject(
                "The phase was rejected: phase_id is not in the current goal list.",
                "Use a phase_id from the latest ordered goal list shown in the prompt.",
            );
        };
        let active = active_index(&goals, &self.data).map(|i| goals[i].id);
        let status = goals[index].status.clone();
        let is_active = active == Some(id);

        if control == Control::Postpone && status == "postponed" {
            return Ok(Applied::Plan(
                "Phase is already postponed; no change was needed.".into(),
            ));
        }
        if is_active {
            if control == Control::BringForward {
                return self.reject(
                    "The active phase is already being handled and cannot be brought forward.",
                    "Use bring_forward_phase for a pending or postponed phase.",
                );
            }
            if control == Control::Postpone
                && !goals.iter().any(|g| g.id != id && goal_is_open(&g.status))
            {
                return self.reject(
                    "The active phase cannot be postponed because it is the only unresolved phase.",
                    "Finish it, skip it with a documented reason, or add another goal first.",
                );
            }
            if !self.allow_active {
                return self.reject(
                    "The active phase can only be finished through this phase's transition tool.",
                    "Control another phase, or complete the active one.",
                );
            }
            goals[index].status = control.status().into();
            goals[index].note = reason.to_string();
            let next = advance(&self.data, &mut goals, id);
            self.data.set_goals(&goals);
            record_receipt(
                &self.data,
                control.operation(),
                id,
                index,
                Some(&status),
                Some(control.status()),
                reason,
            );
            let next = match next {
                Some(next) => format!("Goal #{next} is next."),
                None => "No goal is left open; the summary is next.".into(),
            };
            return Ok(Applied::Active(format!(
                "The active phase #{id} is now {}. {next}",
                control.status()
            )));
        }
        if matches!(status.as_str(), "done" | "skipped" | "deleted") {
            if matches!(control, Control::Skip | Control::Delete) {
                let shown = if status == "done" {
                    "verified"
                } else {
                    &status
                };
                return Ok(Applied::Plan(format!(
                    "Phase is already {shown}; no change was needed."
                )));
            }
            let shown = if status == "done" {
                "verified"
            } else {
                &status
            };
            return self.reject(
                &format!("The phase is already terminal ({shown}) and cannot be reordered."),
                "Choose a pending or postponed phase from the current list.",
            );
        }
        let first_eligible = goals
            .iter()
            .position(|g| g.id != id && Some(g.id) != active && g.status == "pending");
        if control == Control::BringForward
            && status == "pending"
            && first_eligible.is_none_or(|first| index < first)
        {
            return Ok(Applied::Plan(
                "Phase is already next among eligible work; no change was needed.".into(),
            ));
        }
        goals[index].status = control.status().into();
        if control != Control::BringForward {
            goals[index].note = reason.to_string();
        }
        let mut position = index;
        if control == Control::BringForward {
            if let Some(first) = first_eligible.filter(|first| *first < index) {
                let goal = goals.remove(index);
                goals.insert(first, goal);
                position = first;
            }
        }
        self.data.set_goals(&goals);
        record_receipt(
            &self.data,
            control.operation(),
            id,
            position,
            Some(&status),
            Some(control.status()),
            reason,
        );
        Ok(Applied::Plan(format!(
            "Phase #{id} is now {}; the plan was checkpointed. Continue the active phase.",
            control.status()
        )))
    }
}

#[async_trait::async_trait]
impl Tool for PhaseControlTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.control.tool()
    }

    fn description(&self) -> &str {
        self.control.description()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "phase_id": {
                    "type": "string",
                    "description": "The stable phase_id (goal id) from the current goal list."
                },
                "reason": { "type": "string", "description": self.control.reason_hint() }
            },
            "required": ["phase_id", "reason"],
            "additionalProperties": false
        })
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
        let field = |k: &str| match input.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let phase_id = field("phase_id");
        let reason = field("reason");
        Ok(match self.apply(&phase_id, &reason) {
            Ok(Applied::Plan(message)) => ToolResult::ok(message, &ctx.tool_use_id),
            Ok(Applied::Active(message)) => {
                let decided = self.env.decide_group_transition(PhaseDecision {
                    tool: self.control.tool().to_string(),
                    outcome: Outcome::Rejected,
                    edge: Edge::Default,
                    text: message.clone(),
                    args: json!({ "phase_id": phase_id, "reason": reason.trim() }),
                });
                if decided {
                    ToolResult::ok(
                        format!(
                            "{message} The control was applied at this safe boundary; start no \
                             new work.\n{STOP_NOTE}"
                        ),
                        &ctx.tool_use_id,
                    )
                } else {
                    ToolResult::error(
                        "This phase already transitioned. Stop now; the runner takes over.",
                        &ctx.tool_use_id,
                    )
                }
            }
            Err(message) => ToolResult::error(message, &ctx.tool_use_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::tools::PhaseControl;
    use std::sync::Mutex;

    fn ctx() -> ToolContext {
        ToolContext::new("t")
    }

    fn env(data: &RunData) -> TransitionEnv {
        TransitionEnv {
            control: Arc::new(Mutex::new(PhaseControl::default())),
            data: data.clone(),
            approval_gate: None,
            signals: None,
        }
    }

    fn statuses(data: &RunData) -> Vec<(u32, String)> {
        data.goals().into_iter().map(|g| (g.id, g.status)).collect()
    }

    fn finalized(goals: &[&str]) -> RunData {
        let data = RunData::default();
        run_effect("finalize_goals", &data, &json!({ "goals": goals })).unwrap();
        data
    }

    async fn control(data: &RunData, env: &TransitionEnv, tool: &str, id: &str) -> ToolResult {
        let tools = control_tools(data, env, true).tools;
        tools
            .iter()
            .find(|t| t.name() == tool)
            .unwrap()
            .call(json!({"phase_id": id, "reason": "because"}), &ctx())
            .await
            .unwrap()
    }

    #[test]
    fn finalize_goals_accepts_strings_and_text_objects_like_agenthicc() {
        let data = RunData::default();
        let note = run_effect(
            "finalize_goals",
            &data,
            &json!({"goals": ["  write code ", {"goal": "write tests"}, "", {"text": " "}]}),
        )
        .unwrap();
        assert_eq!(
            note,
            "2 goals recorded. Implementation starts with goal #1."
        );
        assert_eq!(
            active_goal_field(&data, "text").as_deref(),
            Some("write code")
        );

        let err =
            run_effect("finalize_goals", &data, &json!({"goals": [3, {"x": "y"}]})).unwrap_err();
        assert!(err.contains("goal items at index 0, 1"), "{err}");
        let err = run_effect("finalize_goals", &data, &json!({"goals": ["", " "]})).unwrap_err();
        assert!(err.contains("at least one non-empty goal"), "{err}");
        assert!(run_effect("finalize_goals", &data, &json!({"goals": "x"})).is_err());
    }

    #[test]
    fn an_implement_verify_cycle_moves_the_cursor_only_on_a_satisfied_verdict() {
        let data = finalized(&["one", "two"]);
        run_effect(
            "goal_implemented",
            &data,
            &json!({"summary": "did one", "files": ["a.rs", " ", "b.rs"]}),
        )
        .unwrap();
        assert_eq!(active_goal_field(&data, "attempts").as_deref(), Some("1"));
        assert_eq!(
            active_goal_field(&data, "files").as_deref(),
            Some("a.rs, b.rs")
        );

        let note = run_effect(
            "goal_verdict",
            &data,
            &json!({"satisfied": false, "evidence": "tests fail"}),
        )
        .unwrap();
        assert!(note.contains("not satisfied"), "{note}");
        assert_eq!(active_goal_field(&data, "id").as_deref(), Some("1"));

        run_effect(
            "goal_verdict",
            &data,
            &json!({"satisfied": true, "evidence": "green"}),
        )
        .unwrap();
        assert_eq!(active_goal_field(&data, "id").as_deref(), Some("2"));
        assert_eq!(active_goal_field(&data, "position").as_deref(), Some("2"));
        let note = run_effect(
            "goal_verdict",
            &data,
            &json!({"satisfied": true, "evidence": "green"}),
        )
        .unwrap();
        assert!(note.contains("Every goal is resolved"), "{note}");
        assert_eq!(active_goal_field(&data, "text").as_deref(), Some(""));
        assert!(
            goals_view(&data, "dispositions")
                .unwrap()
                .contains("[disposition=verified]")
        );
    }

    #[test]
    fn implementation_records_are_validated_like_agenthiccs() {
        let data = finalized(&["one"]);
        let long = "x".repeat(MAX_RECORD_TEXT_CHARS + 1);
        assert!(
            run_effect(
                "goal_implemented",
                &data,
                &json!({"summary": long, "files": []})
            )
            .unwrap_err()
            .contains("exceeds")
        );
        assert!(
            run_effect(
                "goal_implemented",
                &data,
                &json!({"summary": "s", "files": [1]})
            )
            .unwrap_err()
            .contains("files[0] must be a string path")
        );
        let many: Vec<String> = (0..300).map(|i| format!("f{i}")).collect();
        assert!(
            run_effect(
                "goal_implemented",
                &data,
                &json!({"summary": "s", "files": many})
            )
            .unwrap_err()
            .contains("at most 256 paths")
        );
    }

    #[tokio::test]
    async fn controlling_another_goal_changes_the_plan_without_ending_the_phase() {
        let data = finalized(&["one", "two", "three", "four"]);
        let e = env(&data);
        let r = control(&data, &e, "postpone_phase", "4").await;
        assert!(!r.is_error, "{}", r.content);
        let r = control(&data, &e, "delete_phase", "#3").await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            e.already_decided().is_none(),
            "the active goal is untouched"
        );
        assert_eq!(
            statuses(&data),
            [
                (1, "pending".into()),
                (2, "pending".into()),
                (3, "deleted".into()),
                (4, "postponed".into())
            ]
        );
        // Bringing the postponed goal forward puts it before #2 — the first
        // eligible goal after the active #1.
        let r = control(&data, &e, "bring_forward_phase", "4").await;
        assert!(!r.is_error, "{}", r.content);
        let order: Vec<u32> = data.goals().iter().map(|g| g.id).collect();
        assert_eq!(order, vec![1, 4, 2, 3]);
        assert_eq!(statuses(&data)[1], (4, "pending".into()));
        // Already next: nothing to do.
        let r = control(&data, &e, "bring_forward_phase", "4").await;
        assert!(r.content.contains("already next"), "{}", r.content);
        // Idempotent and terminal cases.
        assert!(!control(&data, &e, "skip_phase", "3").await.is_error);
        let r = control(&data, &e, "postpone_phase", "3").await;
        assert!(
            r.is_error && r.content.contains("already terminal"),
            "{}",
            r.content
        );
        let receipts = data.get(RECEIPTS_KEY).unwrap();
        assert_eq!(receipts.as_array().unwrap().len(), 3);
        assert_eq!(data.get(REVISION_KEY), Some(json!(3)));
        assert_eq!(active_goal_field(&data, "id").as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn controlling_the_active_goal_ends_the_phase_through_on_reject() {
        let data = finalized(&["one", "two"]);
        let e = env(&data);
        let r = control(&data, &e, "skip_phase", "1").await;
        assert!(!r.is_error, "{}", r.content);
        let decision = e.control.lock().unwrap().decision.clone().unwrap();
        assert_eq!(decision.tool, "skip_phase");
        assert_eq!(decision.outcome, Outcome::Rejected);
        assert_eq!(decision.edge, Edge::Default);
        assert_eq!(active_goal_field(&data, "id").as_deref(), Some("2"));
        // A second control in the same phase run is refused.
        let r = control(&data, &e, "delete_phase", "2").await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn the_only_open_goal_cannot_be_postponed_and_bring_forward_skips_the_active_one() {
        let data = finalized(&["only"]);
        let e = env(&data);
        let r = control(&data, &e, "postpone_phase", "1").await;
        assert!(r.content.contains("only unresolved phase"), "{}", r.content);
        let r = control(&data, &e, "bring_forward_phase", "1").await;
        assert!(r.content.contains("already being handled"), "{}", r.content);
        assert!(e.already_decided().is_none());
    }

    #[tokio::test]
    async fn without_on_reject_the_active_goal_cannot_be_controlled() {
        let data = finalized(&["one", "two"]);
        let e = env(&data);
        let tools = control_tools(&data, &e, false);
        assert!(tools.transitions.is_empty());
        let r = tools.tools[1]
            .call(json!({"phase_id": "1", "reason": "x"}), &ctx())
            .await
            .unwrap();
        assert!(r.is_error, "{}", r.content);
        assert_eq!(statuses(&data)[0].1, "pending");
    }

    #[test]
    fn a_postponed_goal_is_promoted_when_nothing_else_is_pending() {
        let data = finalized(&["one", "two"]);
        let mut goals = data.goals();
        goals[1].status = "postponed".into();
        data.set_goals(&goals);
        run_effect(
            "goal_verdict",
            &data,
            &json!({"satisfied": true, "evidence": "ok"}),
        )
        .unwrap();
        assert_eq!(statuses(&data)[1], (2, "pending".into()));
        assert_eq!(active_goal_field(&data, "id").as_deref(), Some("2"));
    }

    #[test]
    fn limits_follow_the_workflow_params() {
        let mut params = std::collections::BTreeMap::new();
        params.insert("max_goals".to_string(), toml::Value::Integer(2));
        params.insert(
            "max_goal_text_chars".to_string(),
            toml::Value::String("5".into()),
        );
        let data =
            RunData::new(Default::default(), std::path::Path::new("/w")).with_run("r", &params);
        let goals = vec![Goal::default(), Goal::default()];
        assert!(
            check_new_goal(&data, &goals, "x")
                .unwrap_err()
                .contains("limited to 2")
        );
        assert!(
            check_new_goal(&data, &[], "toolong")
                .unwrap_err()
                .contains("5 characters")
        );
        assert!(check_new_goal(&data, &[], "  ").is_err());
        assert!(check_new_goal(&data, &[], "ok").is_ok());
    }
}
