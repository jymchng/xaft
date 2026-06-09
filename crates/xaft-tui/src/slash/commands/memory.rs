//! /memory command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct MemoryHandler;

impl SlashHandler for MemoryHandler {
    fn description(&self) -> &'static str {
        "Browse the memory store"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[query]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["No memory store connected.".into()])
    }
}
