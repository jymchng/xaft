//! /rewind command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct RewindHandler;

impl SlashHandler for RewindHandler {
    fn description(&self) -> &'static str {
        "Roll back transcript to message N"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[N]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/rewind: not yet connected to runtime.".into()])
    }
}
