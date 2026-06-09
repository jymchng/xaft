//! /config command handler.

use std::sync::Arc;

use xaft_config::XaftConfig;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ConfigHandler {
    config: Arc<XaftConfig>,
}

impl ConfigHandler {
    pub fn new(config: Arc<XaftConfig>) -> Self {
        Self { config }
    }
}

impl SlashHandler for ConfigHandler {
    fn description(&self) -> &'static str {
        "Show the resolved configuration"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[key.path]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let key_filter = ctx.args.trim();

        // Serialize the config to TOML.
        let toml_str = match toml::to_string_pretty(&*self.config) {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("Failed to serialize config: {e}")),
        };

        if key_filter.is_empty() {
            let mut lines = vec!["Resolved config (6-layer merge):".to_string()];
            for line in toml_str.lines() {
                lines.push(format!("  {}", line));
            }
            CommandResult::Lines(lines)
        } else {
            // Filter lines that relate to the given key path.
            // Simple heuristic: find the [section] matching the prefix and print until next [section].
            let section_header = format!("[{}]", key_filter);
            let mut in_section = false;
            let mut found = false;
            let mut output_lines = Vec::new();

            for line in toml_str.lines() {
                if line.trim_start().starts_with('[') {
                    if line.contains(&section_header) {
                        in_section = true;
                        found = true;
                        output_lines.push(format!("  {}", line));
                    } else if in_section {
                        break; // End of relevant section.
                    }
                } else if in_section {
                    output_lines.push(format!("  {}", line));
                }
            }

            if !found {
                return CommandResult::Error(format!("No config key matches '{key_filter}'."));
            }

            CommandResult::Lines(output_lines)
        }
    }
}
