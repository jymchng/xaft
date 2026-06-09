//! /diff command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct DiffHandler;

impl SlashHandler for DiffHandler {
    fn description(&self) -> &'static str {
        "Show working-tree diff against session base"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[path]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/diff: not yet connected to runtime.".into()])
    }
}
