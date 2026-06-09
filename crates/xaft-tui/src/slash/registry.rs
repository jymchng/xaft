//! Slash command registry — maps SlashCommand variants to handlers.

use std::collections::HashMap;
use std::mem::Discriminant;
use std::sync::Arc;

use super::{CommandContext, CommandResult, SlashCommand};

// ── SlashHandler trait ────────────────────────────────────────────────────────

/// Implemented by every slash command handler.
pub trait SlashHandler: Send + Sync {
    /// Human-readable description shown in /help output.
    fn description(&self) -> &'static str;

    /// Optional argument hint, e.g. `"[session-id]"`.
    fn args_hint(&self) -> Option<&'static str> {
        None
    }

    /// True if the handler needs async execution.
    fn is_async(&self) -> bool {
        false
    }

    /// Execute synchronously. Called only when `is_async() == false`.
    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let _ = ctx;
        CommandResult::Error("not implemented".into())
    }

    /// Execute asynchronously. Called only when `is_async() == true`.
    fn execute_boxed_async(
        &self,
        ctx: CommandContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandResult> + Send + 'static>> {
        let _ = ctx;
        Box::pin(async { CommandResult::Error("async not implemented".into()) })
    }
}

// ── SlashCommandRegistry ──────────────────────────────────────────────────────

/// Registry mapping SlashCommand discriminants to handlers.
pub struct SlashCommandRegistry {
    handlers: HashMap<Discriminant<SlashCommand>, Arc<dyn SlashHandler>>,
}

impl SlashCommandRegistry {
    /// Create a new builder.
    pub fn builder() -> SlashCommandRegistryBuilder {
        SlashCommandRegistryBuilder {
            handlers: HashMap::new(),
        }
    }

    /// Look up the handler for a command.
    ///
    /// # Panics
    ///
    /// Panics if no handler is registered for the variant. This is a
    /// programming error detectable at startup or in tests.
    pub fn get(&self, cmd: &SlashCommand) -> Arc<dyn SlashHandler> {
        self.handlers
            .get(&std::mem::discriminant(cmd))
            .unwrap_or_else(|| {
                panic!(
                    "SlashCommandRegistry: no handler registered for {:?} — \
                     every SlashCommand variant must have a registered handler",
                    cmd
                )
            })
            .clone()
    }

    /// Check that every variant in the example list has a handler.
    /// Used in tests and debug builds.
    pub fn validate(&self) {
        for variant in SlashCommand::all_discriminants() {
            assert!(
                self.handlers.contains_key(&variant),
                "SlashCommandRegistry: missing handler for a SlashCommand variant"
            );
        }
    }
}

// ── SlashCommandRegistryBuilder ───────────────────────────────────────────────

pub struct SlashCommandRegistryBuilder {
    handlers: HashMap<Discriminant<SlashCommand>, Arc<dyn SlashHandler>>,
}

impl SlashCommandRegistryBuilder {
    /// Register a handler for the variant identified by `example`.
    pub fn register<H>(mut self, example: SlashCommand, handler: H) -> Self
    where
        H: SlashHandler + 'static,
    {
        self.handlers
            .insert(std::mem::discriminant(&example), Arc::new(handler));
        self
    }

    /// Build the registry.
    pub fn build(self) -> SlashCommandRegistry {
        SlashCommandRegistry {
            handlers: self.handlers,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::commands::build_default_registry;

    #[test]
    fn registry_all_slash_command_variants_have_handlers() {
        let registry = build_default_registry();
        // Access every variant — must not panic.
        for cmd in SlashCommand::all_example_variants() {
            let _ = registry.get(&cmd);
        }
    }

    #[test]
    #[should_panic(expected = "no handler registered")]
    fn registry_missing_handler_panics_on_get() {
        // Build a registry without registering any handlers, then try to get Help.
        let registry = SlashCommandRegistry::builder().build();
        let _ = registry.get(&SlashCommand::Help);
    }
}
