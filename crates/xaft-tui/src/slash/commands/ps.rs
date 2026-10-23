//! `/ps` (alias `/processes`) and `/stop` (alias `/stop-terminal`) — parity
//! rows 20 and 22.
//!
//! Both read the session's ONE background-terminal manager from
//! `ctx.services.terminal_manager` — the same `Arc` the runtime gives
//! `run_command {"background": true}` and the four terminal tools — so they
//! list and stop exactly the terminals the agent started. Their contracts are
//! agenthicc's `_cmd_ps` / `_cmd_stop` (`commands/builtins.py`), message for
//! message:
//!
//! * no manager ⇒ `Background terminal management is unavailable.`
//! * `/ps --json` ⇒ every record's full durable row, keys sorted;
//! * `/ps [terminal-id]` with an overlay host ⇒ the interactive
//!   [`TerminalListMenu`](crate::terminals::TerminalListMenu), pre-selecting
//!   the id; without one ⇒ the `Background Terminals` table, or
//!   `No background terminals for this session.`;
//! * `/stop` ⇒ stop every visible terminal; `/stop all` asks for `--confirm`
//!   (or `--force`) first; `/stop <id>` stops one; `--force` skips the grace
//!   period.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};
use crate::terminals::{TerminalListMenu, terminal_table_lines};

/// Shown when the session has no terminal manager attached.
pub const TERMINALS_UNAVAILABLE: &str = "Background terminal management is unavailable.";

/// Shown when the manager holds no records for this session.
pub const NO_TERMINALS: &str = "No background terminals for this session.";

/// `/ps [terminal-id] [--json]`.
pub struct PsHandler;

impl SlashHandler for PsHandler {
    fn description(&self) -> &'static str {
        "Open the live manager for owned background terminals"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[terminal-id] [--json]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let Some(manager) = ctx.services.terminal_manager.clone() else {
            return CommandResult::Lines(vec![TERMINALS_UNAVAILABLE.to_string()]);
        };
        let args: Vec<&str> = ctx.args.split_whitespace().collect();
        if args.contains(&"--json") {
            let rows: Vec<serde_json::Value> =
                manager.list_records().iter().map(|r| r.to_dict()).collect();
            // `serde_json::Value` objects are `BTreeMap`s here (no
            // `preserve_order`), so keys come out sorted — agenthicc's
            // `json.dumps(..., sort_keys=True)`.
            let json = serde_json::to_string(&serde_json::Value::Array(rows))
                .unwrap_or_else(|_| "[]".to_string());
            return CommandResult::Lines(vec![json]);
        }
        let selected = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .map(|s| s.to_string());
        if ctx.services.overlay_host {
            return CommandResult::OpenMenu(Box::new(TerminalListMenu::new(manager, selected)));
        }
        let records = manager.list_records();
        if records.is_empty() {
            return CommandResult::Lines(vec![NO_TERMINALS.to_string()]);
        }
        CommandResult::Lines(terminal_table_lines(&records))
    }
}

/// `/stop [terminal-id|all] [--force] [--confirm]`.
pub struct StopHandler;

impl SlashHandler for StopHandler {
    fn description(&self) -> &'static str {
        "Stop all or one named background terminal"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[terminal-id|all] [--force]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let Some(manager) = ctx.services.terminal_manager.clone() else {
            return CommandResult::Lines(vec![TERMINALS_UNAVAILABLE.to_string()]);
        };
        let args: Vec<&str> = ctx.args.split_whitespace().collect();
        let force = args.contains(&"--force");
        let target = args.iter().find(|a| !a.starts_with('-')).copied();
        match target {
            Some("all") => {
                let confirmed = force || args.contains(&"--confirm");
                if !confirmed {
                    return CommandResult::Lines(vec![
                        "This will stop every visible background terminal. Repeat as /stop all \
                         --confirm (or --force) to confirm."
                            .to_string(),
                    ]);
                }
                let count = manager.request_stop_all(force);
                CommandResult::Lines(vec![format!(
                    "Stop requested for {count} background terminal(s)."
                )])
            }
            None => {
                let count = manager.request_stop_all(force);
                CommandResult::Lines(vec![format!(
                    "Stop requested for {count} background terminal(s)."
                )])
            }
            Some(id) => {
                if manager.request_stop(id, force) {
                    CommandResult::Lines(vec![format!("Stop requested for {id}.")])
                } else {
                    CommandResult::Lines(vec![format!("No running owned terminal found for {id}.")])
                }
            }
        }
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
    use xaft_tools::terminals::{TerminalManager, TerminalManagerConfig};

