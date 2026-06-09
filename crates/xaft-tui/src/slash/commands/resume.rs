//! /resume command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ResumeHandler;

impl SlashHandler for ResumeHandler {
    fn description(&self) -> &'static str {
        "Resume a prior session"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[session-id]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/resume: not yet connected to runtime.".into()])
    }
}
