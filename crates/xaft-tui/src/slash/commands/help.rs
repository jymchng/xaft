//! /help command handler.

use std::sync::Arc;

use crate::slash::parser::COMMAND_TABLE;
use crate::slash::registry::{SlashCommandRegistry, SlashHandler};
use crate::slash::{CommandContext, CommandResult};

pub struct HelpHandler {
    registry: Option<Arc<SlashCommandRegistry>>,
}

impl HelpHandler {
    pub fn new(registry: Arc<SlashCommandRegistry>) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    /// Create a help handler that reads directly from COMMAND_TABLE (no registry needed).
    pub fn new_with_table() -> Self {
        Self { registry: None }
    }
}

impl SlashHandler for HelpHandler {
    fn description(&self) -> &'static str {
        "Show this help (or /help <command> for details)"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[command]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let arg = ctx.args.trim();

        if arg.is_empty() {
            // List all canonical commands alphabetically (deduplicate by canonical name).
            let mut seen_canonical: std::collections::HashSet<&'static str> =
                std::collections::HashSet::new();
            let mut entries: Vec<(&'static str, &'static str)> = Vec::new();

            for (trigger, meta) in COMMAND_TABLE.iter() {
                if meta.canonical == *trigger && seen_canonical.insert(meta.canonical) {
                    entries.push((meta.canonical, meta.description));
                }
            }
            entries.sort_by_key(|(name, _)| *name);

            let trigger_width = entries.iter().map(|(n, _)| n.len()).max().unwrap_or(0) + 2;

            let mut lines = vec!["Slash commands:".to_string(), String::new()];
            for (name, desc) in &entries {
                let padded = format!("/{:<width$}", name, width = trigger_width);
                lines.push(format!("  {}  {}", padded, desc));
            }
            lines.push(String::new());
            lines.push("  Type /help <command> for detailed usage.".to_string());
            return CommandResult::Lines(lines);
        }

        // Single-command detail.
        let trigger = arg.trim_start_matches('/').to_ascii_lowercase();
        match COMMAND_TABLE.iter().find(|(k, _)| **k == trigger) {
            None => CommandResult::Error(format!(
                "Unknown command: /{trigger}. Type /help for a list."
            )),
            Some((_, meta)) => {
                let aliases: Vec<String> = if meta.aliases.is_empty() {
                    vec!["(none)".to_string()]
                } else {
                    meta.aliases.iter().map(|a| format!("/{a}")).collect()
                };
                let args_str = meta.args_hint.unwrap_or("(none)");
                let mut lines = vec![
                    format!("  /{} — {}", meta.canonical, meta.description),
                    format!("  Aliases: {}", aliases.join(", ")),
                    format!("  Args:     {}", args_str),
                ];
                lines.push(String::new());
                lines.push(format!("  Example: /{}", meta.canonical));
                CommandResult::Lines(lines)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::commands::build_default_registry;
    use crate::slash::{AgentStatsMap, CommandContext, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn test_ctx(args: &str) -> CommandContext {
        CommandContext {
            args: args.into(),
            command: SlashCommand::Help,
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

    fn result_to_string(result: &CommandResult) -> String {
        match result {
            CommandResult::Lines(ls) => ls.join("\n"),
            CommandResult::StyledLines(ls) => ls
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            CommandResult::Error(e) => e.clone(),
            CommandResult::Handled => String::new(),
        }
    }

    #[test]
    fn help_handler_lists_all_commands() {
        let registry = build_default_registry();
        let handler = HelpHandler::new(Arc::new(registry));
        let ctx = test_ctx("");
        let result = handler.execute(ctx);
        let text = result_to_string(&result);
        for trigger in &["clear", "compact", "cost", "model", "agents"] {
            assert!(
                text.contains(trigger),
                "missing {trigger} in /help output:\n{text}"
            );
        }
    }

    #[test]
    fn help_handler_single_command_detail() {
        let registry = build_default_registry();
        let handler = HelpHandler::new(Arc::new(registry));
        let ctx = test_ctx("compact");
        let result = handler.execute(ctx);
        let text = result_to_string(&result);
        assert!(
            text.contains("Manually trigger context compaction"),
            "text was:\n{text}"
        );
        assert!(text.contains("Aliases:"), "text was:\n{text}");
    }

    #[test]
    fn help_handler_unknown_subcommand() {
        let registry = build_default_registry();
        let handler = HelpHandler::new(Arc::new(registry));
        let ctx = test_ctx("blorp");
        assert!(matches!(handler.execute(ctx), CommandResult::Error(_)));
    }
}
