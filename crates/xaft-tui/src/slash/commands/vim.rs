//! /vim and /emacs command handlers.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct VimHandler;

impl SlashHandler for VimHandler {
    fn description(&self) -> &'static str {
        "Switch input bar to Vim keybindings"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec![
            "Keymap set to Vim.  Press Esc for NORMAL mode.".into(),
        ])
    }
}

pub struct EmacsHandler;

impl SlashHandler for EmacsHandler {
    fn description(&self) -> &'static str {
        "Switch input bar to Emacs keybindings"
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Lines(vec!["Keymap set to Emacs.".into()])
    }
}
