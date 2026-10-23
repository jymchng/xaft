//! `/workers` — durable parallel worker tasks for this session (parity slash
//! row 21).
//!
//! agenthicc's `_cmd_workers` is `ManifestStore().list()` filtered to
//! `parent_session_id == ctx.session_id` (every manifest when the session has
//! no id yet), printed as a `Parallel Workers` table — Orchestration / Task /
//! Status / Worktree, one row per task — or, with `--json`, as the manifests
//! themselves. This handler does exactly that over the manifest roots in
//! `ctx.services.worker_roots`: in a live TUI, the workspace's
//! `.xaft/workers/` (the planner's `explore_repository` fan-outs, recorded by
//! `xaft_runtime::workers::WorkerRecorder`) and the user-level orchestration
//! store the `worktrees` subsystem writes. Explorer subagents share the
//! session's workspace, so their Worktree cell is `—`.

use serde_json::Value;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// The empty-table row agenthicc prints.
pub const NO_WORKER_ORCHESTRATIONS: &str = "no worker orchestrations";

/// The manifest roots a live TUI reads: this workspace's recorded fan-outs
/// (`<workspace>/.xaft/workers`, where the planner's `explore_repository`
/// records them) and the user-level orchestration store
/// (`$XAFT_HOME/orchestrations`, else `~/.xaft/orchestrations`) that the
/// `worktrees` subsystem's coordinator writes.
pub fn default_worker_roots(working_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut roots = vec![xaft_runtime::workers::workspace_worker_root(working_dir)];
    if let Some(home) = std::env::var_os("XAFT_HOME") {
        roots.push(std::path::PathBuf::from(home).join("orchestrations"));
    } else if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".xaft").join("orchestrations"));
    }
    roots
}

fn cut(value: &str, n: usize) -> String {
    value.chars().take(n).collect()
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `/workers [--json]`.
pub struct WorkersHandler;

impl SlashHandler for WorkersHandler {
    fn description(&self) -> &'static str {
        "Show durable parallel worker tasks"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[--json]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let visible = xaft_runtime::workers::manifests_for_session(
            &ctx.services.worker_roots,
            ctx.session_id.as_deref(),
        );
        if ctx.args.split_whitespace().any(|a| a == "--json") {
            // Keys come out sorted (`serde_json::Value` maps are `BTreeMap`s
            // here), like agenthicc's `json.dumps(..., sort_keys=True)`.
            let json =
                serde_json::to_string(&Value::Array(visible)).unwrap_or_else(|_| "[]".to_string());
            return CommandResult::Lines(vec![json]);
        }
        let mut lines = vec![
            "Parallel Workers".to_string(),
            format!(
                "{:<17} {:<24} {:<12} {}",
                "Orchestration", "Task", "Status", "Worktree"
            ),
        ];
        for manifest in &visible {
            let orchestration = cut(str_field(manifest, "orchestration_id"), 16);
            let tasks = manifest
                .get("tasks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for task in &tasks {
                let worktree = cut(str_field(task, "worktree_id"), 16);
                lines.push(format!(
                    "{:<17} {:<24} {:<12} {}",
                    orchestration,
                    str_field(task, "task_id"),
                    str_field(task, "status"),
                    if worktree.is_empty() {
                        "—".to_string()
                    } else {
                        worktree
                    }
                ));
            }
        }
        if visible.is_empty() {
            lines.push(format!(
                "{:<17} {:<24} {:<12} {}",
                "—", "—", NO_WORKER_ORCHESTRATIONS, "—"
            ));
        }
        CommandResult::Lines(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::{AgentStatsMap, SessionServices, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;
    use xaft_runtime::workers::WorkerRecorder;

    fn ctx(args: &str, session: Option<&str>, roots: Vec<PathBuf>) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command: SlashCommand::Workers,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: session.map(str::to_string),
            working_dir: PathBuf::from("."),
            terminal_cols: 120,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
            services: SessionServices {
                worker_roots: roots,
                ..Default::default()
            },
        }
    }

    fn lines(result: CommandResult) -> Vec<String> {
        match result {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_store_prints_agenthiccs_empty_row() {
        let dir = tempfile::tempdir().unwrap();
        let out = lines(WorkersHandler.execute(ctx("", Some("s1"), vec![dir.path().into()])));
        assert_eq!(out[0], "Parallel Workers");
        assert!(out[2].contains(NO_WORKER_ORCHESTRATIONS), "{out:?}");
    }

    #[test]
    fn workers_lists_this_sessions_tasks_only() {
        let dir = tempfile::tempdir().unwrap();
        let mine = WorkerRecorder::new(dir.path(), "s1", "/repo");
        let mut manifest = mine.begin(&[
            ("explore-1".into(), "src/a.rs".into()),
            ("explore-2".into(), "src/b.rs".into()),
        ]);
        mine.finish(&mut manifest, &[Ok(()), Err("timeout".into())], false);
        WorkerRecorder::new(dir.path(), "s2", "/repo").begin(&[("explore-1".into(), "x".into())]);

        let out = lines(WorkersHandler.execute(ctx("", Some("s1"), vec![dir.path().into()])));
        let rows: Vec<&String> = out.iter().skip(2).collect();
        assert_eq!(rows.len(), 2, "{out:?}");
        let id = manifest["orchestration_id"].as_str().unwrap();
        assert!(rows[0].starts_with(&id[..16]));
        assert!(rows[0].contains("explore-1") && rows[0].contains("completed"));
        assert!(rows[1].contains("explore-2") && rows[1].contains("failed"));
        assert!(
            rows[0].trim_end().ends_with('—'),
            "explorers share the workspace"
        );

        // No session yet: agenthicc shows every orchestration.
        let all = lines(WorkersHandler.execute(ctx("", None, vec![dir.path().into()])));
        assert_eq!(all.len() - 2, 3, "{all:?}");
    }

    #[test]
    fn workers_json_emits_the_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = WorkerRecorder::new(dir.path(), "s1", "/repo");
        recorder.begin(&[("explore-1".into(), "src/a.rs".into())]);
        let out = lines(WorkersHandler.execute(ctx("--json", Some("s1"), vec![dir.path().into()])));
        let parsed: Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["parent_session_id"], serde_json::json!("s1"));
        assert_eq!(
            parsed[0]["tasks"][0]["task_id"],
            serde_json::json!("explore-1")
        );
    }
}
