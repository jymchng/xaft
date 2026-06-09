//! /compact command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct CompactHandler;

impl SlashHandler for CompactHandler {
    fn description(&self) -> &'static str {
        "Manually trigger context compaction"
    }

    fn is_async(&self) -> bool {
        false // Stub — returns a message for now.
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/compact: not yet connected to runtime.".into()])
    }
}
