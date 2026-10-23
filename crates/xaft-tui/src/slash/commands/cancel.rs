//! `/cancel` command handler (alias: `/interrupt`).
//!
//! Matches agenthicc's `_cmd_cancel`, which distinguishes "there is a run to
//! stop" from "nothing is running" and reports which case it hit:
//!
//! ```python
//! def _cmd_cancel(ctx):
//!     if ctx.cancel_active() ...:
//!         print("Cancellation requested for the active run.")
//!     else:
//!         print("No active intent to cancel.")
//! ```

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct CancelHandler;

impl SlashHandler for CancelHandler {
    fn description(&self) -> &'static str {
        "Cancel the currently running intent"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        if ctx.request_cancel() {
            CommandResult::Lines(vec!["Cancellation requested for the active run.".into()])
        } else {
            CommandResult::Lines(vec!["No active intent to cancel.".into()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn ctx(intent_active: bool) -> (CommandContext, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        (
            CommandContext {
                args: String::new(),
                command: SlashCommand::Cancel,
                signals: Arc::new(SignalBus::new()),
                config: Arc::new(XaftConfig::default()),
                session_id: None,
                working_dir: PathBuf::from("/tmp"),
                terminal_cols: 80,
                llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
                conversation_store: None,
                session_store: None,
                intent_active: Arc::new(AtomicBool::new(intent_active)),
                cancel_requested: Arc::clone(&cancel),
                input_history: None,
                services: Default::default(),
            },
            cancel,
        )
    }

    #[test]
    fn cancel_with_active_intent_raises_the_flag() {
        let (c, flag) = ctx(true);
        let out = CancelHandler.execute(c);
        assert!(matches!(&out, CommandResult::Lines(v)
            if v == &vec!["Cancellation requested for the active run.".to_string()]));
        assert!(
            flag.load(Ordering::SeqCst),
            "/cancel must raise cancel_requested so the run loop can act on it"
        );
    }

    #[test]
    fn cancel_with_no_active_intent_does_not_raise_the_flag() {
        let (c, flag) = ctx(false);
        let out = CancelHandler.execute(c);
        assert!(matches!(&out, CommandResult::Lines(v)
            if v == &vec!["No active intent to cancel.".to_string()]));
        assert!(
            !flag.load(Ordering::SeqCst),
            "nothing was running, so nothing should be cancelled"
        );
    }
}
