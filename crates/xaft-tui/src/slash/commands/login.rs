//! /login and /logout command handlers.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct LoginHandler;

impl SlashHandler for LoginHandler {
    fn description(&self) -> &'static str {
        "Authenticate (OAuth or API key)"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[provider]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/login: not yet connected to runtime.".into()])
    }
}

pub struct LogoutHandler;

impl SlashHandler for LogoutHandler {
    fn description(&self) -> &'static str {
        "Remove stored credentials"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[provider]")
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["/logout: not yet connected to runtime.".into()])
    }
}