    fn ctx(command: SlashCommand, args: &str, services: SessionServices) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("."),
            terminal_cols: 120,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
            services,
        }
    }

    fn manager(dir: &std::path::Path) -> Arc<TerminalManager> {
        let mut config = TerminalManagerConfig::new("sess-ps", dir);
        config.store_root = dir.join("store");
        config.cancel_grace_s = 1.0;
        Arc::new(TerminalManager::new(config))
    }

    fn with(manager: &Arc<TerminalManager>, overlay_host: bool) -> SessionServices {
        SessionServices {
            terminal_manager: Some(Arc::clone(manager)),
            overlay_host,
            ..Default::default()
        }
    }

    async fn start(m: &TerminalManager) -> String {
        let argv = vec!["/bin/sleep".to_string(), "30".to_string()];
        let out = m
            .start(
                None,
                Some(&argv),
                None,
                0.0,
                None,
                false,
                "sleep 30",
                "tc",
                "oneshot",
                None,
            )
            .await;
        out["terminal_id"].as_str().unwrap().to_string()
    }

    fn lines(result: CommandResult) -> Vec<String> {
        match result {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn both_commands_report_a_missing_manager() {
        for (cmd, handler) in [
            (
                SlashCommand::Ps,
                Box::new(PsHandler) as Box<dyn SlashHandler>,
            ),
            (SlashCommand::Stop, Box::new(StopHandler)),
        ] {
            let out = lines(handler.execute(ctx(cmd, "", SessionServices::default())));
            assert_eq!(out, vec![TERMINALS_UNAVAILABLE.to_string()]);
        }
    }

    #[test]
    fn ps_without_an_overlay_host_reports_an_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let out = lines(PsHandler.execute(ctx(SlashCommand::Ps, "", with(&m, false))));
        assert_eq!(out, vec![NO_TERMINALS.to_string()]);
    }

    #[tokio::test]
    async fn ps_lists_a_running_terminal_as_a_table_and_as_json() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m).await;

        let table = lines(PsHandler.execute(ctx(SlashCommand::Ps, "", with(&m, false))));
        assert_eq!(table[0], "Background Terminals");
        assert!(table[1].contains("ID") && table[1].contains("Return"));
        let row = table
            .iter()
            .find(|l| l.contains(&id))
            .expect("terminal row");
        assert!(row.contains("running"), "{row}");
        assert!(
            row.trim_end().ends_with('—'),
            "a live terminal has no return code yet: {row}"
        );

        let json = lines(PsHandler.execute(ctx(SlashCommand::Ps, "--json", with(&m, false))));
        let parsed: serde_json::Value = serde_json::from_str(&json[0]).expect("valid JSON");
        let rows = parsed.as_array().expect("an array of records");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["terminal_id"], serde_json::json!(id));
        // Keys are sorted, as agenthicc's `sort_keys=True` prints them.
        let keys: Vec<&String> = rows[0].as_object().unwrap().keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);

        assert!(m.request_stop(&id, true));
        m.wait(&id, 5.0).await;
    }

    #[tokio::test]
    async fn ps_with_an_overlay_host_opens_the_terminal_overlay_on_the_named_id() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m).await;
        match PsHandler.execute(ctx(SlashCommand::Ps, &id, with(&m, true))) {
            CommandResult::OpenMenu(widget) => {
                assert_eq!(widget.title(), "Background Terminals");
                let rows = widget
                    .overlay_lines(120, crate::menu::OVERLAY_MAX_ROWS)
                    .expect("the overlay draws through the renderer");
                assert!(rows.iter().any(|r| r.text.contains(&id)));
            }
            other => panic!("expected the overlay, got {other:?}"),
        }
        assert!(m.request_stop(&id, true));
        m.wait(&id, 5.0).await;
    }

    #[tokio::test]
    async fn stop_all_requires_confirmation_and_then_stops_everything() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m).await;

        let ask = lines(StopHandler.execute(ctx(SlashCommand::Stop, "all", with(&m, false))));
        assert!(ask[0].starts_with("This will stop every visible background terminal."));
        assert!(
            m.owns_live(&id),
            "an unconfirmed /stop all must not stop anything"
        );

        let done =
            lines(StopHandler.execute(ctx(SlashCommand::Stop, "all --confirm", with(&m, false))));
        assert_eq!(
            done,
            vec!["Stop requested for 1 background terminal(s).".to_string()]
        );
        let waited = m.wait(&id, 5.0).await;
        assert_eq!(waited["state"], serde_json::json!("stopped"), "{waited}");
    }

    #[tokio::test]
    async fn stop_with_no_target_stops_every_terminal_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m).await;
        let out = lines(StopHandler.execute(ctx(SlashCommand::Stop, "--force", with(&m, false))));
        assert_eq!(
            out,
            vec!["Stop requested for 1 background terminal(s).".to_string()]
        );
        let waited = m.wait(&id, 5.0).await;
        assert_eq!(waited["state"], serde_json::json!("stopped"), "{waited}");
    }

    #[tokio::test]
    async fn stop_names_one_terminal_and_refuses_an_unknown_one() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m).await;

        let out = lines(StopHandler.execute(ctx(SlashCommand::Stop, &id, with(&m, false))));
        assert_eq!(out, vec![format!("Stop requested for {id}.")]);
        m.wait(&id, 5.0).await;

        let out = lines(StopHandler.execute(ctx(SlashCommand::Stop, "term-nope", with(&m, false))));
        assert_eq!(
            out,
            vec!["No running owned terminal found for term-nope.".to_string()]
        );
    }
}
