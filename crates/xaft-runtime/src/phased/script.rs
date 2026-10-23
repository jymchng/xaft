//! Script phases — a workflow's escape hatch for arbitrary logic.
//!
//! agenthicc custom workflows can put any Python in their runner. A
//! declarative xaft workflow gets the same power, in any language, from a
//! phase with `agent_type = "script"`: the engine runs its `command` with
//! `sh -c` in the run's working directory, without a model, and the command
//! decides what happens next. This is the "TOML graph + scripted escape
//! hatch" design `docs/design/custom-workflows.md` recommended.
//!
//! # What the command sees
//!
//! The command is **never templated** — interpolating the user's intent into
//! a shell line would be an injection hole. The run is described through
//! environment variables instead:
//!
//! | variable | value |
//! |---|---|
//! | `XAFT_WORKFLOW` | workflow name |
//! | `XAFT_RUN_ID` | durable run id |
//! | `XAFT_PHASE` / `XAFT_ITERATION` | this phase and its entry count |
//! | `XAFT_INTENT` | the run's intent |
//! | `XAFT_WORKING_DIR` | the run's working directory |
//! | `XAFT_RUN_FILE` | the run's checkpoint JSON (every phase output and the workflow state) |
//! | `XAFT_PREVIOUS_OUTPUT` | the most recent phase output, truncated to 32 KiB |
//!
//! # How it decides
//!
//! If stdout — or its last non-empty line — is a JSON object, its keys are
//! read: `approved` (bool), `summary` (string), `next` (a phase name, or
//! `"end"`), `exit` (bool: end the run early, like `exit_workflow`) and
//! `state` (an object of values merged into the workflow state that prompts
//! read as `{state.<key>}`). Otherwise the exit code decides: zero is
//! approved, non-zero is rejected. A rejected phase follows `on_reject`;
//! with no `on_reject` it fails the phase (so `on_error` applies, or the run
//! stops resumably).
//!
//! The command goes through the same approval path as any engine-initiated
//! action: with a user attached it is shown and must be approved first.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracing::warn;

use xaft_config::types::PhaseDefinition;

use crate::error::RuntimeError;

use super::groups::RunData;
use super::state::{PhaseOutput, WorkflowRunState, snippet};
use super::{Engine, META_END, META_EXIT, META_NEXT, tools};

/// Default seconds before a script phase's command is killed.
pub const DEFAULT_SCRIPT_TIMEOUT_S: u64 = 600;

/// Largest value passed in `XAFT_PREVIOUS_OUTPUT`.
const PREVIOUS_OUTPUT_LIMIT: usize = 32 * 1024;

/// The decision a script's stdout can carry.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ScriptVerdict {
    pub approved: Option<bool>,
    pub summary: Option<String>,
    pub next: Option<String>,
    pub exit: bool,
    pub state: serde_json::Map<String, Value>,
}

/// Read a JSON verdict from the whole stdout, or from its last non-empty line.
pub(crate) fn parse_verdict(stdout: &str) -> Option<ScriptVerdict> {
    let trimmed = stdout.trim();
    let candidate = serde_json::from_str::<Value>(trimmed)
        .ok()
        .filter(Value::is_object)
        .or_else(|| {
            trimmed
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .and_then(|l| serde_json::from_str::<Value>(l.trim()).ok())
                .filter(Value::is_object)
        })?;
    let obj = candidate.as_object()?;
    Some(ScriptVerdict {
        approved: obj.get("approved").and_then(Value::as_bool),
        summary: obj
            .get("summary")
            .or_else(|| obj.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string),
        next: obj.get("next").and_then(Value::as_str).map(str::to_string),
        exit: obj.get("exit").and_then(Value::as_bool).unwrap_or(false),
        state: obj
            .get("state")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    })
}

fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let skipped: String = text.chars().skip(count - max).collect();
    format!("…{skipped}")
}

