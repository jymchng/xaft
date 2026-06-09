//! /model command handler.

use std::sync::{Arc, RwLock};

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct ModelHandler {
    /// Shared current model name.
    current_model: Arc<RwLock<String>>,
}

impl ModelHandler {
    pub fn new() -> Self {
        Self {
            current_model: Arc::new(RwLock::new(String::new())),
        }
    }

    pub fn with_shared(current_model: Arc<RwLock<String>>) -> Self {
        Self { current_model }
    }

    pub fn model_override(&self) -> Option<String> {
        let m = self.current_model.read().ok()?;
        if m.is_empty() { None } else { Some(m.clone()) }
    }
}

impl Default for ModelHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl SlashHandler for ModelHandler {
    fn description(&self) -> &'static str {
        "Switch the LLM model mid-session"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("<name>")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let name = ctx.args.trim().to_string();
        if name.is_empty() {
            return CommandResult::Error(
                "/model requires a model name.\n  Usage: /model <name>".into(),
            );
        }
        if let Ok(mut m) = self.current_model.write() {
            *m = name.clone();
        }
        CommandResult::Lines(vec![
            format!("Model switched to: {name}"),
            "Note: the change takes effect on the next agent turn.".into(),
        ])
    }
}
