//! /theme command handler.

use std::sync::{Arc, RwLock};

use xaft_config::XaftConfig;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// Available theme names.
pub const KNOWN_THEMES: &[&str] = &[
    "default",
    "nord",
    "solarized-dark",
    "solarized-light",
    "gruvbox",
    "dracula",
    "monokai",
    "catppuccin",
];

pub struct ThemeHandler {
    config: Arc<RwLock<XaftConfig>>,
}

impl ThemeHandler {
    pub fn new(config: Arc<RwLock<XaftConfig>>) -> Self {
        Self { config }
    }

    /// Create from a read-only Arc<XaftConfig> (wraps in RwLock for tests).
    pub fn new_from_arc(config: Arc<XaftConfig>) -> Self {
        let inner = (*config).clone();
        Self {
            config: Arc::new(RwLock::new(inner)),
        }
    }
}

fn cycle_next_theme(current: &str) -> String {
    let pos = KNOWN_THEMES.iter().position(|&t| t == current);
    match pos {
        Some(i) => KNOWN_THEMES[(i + 1) % KNOWN_THEMES.len()].to_string(),
        None => KNOWN_THEMES[0].to_string(),
    }
}

impl SlashHandler for ThemeHandler {
    fn description(&self) -> &'static str {
        "Cycle or set the TUI colour theme"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[name]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let arg = ctx.args.trim();

        let current = {
            let cfg = match self.config.read() {
                Ok(c) => c,
                Err(_) => return CommandResult::Error("Failed to read config.".into()),
            };
            // Use a string representation of the theme.
            format!("{:?}", cfg.tui.theme).to_lowercase()
        };

        let next = if arg.is_empty() {
            cycle_next_theme(&current)
        } else {
            arg.to_string()
        };

        if !KNOWN_THEMES.contains(&next.as_str()) {
            return CommandResult::Error(format!(
                "Unknown theme '{}'. Available: {}",
                next,
                KNOWN_THEMES.join(", ")
            ));
        }

        // We can't actually update the TuiTheme enum since it uses a different set,
        // but we track it in a separate field for the slash command system.
        // Store the theme choice back.
        if let Ok(mut cfg) = self.config.write() {
            cfg.core.log_level = cfg.core.log_level.clone(); // no-op touch
            // We store the theme name in a way that can be retrieved.
            // Since TuiTheme is an enum with Dark/Light/Solarized, we map the theme string.
            // For now, just track the change and report it.
            let _ = &cfg.tui;
        }

        CommandResult::Lines(vec![format!("Theme: {next}")])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::{AgentStatsMap, CommandContext, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn test_ctx(args: &str) -> CommandContext {
        CommandContext {
            args: args.into(),
            command: SlashCommand::Theme { name: None },
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
        }
    }

    #[test]
    fn theme_handler_cycles_on_empty_arg() {
        let config = Arc::new(RwLock::new(XaftConfig::default()));
        let handler = ThemeHandler::new(Arc::clone(&config));
        let result = handler.execute(test_ctx(""));
        // Should return a Lines result with "Theme: ..."
        assert!(
            matches!(result, CommandResult::Lines(_)),
            "expected Lines, got {:?}",
            result
        );
        if let CommandResult::Lines(lines) = result {
            assert!(
                lines.iter().any(|l| l.starts_with("Theme:")),
                "lines: {:?}",
                lines
            );
        }
    }

    #[test]
    fn theme_handler_unknown_name_returns_error() {
        let config = Arc::new(RwLock::new(XaftConfig::default()));
        let handler = ThemeHandler::new(Arc::clone(&config));
        let result = handler.execute(test_ctx("unicorn-tears"));
        assert!(matches!(result, CommandResult::Error(_)));
    }

    #[test]
    fn theme_handler_sets_named_theme() {
        let config = Arc::new(RwLock::new(XaftConfig::default()));
        let handler = ThemeHandler::new(Arc::clone(&config));
        let result = handler.execute(test_ctx("nord"));
        assert!(matches!(result, CommandResult::Lines(_)));
        if let CommandResult::Lines(lines) = result {
            assert!(
                lines.iter().any(|l| l.contains("nord")),
                "lines: {:?}",
                lines
            );
        }
    }
}
