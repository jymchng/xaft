//! /permissions command handler.

use std::sync::Arc;

use xaft_config::XaftConfig;

use crate::slash::registry::SlashHandler;
use crate::slash::table::CliTable;
use crate::slash::{CommandContext, CommandResult};

pub struct PermissionsHandler {
    config: Arc<XaftConfig>,
}

impl PermissionsHandler {
    pub fn new(config: Arc<XaftConfig>) -> Self {
        Self { config }
    }
}

impl SlashHandler for PermissionsHandler {
    fn description(&self) -> &'static str {
        "Open the per-tool permission editor"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        // The config doesn't have a permissions field yet, so show what we can.
        let mut table = CliTable::new(["TOOL", "RULE", "SOURCE"]);

        // Built-in defaults shown when no config permissions defined.
        table.push_row(["bash_exec", "confirm", "built-in default"]);
        table.push_row(["write_file", "allow", "built-in default"]);
        table.push_row(["read_file", "allow", "built-in default"]);
        table.push_row(["delete_file", "deny", "built-in default"]);

        let lines = table.render(ctx.terminal_cols);
        CommandResult::StyledLines(lines)
    }
}
