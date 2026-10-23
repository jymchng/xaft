//! Prompt assembly for phased workflows.
//!
//! A phase's system prompt is built the way agenthicc's generic
//! `WorkflowRunner._run_phase` builds it: the phase's own prompt (or its
//! role's default) followed by framework-owned blocks the author cannot
//! remove — the requirements-clarification reminder and the list of transition
//! tools the phase may call. The phase's *task message* is separate: by
//! default the workflow context block plus the intent, or the phase's `prompt`
//! template with its placeholders filled in.

use std::sync::OnceLock;

use regex::Regex;
use xaft_tools::capabilities::{READ_CAPS, ToolCapability};

use super::groups::{RunData, artifacts_block, goals_block, next_goal};
use super::state::WorkflowRunState;

/// agenthicc's `_WORKFLOW_USER_QUESTION_REMINDER`, verbatim.
pub const CLARIFICATION_REMINDER: &str = "[REQUIREMENTS CLARIFICATION]\n\
Do not guess when an unanswered requirement could materially change the work. \
You can ask the user multiple focused questions, including several questions \
in one call, by using the `ask_user` tool. Ask about goals, constraints, \
priorities, choices, and acceptance criteria whenever they are unclear; use \
the available exploration tools first when the answer can be discovered from \
the project. Incorporate the user's answers into this workflow before making \
the next phase decision.";

/// Goal-list tools that record work without ending the phase
/// (agenthicc's `_NON_TRANSITION_CONTROL_TOOL_NAMES`).
pub const NON_TRANSITION_MUTATIONS: &[&str] = &["append_goal", "insert_goal"];

/// The default tool capabilities of a role — agenthicc's `ROLE_DEFAULT_ALLOWED`.
///
/// `None` means "every capability the phase's mode allows".
pub fn role_default_capabilities(role: &str) -> Option<Vec<ToolCapability>> {
    match role {
        "planner" | "reviewer" | "explorer" | "verifier" | "qa" => Some(READ_CAPS.to_vec()),
        "human" | "script" => Some(Vec::new()),
        _ => None,
    }
}

/// One-line description of a role.
pub fn role_summary(role: &str) -> &'static str {
    match role {
        "auto" => "general-purpose agent; the default",
        "planner" => "explores read-only and produces a plan",
        "executor" | "coder" => "implements changes with the full tool set",
        "reviewer" | "qa" => "inspects changes and runs tests, read-only",
        "explorer" => "maps the relevant code, read-only",
        "verifier" => "checks acceptance criteria against the workspace, read-only",
        "fixer" => "fixes reported issues",
        "custom" => "no role prompt; the phase's system_prompt is everything",
        "human" => "no agent: the user approves or rejects the work so far",
        "script" => "no agent: runs `command` and lets its exit code or JSON output pick the edge",
        _ => "custom role",
    }
}

/// The default role prompt, used when a phase declares no `system_prompt`.
pub fn role_prompt(role: &str) -> &'static str {
    match role {
        "planner" => {
            "You are the planner. Explore the codebase with the read-only tools and produce a \
             concrete, ordered implementation plan. Do not modify files."
        }
        "executor" | "coder" => {
            "You are the executor. Implement the requested changes with the available tools, \
             keeping edits minimal and verifying them (build, tests) as you go."
        }
        "reviewer" | "qa" => {
            "You are the reviewer. Inspect the changes critically, run the tests, and report \
             concrete defects with file:line references."
        }
        "explorer" => {
            "You are the explorer. Map the code relevant to the task with the read-only tools \
             and report the facts the next phase needs, citing paths."
        }
        "verifier" => {
            "You are the verifier. Check the stated acceptance criteria against the actual \
             workspace state and report pass or fail with evidence."
        }
        "fixer" => "You are the fixer. Fix exactly the reported issues, then verify the fix.",
        _ => {
            "You are xaft, an autonomous software-engineering agent. Use the available tools \
             to complete this phase's task precisely and verify your work before finishing."
        }
    }
}

