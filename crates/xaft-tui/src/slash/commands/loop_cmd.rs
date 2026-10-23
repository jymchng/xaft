//! `/loop` and `/loops` — parity rows 3 and 4.
//!
//! Thin adapters over the session's [`LoopManager`](crate::loops::LoopManager)
//! (`ctx.services.loop_manager`), mirroring agenthicc's `_cmd_loop` and
//! `_cmd_loops`:
//!
//! * no manager ⇒ `Loop scheduling is unavailable in this session.`
//! * `/loop [<interval>] <prompt or /command> | status | pause | resume | stop`
//!   ⇒ whatever the manager answers;
//! * `/loops` with an overlay host ⇒ the interactive
//!   [`LoopJobsMenu`](crate::loops::LoopJobsMenu); without one ⇒ the
//!   `Scheduled Loops` table; any argument ⇒ `Usage: /loops`.

use crate::loops::{LOOPS_UNAVAILABLE, LoopJobsMenu, loop_table_lines, validate_loop_payload};
use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// `/loop`.
pub struct LoopHandler;

impl SlashHandler for LoopHandler {
    fn description(&self) -> &'static str {
        "Schedule a recurring prompt or registered slash command"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[interval] <prompt> | status | pause | resume | stop")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let Some(manager) = ctx.services.loop_manager.clone() else {
            return CommandResult::Lines(vec![LOOPS_UNAVAILABLE.to_string()]);
        };
        let message = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handle_command(&ctx.args, &validate_loop_payload);
        CommandResult::Lines(vec![message])
    }
}

/// `/loops`.
pub struct LoopsHandler;

impl SlashHandler for LoopsHandler {
    fn description(&self) -> &'static str {
        "Manage all scheduled loop jobs"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let Some(manager) = ctx.services.loop_manager.clone() else {
            return CommandResult::Lines(vec![LOOPS_UNAVAILABLE.to_string()]);
        };
        if !ctx.args.trim().is_empty() {
            return CommandResult::Lines(vec!["Usage: /loops".to_string()]);
        }
        if ctx.services.overlay_host {
            return CommandResult::OpenMenu(Box::new(LoopJobsMenu::new(manager)));
        }
        let guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        CommandResult::Lines(loop_table_lines(&guard.list_jobs(), guard.session_id()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::LoopManager;
    use crate::slash::{AgentStatsMap, SessionServices, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex, RwLock};
    use xaft_config::{LoopsConfig, XaftConfig};

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

    fn services(dir: &std::path::Path, overlay_host: bool) -> SessionServices {
        let mut manager = LoopManager::new("session-1", LoopsConfig::default(), dir);
        manager.start(false);
        SessionServices {
            loop_manager: Some(Arc::new(Mutex::new(manager))),
            overlay_host,
            ..Default::default()
        }
    }

    fn lines(result: CommandResult) -> Vec<String> {
        match result {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn both_commands_report_a_missing_scheduler() {
        let none = SessionServices::default();
        assert_eq!(
            lines(LoopHandler.execute(ctx(SlashCommand::Loop, "5m x", none.clone()))),
            vec![LOOPS_UNAVAILABLE.to_string()]
        );
        assert_eq!(
            lines(LoopsHandler.execute(ctx(SlashCommand::Loops, "", none))),
            vec![LOOPS_UNAVAILABLE.to_string()]
        );
    }

    #[test]
    fn loop_schedules_then_reports_status_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let svc = services(dir.path(), false);
        let out =
            lines(LoopHandler.execute(ctx(SlashCommand::Loop, "5m check the build", svc.clone())));
        assert_eq!(
            out,
            vec![
                "↻ Loop scheduled every 5m; first iteration is due now and expires in 3d."
                    .to_string()
            ]
        );
        let status = lines(LoopHandler.execute(ctx(SlashCommand::Loop, "status", svc.clone())));
        assert!(
            status[0].starts_with("Loop scheduled · every 5m"),
            "{status:?}"
        );
        assert!(status[0].contains("payload=check the build"), "{status:?}");

        let stop = lines(LoopHandler.execute(ctx(SlashCommand::Loop, "stop", svc)));
        assert_eq!(stop, vec!["Loop stopped.".to_string()]);
    }

    #[test]
    fn loop_refuses_an_unknown_scheduled_command() {
        let dir = tempfile::tempdir().unwrap();
        let svc = services(dir.path(), false);
        let out = lines(LoopHandler.execute(ctx(SlashCommand::Loop, "1m /nope", svc)));
        assert_eq!(
            out,
            vec!["Unknown scheduled command '/nope'. Use /help to inspect commands.".to_string()]
        );
    }

    #[test]
    fn loops_lists_jobs_as_a_table_without_an_overlay_host() {
        let dir = tempfile::tempdir().unwrap();
        let svc = services(dir.path(), false);
        let empty = lines(LoopsHandler.execute(ctx(SlashCommand::Loops, "", svc.clone())));
        assert_eq!(empty[0], "Scheduled Loops");
        assert!(empty.last().unwrap().contains("No scheduled loop jobs"));

        lines(LoopHandler.execute(ctx(SlashCommand::Loop, "2m /status", svc.clone())));
        let listed = lines(LoopsHandler.execute(ctx(SlashCommand::Loops, "", svc.clone())));
        let row = listed
            .iter()
            .find(|l| l.contains("/status"))
            .expect("job row");
        assert!(
            row.contains("scheduled") && row.contains("current"),
            "{row}"
        );

        assert_eq!(
            lines(LoopsHandler.execute(ctx(SlashCommand::Loops, "extra", svc))),
            vec!["Usage: /loops".to_string()]
        );
    }

    #[test]
    fn loops_opens_the_overlay_with_an_overlay_host() {
        let dir = tempfile::tempdir().unwrap();
        let svc = services(dir.path(), true);
        match LoopsHandler.execute(ctx(SlashCommand::Loops, "", svc)) {
            CommandResult::OpenMenu(widget) => assert_eq!(widget.title(), "Scheduled Loops"),
            other => panic!("expected the overlay, got {other:?}"),
        }
    }
}
