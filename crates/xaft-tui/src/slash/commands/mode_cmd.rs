//! /mode command — show or switch the active operational mode.

use std::sync::Arc;

use xaft_config::XaftConfig;

use crate::mode::builtins::builtin_modes;
use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ModeHandler {
    pub config: Arc<XaftConfig>,
}

impl ModeHandler {
    pub fn new(config: Arc<XaftConfig>) -> Self {
        Self { config }
    }
}

impl SlashHandler for ModeHandler {
    fn description(&self) -> &'static str {
        "Show or switch the active mode  (/mode [auto|plan|ask|review|safe|debug])"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[mode-name]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let args = ctx.args.trim().to_lowercase();
        if args.is_empty() {
            // List available modes.
            let mut lines = vec!["Available modes (Shift+Tab to cycle):".to_string()];
            for m in builtin_modes() {
                lines.push(format!(
                    "  {} {} — {}",
                    m.ansi_badge(),
                    m.name,
                    m.description
                ));
            }
            lines.push(String::new());
            lines.push("  /mode <name>   or   Shift+Tab to switch".to_string());
            CommandResult::Lines(lines)
        } else {
            // The actual switch is handled via TuiEvent routing in state.rs.
            // For now, emit a hint; direct switching happens via /mode <name>
            // going through handle_slash_parse_result → SlashCommand::Mode.
            // The state.rs SlashCommand::Mode arm does the actual mode.set().
            CommandResult::Lines(vec![format!(
                "  Switching to '{}' mode — use Shift+Tab to cycle interactively",
                args
            )])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::{AgentStatsMap, CommandContext, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::RwLock;

    fn make_ctx(args: &str) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command: SlashCommand::Mode {
                name: if args.is_empty() {
                    None
                } else {
                    Some(args.to_string())
                },
            },
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("."),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
        }
    }

    #[test]
    fn mode_no_args_lists_modes() {
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let result = h.execute(make_ctx(""));
        let CommandResult::Lines(lines) = result else {
            panic!("expected Lines")
        };
        let text = lines.join("\n");
        assert!(text.contains("auto"), "should list auto mode");
        assert!(text.contains("plan"), "should list plan mode");
        assert!(text.contains("safe"), "should list safe mode");
    }

    #[test]
    fn mode_with_name_shows_switch_message() {
        let h = ModeHandler::new(Arc::new(XaftConfig::default()));
        let result = h.execute(make_ctx("plan"));
        let CommandResult::Lines(lines) = result else {
            panic!("expected Lines")
        };
        assert!(lines.iter().any(|l| l.contains("plan")));
    }
}