/// What a phase's mode allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModePolicy {
    /// agenthicc's Plan: the restricted capabilities (write, git-write,
    /// execute, network, undeclared) are hard-blocked; read, search, git-read,
    /// memory and control tools remain.
    pub read_only: bool,
    /// The restricted tools need the user's approval before each call —
    /// agenthicc's Safe (`approval_required` for write, git-write, execute,
    /// network and undeclared tools).
    pub approval_gated: bool,
    /// Text placed before the phase prompt.
    pub patch: &'static str,
}

/// The policy for a phase `mode_override` (canonical names and aliases).
pub fn mode_policy(mode: Option<&str>) -> ModePolicy {
    match mode.map(|m| m.trim().to_ascii_lowercase()).as_deref() {
        Some("plan") | Some("review") => ModePolicy {
            read_only: true,
            approval_gated: false,
            patch: "[MODE: PLAN]\nThis phase is read-only: you MUST NOT write, edit, create or \
                    delete files, execute commands, modify git state or reach the network. \
                    Only reading, listing and searching are available.",
        },
        // agenthicc's Safe (and its `guard` / `ask` aliases): gate-and-ask,
        // the same semantics the TUI's Safe mode enforces.
        Some("safe") | Some("guard") | Some("ask") => ModePolicy {
            read_only: false,
            approval_gated: true,
            patch: "## SAFE MODE\nSide-effecting tools require explicit user approval before \
                    execution. Read, search, and git-read tools may be used directly.",
        },
        _ => ModePolicy {
            read_only: false,
            approval_gated: false,
            patch: "",
        },
    }
}

/// agenthicc's `phase_transition_instruction`.
///
/// `transitions` are the tools that end the phase; `mutations` the goal-list
/// tools that record work without ending it.
pub fn transition_instruction(phase: &str, transitions: &[String], mutations: &[String]) -> String {
    let label = if phase.is_empty() {
        String::new()
    } else {
        format!(" in the '{phase}' phase")
    };
    if transitions.is_empty() && mutations.is_empty() {
        return format!(
            "[PHASE TRANSITION TOOLS]\nNo phase-transition tool is available{label}. Do not \
             claim that the workflow advanced in prose or invent a replacement tool; the \
             enclosing runner will apply its declared phase graph after this turn."
        );
    }
    let mut sections = Vec::new();
    if !transitions.is_empty() {
        let names = transitions
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ");
        sections.push(format!(
            "[PHASE TRANSITION TOOLS]\nAvailable transition tool(s){label}: {names}. Call \
             exactly the appropriate tool when this phase's work is complete or a documented \
             branch must be taken. A phase changes only after a transition tool call succeeds; \
             prose such as 'done' or 'moving to the next phase' never advances the workflow. \
             After a successful transition call, stop and let the runner take control."
        ));
    }
    if !mutations.is_empty() {
        let names = mutations
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ");
        sections.push(format!(
            "[GOAL LIST MUTATION TOOLS]\nAvailable non-transition tool(s){label}: {names}. \
             Use these only to record newly discovered concrete work. They do not change phase \
             or finish the current goal; after a successful mutation, continue the current \
             goal and call its normal transition tool when complete."
        ));
    }
    sections.join("\n\n")
}

/// The continuation message for a turn that ended without a required call.
pub fn default_continuation(kind: ContinuationKind, transitions: &[String]) -> String {
    match kind {
        ContinuationKind::Completion => "Continue implementing — you have not yet called \
            mark_execute_complete(). Resume from where you left off and complete all remaining \
            tasks. Call mark_execute_complete() once everything is done."
            .to_string(),
        ContinuationKind::Review => "You have not yet called approve_review() or \
            reject_review(). Review the implementation now and call one of these tools: \
            approve_review(summary) if all tests pass and the code is correct, or \
            reject_review(reason) if there are issues that need fixing."
            .to_string(),
        ContinuationKind::Plan => "You have not yet finalized the plan. Return to the task \
            described above, develop or refine your implementation plan, present it with \
            request_plan_approval(plan), and once it is approved call finalize_plan(plan)."
            .to_string(),
        ContinuationKind::Declared => format!(
            "You have not yet called any of this phase's transition tools ({}). Resume from \
             where you left off, finish the phase's work, then call exactly one of them.",
            transitions.join(", ")
        ),
    }
}

