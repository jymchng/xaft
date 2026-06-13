//! Slash-command palette trigger handler.
//!
//! Migrated from `AppState::refresh_slash_palette()` and the slash-palette
//! section of `handle_key()` in `state.rs`.

use std::sync::Arc;

use crate::slash::parser::{COMMAND_TABLE, SlashCommandParser};
use crate::slash::registry::SlashCommandRegistry;
use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

/// Trigger handler for `/` slash-command palette completion.
pub struct SlashCommandTriggerHandler {
    registry: Arc<SlashCommandRegistry>,
}

impl SlashCommandTriggerHandler {
    /// Create a new handler backed by the given slash registry.
    pub fn new(registry: Arc<SlashCommandRegistry>) -> Self {
        Self { registry }
    }
}

impl TriggerHandler for SlashCommandTriggerHandler {
    fn trigger_char(&self) -> char {
        '/'
    }

    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        // `ctx.prefix` is everything after the leading '/'.
        let candidates = SlashCommandParser::completions(ctx.prefix);
        candidates
            .iter()
            .map(|&trigger| {
                let meta = COMMAND_TABLE
                    .iter()
                    .find(|(k, _)| k == &trigger)
                    .map(|(_, m)| m);
                let description = meta.map(|m| m.description).unwrap_or("");
                let args_hint = meta.and_then(|m| m.args_hint);

                // `display` mirrors the pre-refactor SlashPaletteRow rendering:
                // trigger text only (description/args rendered by draw_trigger_dropdown).
                MatchItem {
                    display: trigger.to_string(),
                    insert: format!("/{} ", trigger),
                    hint: args_hint.map(String::from),
                    kind: MatchKind::Command,
                }
            })
            .collect()
    }

    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    /// The user can submit `/clear` even if it partially matches `/clearall`.
    fn allows_free_text(&self) -> bool {
        true
    }

    fn max_visible(&self) -> usize {
        8
    }
}

// Keep registry field alive (Arc clone is cheap, and it prevents dead_code lint)
impl Drop for SlashCommandTriggerHandler {
    fn drop(&mut self) {
        let _ = &self.registry;
    }
}
