//! `/status` handler — show the current session and model.
//!
//! Mirrors agenthicc's `_cmd_status`:
//!
//! ```python
//! def _cmd_status(ctx):
//!     print(f"  Session: {ctx.session_id or '(new)'}")
//!     print(f"  Model: {ctx.model or '(unknown)'}")
//! ```

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct StatusHandler;

impl SlashHandler for StatusHandler {
    fn description(&self) -> &'static str {
        "Show the active session and model"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let session = ctx.session_id.as_deref().unwrap_or("(new)");
        let model = configured_model(&ctx);
        CommandResult::Lines(vec![
            format!("Session: {session}"),
            format!("Model: {model}"),
        ])
    }
}

/// The model the `default` agent preset will use, or `(unknown)`.
///
/// `/model` can override the model for the live session, but that override
/// lives in `ModelHandler`, which this context does not carry — so the
/// configured value is what is shown, and it is labelled as such below.
fn configured_model(ctx: &CommandContext) -> String {
    let cfg = &ctx.config;
    if let Some(model) = cfg.agent.get("default").map(|a| a.model.as_str()) {
        if !model.is_empty() {
            return model.to_string();
        }
    }
    let fast = cfg.model_tiers.resolve("").fast;
    if !fast.is_empty() {
        return fast.to_string();
    }
    "(unknown)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn ctx(session_id: Option<&str>, cfg: XaftConfig) -> CommandContext {
        CommandContext {
            args: String::new(),
            command: SlashCommand::Status,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(cfg),
            session_id: session_id.map(str::to_string),
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
        }
    }

    fn lines(out: CommandResult) -> Vec<String> {
        match out {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn shows_session_and_model() {
        let mut cfg = XaftConfig::default();
        cfg.agent.get_mut("default").unwrap().model = "claude-opus-4-8".into();
        let out = lines(StatusHandler.execute(ctx(Some("sess-123"), cfg)));
        assert!(out.iter().any(|l| l.contains("sess-123")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("claude-opus-4-8")), "{out:?}");
    }

    #[test]
    fn new_session_reports_new_placeholder() {
        let out = lines(StatusHandler.execute(ctx(None, XaftConfig::default())));
        assert!(out.iter().any(|l| l.contains("(new)")), "{out:?}");
    }
}