/// Why a phase loops until a tool is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationKind {
    /// `require_plan_finalization`.
    Plan,
    /// `require_explicit_completion`.
    Completion,
    /// `require_explicit_review`.
    Review,
    /// Declared `[[phases.transitions]]`.
    Declared,
}

/// Everything a prompt template can reference.
pub struct TemplateContext<'a> {
    /// The run.
    pub state: &'a WorkflowRunState,
    /// The run's workflow data.
    pub data: &'a RunData,
    /// The phase being prompted.
    pub phase: &'a str,
    /// Its iteration (1-based).
    pub iteration: u32,
}

fn placeholder_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\{([a-z_][a-z0-9_]*(?:\.[A-Za-z0-9_\-]+){0,2})\}").expect("valid")
    })
}

/// Fill `{placeholder}`s in `template`. Unknown placeholders are left as-is,
/// so literal braces in a prompt survive.
pub fn render_template(template: &str, ctx: &TemplateContext<'_>) -> String {
    placeholder_re()
        .replace_all(template, |caps: &regex::Captures<'_>| {
            let key = &caps[1];
            resolve_placeholder(key, ctx).unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

fn resolve_placeholder(key: &str, ctx: &TemplateContext<'_>) -> Option<String> {
    let state = ctx.state;
    let mut parts = key.splitn(3, '.');
    let head = parts.next()?;
    let second = parts.next();
    let third = parts.next();
    match (head, second, third) {
        ("intent", None, None) => Some(state.intent.clone()),
        ("phase", None, None) => Some(ctx.phase.to_string()),
        ("workflow", None, None) => Some(state.workflow_name.clone()),
        ("working_dir", None, None) => Some(state.working_dir.display().to_string()),
        ("iteration", None, None) => Some(ctx.iteration.to_string()),
        ("context", None, None) => Some(state.context_block()),
        ("previous_output", None, None) => Some(
            state
                .last_output()
                .map(|o| o.full_text.clone())
                .unwrap_or_default(),
        ),
        ("goals", None, None) => Some(goals_block(ctx.data)),
        ("next_goal", None, None) => Some(next_goal(ctx.data)),
        ("artifacts", None, None) => Some(artifacts_block(ctx.data)),
        ("output", Some(phase), None) => Some(
            state
                .phase_outputs
                .get(phase)
                .map(|o| o.full_text.clone())
                .unwrap_or_default(),
        ),
        ("data", Some(phase), Some(arg)) => Some(
            state
                .phase_outputs
                .get(phase)
                .and_then(|o| o.structured.as_ref())
                .and_then(|s| s.get(arg))
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default(),
        ),
        ("state", Some(k), None) => Some(
            ctx.data
                .get("state")
                .and_then(|m| m.get(k).cloned())
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default(),
        ),
        _ => None,
    }
}

/// agenthicc's `_parse_output_schema`, plus `json`.
pub fn parse_output_schema(text: &str, schema: Option<&str>) -> Option<serde_json::Value> {
    use serde_json::json;
    let schema = schema?;
    static PLAN: OnceLock<Regex> = OnceLock::new();
    static REVIEW: OnceLock<Regex> = OnceLock::new();
    match schema {
        "plan" => {
            let re = PLAN.get_or_init(|| Regex::new(r"(?s)<plan>(.*?)</plan>").expect("valid"));
            let plan = re
                .captures(text)
                .map(|c| c[1].trim().to_string())
                .unwrap_or_else(|| text.to_string());
            Some(json!({ "plan_text": plan }))
        }
        "review_result" => {
            let re =
                REVIEW.get_or_init(|| Regex::new(r"(?is)<review>(.*?)</review>").expect("valid"));
            match re.captures(text) {
                Some(c) => {
                    let content = c[1].trim().to_string();
                    let approved = content.to_ascii_lowercase().starts_with("approved");
                    Some(json!({ "content": content, "approved": approved }))
                }
                None => Some(json!({ "content": text, "approved": null, "incomplete": true })),
            }
        }
        "free_text" => Some(json!({ "text": text })),
        "json" => {
            let trimmed = text.trim();
            let body = trimmed
                .strip_prefix("```json")
                .or_else(|| trimmed.strip_prefix("```"))
                .and_then(|b| b.strip_suffix("```"))
                .unwrap_or(trimmed)
                .trim();
            Some(
                serde_json::from_str(body)
                    .unwrap_or_else(|_| json!({ "raw": text, "parse_error": true })),
            )
        }
        _ => Some(json!({ "raw": text })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::state::PhaseOutput;
    use serde_json::json;

    fn state() -> WorkflowRunState {
        let mut s = WorkflowRunState::new("r", "flow", "add a cache", "s", "/w", "fp");
        s.add_output(PhaseOutput {
            phase_name: "design".into(),
            role: "planner".into(),
            full_text: "use redis".into(),
            structured: Some(json!({"design": "use redis", "owner": "me"})),
            ..Default::default()
        });
        s
    }

    #[test]
    fn templates_fill_known_placeholders_and_keep_unknown_ones() {
        let s = state();
        let data = RunData::default();
        data.set("state", json!({"stack": "rust"}));
        let ctx = TemplateContext {
            state: &s,
            data: &data,
            phase: "build",
            iteration: 2,
        };
        let out = render_template(
            "{intent} | {phase}#{iteration} | {output.design} | {data.design.owner} | \
             {state.stack} | {previous_output} | {unknown} | {\"json\": 1}",
            &ctx,
        );
        assert_eq!(
            out,
            "add a cache | build#2 | use redis | me | rust | use redis | {unknown} | {\"json\": 1}"
        );
    }

    #[test]
    fn transition_instruction_matches_agenthicc_wording() {
        let none = transition_instruction("summarize", &[], &[]);
        assert!(none.contains("No phase-transition tool is available in the 'summarize' phase"));
        let some = transition_instruction(
            "review",
            &["approve_review".into(), "reject_review".into()],
            &["append_goal".into()],
        );
        assert!(some.contains("`approve_review`, `reject_review`"));
        assert!(some.contains("[GOAL LIST MUTATION TOOLS]"));
        assert!(some.contains("prose such as 'done'"));
    }

    #[test]
    fn output_schemas_parse_like_agenthicc() {
        assert_eq!(
            parse_output_schema("x <plan> step 1 </plan> y", Some("plan")).unwrap(),
            json!({"plan_text": "step 1"})
        );
        assert_eq!(
            parse_output_schema("<review>Approved, nice</review>", Some("review_result")).unwrap()
                ["approved"],
            json!(true)
        );
        let incomplete = parse_output_schema("looks fine", Some("review_result")).unwrap();
        assert_eq!(incomplete["incomplete"], json!(true));
        assert_eq!(
            parse_output_schema("```json\n{\"a\": 1}\n```", Some("json")).unwrap(),
            json!({"a": 1})
        );
        assert!(parse_output_schema("t", None).is_none());
    }

    #[test]
    fn read_only_roles_default_to_read_caps() {
        assert_eq!(
            role_default_capabilities("reviewer").unwrap(),
            READ_CAPS.to_vec()
        );
        assert!(role_default_capabilities("auto").is_none());
        assert!(role_default_capabilities("human").unwrap().is_empty());
    }

    #[test]
    fn mode_policies() {
        assert!(mode_policy(Some("plan")).read_only);
        assert!(mode_policy(Some("Review")).read_only);
        assert!(!mode_policy(Some("yolo")).read_only);
        assert!(!mode_policy(None).read_only);
        assert!(mode_policy(None).patch.is_empty());
        for safe in ["safe", "guard", "ask"] {
            let p = mode_policy(Some(safe));
            assert!(p.approval_gated && !p.read_only, "{safe} is gate-and-ask");
        }
    }
}
