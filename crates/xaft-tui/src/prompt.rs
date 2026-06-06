//! Prompt state and formatting for the input line.

use crate::input_bar::Cursor;
use crate::state::AppState;

/// Current state of the user input prompt.
#[derive(Debug, Clone, Default)]
pub struct PromptState {
    /// Multi-line buffer (one entry per logical line, no trailing empty).
    pub lines: Vec<String>,
    /// Logical cursor within `lines`.
    pub cursor: Cursor,
    /// First visible line index (for scroll viewport).
    pub scroll_top: usize,
    /// Whether an agent is actively running (shows a subtle indicator).
    pub agent_active: bool,
    /// Whether the input bar is empty (drives the ephemeral hint line).
    pub is_empty: bool,
}

/// Build a `PromptState` from the current `AppState`.
pub fn build_prompt(state: &AppState) -> PromptState {
    PromptState {
        lines: state.input_bar.lines().to_vec(),
        cursor: state.input_bar.cursor(),
        scroll_top: state.input_bar.scroll_top(),
        agent_active: state.phase.is_active(),
        is_empty: state.input_bar.is_empty(),
    }
}

/// Format the visual prompt line for display (single-line, legacy).
///
/// Returns something like `"❯ user typing here"` or `"❯ "` (empty input).
pub fn format_prompt_line(p: &PromptState) -> String {
    format!("❯ {}", p.lines.join("\n"))
}
