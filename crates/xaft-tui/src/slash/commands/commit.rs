//! /commit command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct CommitHandler;

impl SlashHandler for CommitHandler {
    fn description(&self) -> &'static str {
        "Commit session changes to git"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[message]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/commit: not yet connected to runtime.".into()])
    }
}