impl Engine<'_> {
    /// Run a `script` phase.
    pub(super) async fn run_script_phase(
        &self,
        spec: &PhaseDefinition,
        state: &WorkflowRunState,
        data: &RunData,
    ) -> Result<PhaseOutput, RuntimeError> {
        let t0 = Instant::now();
        let command = spec.command.clone().unwrap_or_default();
        let base = PhaseOutput {
            phase_name: spec.name.clone(),
            role: "script".to_string(),
            ..Default::default()
        };

        // ── Approval ──
        let allowed = tools::ask_user_approval(
            self.deps.approval_gate.as_ref(),
            Some(&self.deps.signals),
            "workflow_script",
            &uuid::Uuid::new_v4().to_string(),
            &json!({ "phase": spec.name, "command": command }),
        )
        .await;
        if !allowed {
            let text = format!("The user declined to run `{command}`.");
            if spec.on_reject.is_some() {
                return Ok(PhaseOutput {
                    full_text: text,
                    approved: Some(false),
                    duration_s: t0.elapsed().as_secs_f64(),
                    ..base
                });
            }
            return Err(RuntimeError::Agent(format!(
                "script phase '{}': {text}",
                spec.name
            )));
        }

        // ── Environment ──
        let iteration = state.phase_iterations.get(&spec.name).copied().unwrap_or(1);
        let run_file = match &self.deps.checkpoints {
            Some(store) => store.dir().join(format!("{}.json", state.run_id)),
            None => {
                // No checkpoint store: hand the script a private snapshot.
                let path = std::env::temp_dir()
                    .join(format!("xaft-workflow-{}-{}.json", state.run_id, spec.name));
                let mut snapshot = state.clone();
                snapshot.data = data.snapshot();
                if let Err(e) = std::fs::write(
                    &path,
                    serde_json::to_vec_pretty(&snapshot).unwrap_or_default(),
                ) {
                    warn!(error = %e, "could not write the script phase's run file");
                }
                path
            }
        };
        let previous = state
            .last_output()
            .map(|o| tail(&o.full_text, PREVIOUS_OUTPUT_LIMIT))
            .unwrap_or_default();

        #[cfg(unix)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(&command);
            c
        };
        #[cfg(not(unix))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(&command);
            c
        };
        cmd.current_dir(&self.deps.working_dir)
            .env("XAFT_WORKFLOW", &self.wf.name)
            .env("XAFT_RUN_ID", &state.run_id)
            .env("XAFT_PHASE", &spec.name)
            .env("XAFT_ITERATION", iteration.to_string())
            .env("XAFT_INTENT", &state.intent)
            .env("XAFT_WORKING_DIR", &self.deps.working_dir)
            .env("XAFT_RUN_FILE", &run_file)
            .env("XAFT_PREVIOUS_OUTPUT", previous)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // ── Run ──
        let timeout = Duration::from_secs(spec.timeout_s.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_S));
        let (success, code, stdout, stderr) =
            match tokio::time::timeout(timeout, cmd.output()).await {
                Ok(Ok(out)) => (
                    out.status.success(),
                    out.status.code(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                ),
                Ok(Err(e)) => {
                    return Err(RuntimeError::Agent(format!(
                        "script phase '{}': cannot run `{command}`: {e}",
                        spec.name
                    )));
                }
                Err(_) => (
                    false,
                    None,
                    String::new(),
                    format!("timed out after {}s", timeout.as_secs()),
                ),
            };

        // ── Verdict ──
        let verdict = parse_verdict(&stdout).unwrap_or_default();
        if !verdict.state.is_empty() {
            let mut merged = match data.get("state") {
                Some(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            merged.extend(verdict.state.clone());
            data.set("state", Value::Object(merged));
        }
        let approved = verdict.approved.unwrap_or(success);
        let code_label = code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "none".to_string());
        let mut full_text = verdict.summary.clone().unwrap_or_else(|| {
            let out = tail(stdout.trim(), 4000);
            if out.is_empty() {
                format!("`{command}` exited with code {code_label}.")
            } else {
                out
            }
        });
        if !success && verdict.summary.is_none() && !stderr.trim().is_empty() {
            full_text.push_str(&format!("\n\nstderr:\n{}", tail(stderr.trim(), 2000)));
        }

        let mut metadata: BTreeMap<String, Value> = BTreeMap::new();
        metadata.insert("command".to_string(), json!(command));
        metadata.insert("exit_code".to_string(), json!(code));
        match verdict.next.as_deref() {
            Some("end") => {
                metadata.insert(META_END.to_string(), json!(true));
            }
            Some(next) => {
                if self.phase(next).is_none() {
                    return Err(RuntimeError::Agent(format!(
                        "script phase '{}' chose next = '{next}', which is not a phase",
                        spec.name
                    )));
                }
                metadata.insert(META_NEXT.to_string(), json!(next));
            }
            None => {}
        }
        if verdict.exit {
            metadata.insert(META_EXIT.to_string(), json!(true));
        }

        let routed = verdict.next.is_some() || verdict.exit || spec.on_reject.is_some();
        if !approved && !routed {
            return Err(RuntimeError::Agent(format!(
                "script phase '{}' failed (exit code {code_label}): {}",
                spec.name,
                snippet(&full_text, 600)
            )));
        }
        Ok(PhaseOutput {
            full_text,
            structured: Some(json!({
                "exit_code": code,
                "stdout": tail(&stdout, 8000),
                "stderr": tail(&stderr, 4000),
            })),
            approved: Some(approved),
            metadata,
            duration_s: t0.elapsed().as_secs_f64(),
            ..base
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_verdict_is_read_from_stdout_or_its_last_line() {
        let v = parse_verdict(r#"{"approved": false, "summary": "2 failing", "next": "fix", "state": {"failures": "2"}}"#)
            .unwrap();
        assert_eq!(v.approved, Some(false));
        assert_eq!(v.summary.as_deref(), Some("2 failing"));
        assert_eq!(v.next.as_deref(), Some("fix"));
        assert_eq!(v.state["failures"], json!("2"));

        let v = parse_verdict("compiling...\nrunning 3 tests\n{\"summary\": \"ok\"}\n").unwrap();
        assert_eq!(v.summary.as_deref(), Some("ok"));
        assert_eq!(v.approved, None, "the exit code still decides");

        assert!(parse_verdict("plain output").is_none());
        assert!(
            parse_verdict("[1, 2]").is_none(),
            "only objects are verdicts"
        );
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("abcdef", 3), "…def");
        assert_eq!(tail("abc", 3), "abc");
    }
}
