//! `/commands` handler — list every registered slash command.
//!
//! Mirrors agenthicc's `_cmd_commands`, which lists the registry's
//! non-skill commands with their source and description. xaft's registry is
//! the static `COMMAND_TABLE`, so the table itself is the listing; that makes
//! this command self-updating — a new `COMMAND_TABLE` entry appears here
//! without touching the handler.

use crate::slash::parser::COMMAND_TABLE;
use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct CommandsHandler;

impl SlashHandler for CommandsHandler {
    fn description(&self) -> &'static str {
        "List all registered commands with their description"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        let mut lines = vec![format!("Registered commands ({}):", canonical_count())];
        for (trigger, meta) in canonical_entries() {
            let hint = meta.args_hint.map(|h| format!(" {h}")).unwrap_or_default();
            let aliases = if meta.aliases.is_empty() {
                String::new()
            } else {
                format!("  (aliases: {})", meta.aliases.join(", "))
            };
            lines.push(format!(
                "  /{trigger}{hint}\n      {}{aliases}",
                meta.description
            ));
        }
        CommandResult::Lines(lines)
    }
}

/// `COMMAND_TABLE` entries whose key equals their canonical name.
///
/// Aliases get their own row in the table (so `/cls` completes), but listing
/// them again here would show one command twice.
fn canonical_entries()
-> impl Iterator<Item = (&'static str, &'static crate::slash::parser::CommandMeta)> {
    COMMAND_TABLE
        .iter()
        .filter(|(trigger, meta)| *trigger == meta.canonical)
        .map(|(trigger, meta)| (*trigger, meta))
}

fn canonical_count() -> usize {
    canonical_entries().count()
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

    fn ctx() -> CommandContext {
        CommandContext {
            args: String::new(),
            command: SlashCommand::Commands,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
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
    fn lists_every_canonical_command_exactly_once() {
        let out = lines(CommandsHandler.execute(ctx()));
        let mut matched = 0;
        for (trigger, _) in canonical_entries() {
            let needle = format!("/{trigger}");
            // Each command is emitted as "  /<trigger>[ hint]". Compare the
            // whole first token: a prefix test would count "/model" as "/mode".
            let head = out
                .iter()
                .filter(|l| l.split_whitespace().next() == Some(needle.as_str()))
                .count();
            assert_eq!(
                head,
                1,
                "expected exactly one row for /{trigger}:\n{}",
                out.join("\n")
            );
            matched += 1;
        }
        assert_eq!(matched, canonical_count());
        assert!(
            out[0].contains(&canonical_count().to_string()),
            "header must state the count: {:?}",
            out[0]
        );
    }

    #[test]
    fn aliases_are_not_duplicated_as_their_own_command() {
        let out = lines(CommandsHandler.execute(ctx()));
        // "/cls" must be reported as an alias of /clear, not as a separate row.
        assert!(
            out.iter()
                .any(|l| l.contains("/clear") && l.contains("aliases: cls"))
        );
    }

    #[test]
    fn every_completable_trigger_is_reachable_from_the_listing() {
        // Aliases are deliberately NOT rows of their own (that would show one
        // command twice), so the invariant is: a completable trigger is either
        // its own row or listed as an alias of the command it points at.
        let out = lines(CommandsHandler.execute(ctx()));
        let text = out.join("\n");
        for trigger in crate::slash::parser::SlashCommandParser::completions("") {
            let own_row = out
                .iter()
                .any(|l| l.trim_start().starts_with(&format!("/{trigger}")));
            let as_alias = text.contains(&format!("aliases: {trigger}"))
                || text.contains(&format!(", {trigger}"))
                || text.contains(&format!("{trigger},"));
            assert!(
                own_row || as_alias,
                "/{trigger} is completable but unreachable from /commands:\n{text}"
            );
        }
    }
}
