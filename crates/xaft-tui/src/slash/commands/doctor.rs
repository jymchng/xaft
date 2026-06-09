//! /doctor command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct DoctorHandler;

impl SlashHandler for DoctorHandler {
    fn description(&self) -> &'static str {
        "Run configuration and connectivity diagnostics"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/doctor: not yet connected to runtime.".into()])
    }
}
