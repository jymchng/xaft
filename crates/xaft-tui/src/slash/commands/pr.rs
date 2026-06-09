//! /pr command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct PrHandler;

impl SlashHandler for PrHandler {
    fn description(&self) -> &'static str {
        "Create a GitHub pull request"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[title]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/pr: not yet connected to runtime.".into()])
    }
}
