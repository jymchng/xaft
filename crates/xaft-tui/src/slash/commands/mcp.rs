//! /mcp command handler.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct McpHandler;

impl SlashHandler for McpHandler {
    fn description(&self) -> &'static str {
        "Show MCP server status"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["No MCP registry connected.".into()])
    }
}
