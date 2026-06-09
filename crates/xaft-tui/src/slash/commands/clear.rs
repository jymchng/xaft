//! /clear command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ClearHandler;

impl SlashHandler for ClearHandler {
    fn description(&self) -> &'static str {
        "Reset the current session transcript"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["Session display cleared.".into()])
    }
}
