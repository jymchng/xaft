//! /init command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct InitHandler;

impl SlashHandler for InitHandler {
    fn description(&self) -> &'static str {
        "Generate AGENTS.md project memory file"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/init: not yet connected to runtime.".into()])
    }
}
