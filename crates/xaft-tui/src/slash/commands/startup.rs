//! `/startup` — startup phase readiness and timing (parity slash row 19).
//!
//! agenthicc's `_cmd_startup`: no coordinator ⇒ `Startup diagnostics are
//! unavailable.`; nothing recorded ⇒ `No startup phases have been recorded.`;
//! otherwise one `name: state (elapsed)` line per phase, in phase-name order.
//! xaft appends agenthicc's documented `deferred` marker and the redacted error
//! when a phase has one. Safe while startup is still in progress — a loading
//! phase shows its elapsed time so far.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// Shown when no startup coordinator is attached.
pub const STARTUP_UNAVAILABLE: &str = "Startup diagnostics are unavailable.";

/// Shown when the coordinator has no phases yet.
pub const NO_STARTUP_PHASES: &str = "No startup phases have been recorded.";

/// `/startup`.
pub struct StartupHandler;

impl SlashHandler for StartupHandler {
    fn description(&self) -> &'static str {
        "Show startup phase readiness and timing diagnostics"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let Some(startup) = ctx.services.startup.as_ref() else {
            return CommandResult::Lines(vec![STARTUP_UNAVAILABLE.to_string()]);
        };
        let reports = startup.snapshot();
        if reports.is_empty() {
            return CommandResult::Lines(vec![NO_STARTUP_PHASES.to_string()]);
        }
        CommandResult::Lines(reports.iter().map(|r| r.render()).collect())
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
    use xaft_runtime::startup::{ReadinessState, StartupCoordinator};

    fn ctx(services: SessionServices) -> CommandContext {
        CommandContext {
            args: String::new(),
            command: SlashCommand::Startup,
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

    fn lines(result: CommandResult) -> Vec<String> {
        match result {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn startup_without_a_coordinator_is_unavailable() {
        assert_eq!(
            lines(StartupHandler.execute(ctx(SessionServices::default()))),
            vec![STARTUP_UNAVAILABLE.to_string()]
        );
    }

    #[test]
    fn startup_with_no_phases_says_so() {
        let services = SessionServices {
            startup: Some(StartupCoordinator::new()),
            ..Default::default()
        };
        assert_eq!(
            lines(StartupHandler.execute(ctx(services))),
            vec![NO_STARTUP_PHASES.to_string()]
        );
    }

    #[test]
    fn startup_lists_every_phase_in_name_order() {
        let startup = StartupCoordinator::new();
        startup.begin("runtime");
        startup.finish("runtime");
        startup.begin("configuration");
        startup.finish("configuration");
        startup.begin_deferred("skills");
        startup.finish_with(
            "session_service",
            ReadinessState::Degraded,
            Some("sqlite unavailable: password=hunter2"),
        );
        let services = SessionServices {
            startup: Some(startup),
            ..Default::default()
        };
        let out = lines(StartupHandler.execute(ctx(services)));
        assert_eq!(out.len(), 4);
        assert!(out[0].starts_with("configuration: ready ("), "{out:?}");
        assert!(out[1].starts_with("runtime: ready ("), "{out:?}");
        assert!(
            out[2].starts_with("session_service: degraded (")
                && out[2].ends_with("sqlite unavailable: password=<redacted>"),
            "{out:?}"
        );
        assert!(
            out[3].starts_with("skills: loading (") && out[3].contains("deferred"),
            "{out:?}"
        );
    }
}
