//! /agents command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct AgentsHandler;

impl SlashHandler for AgentsHandler {
    fn description(&self) -> &'static str {
        "List loaded agents and their tool counts"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["No agent registry connected.".into()])
    }
}
