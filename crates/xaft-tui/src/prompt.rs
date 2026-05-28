//! Prompt state and formatting for the input line.

use crate::state::AppState;

/// Current state of the user input prompt.
#[derive(Debug, Clone, Default)]
pub struct PromptState {
    /// Text currently typed by the user.
    pub buffer: String,
    /// Whether an agent is actively running (shows a subtle indicator).
    pub agent_active: bool,
}

/// Build a `PromptState` from the current `AppState`.
pub fn build_prompt(state: &AppState) -> PromptState {
    PromptState {
        buffer: state.input_buffer.clone(),
        agent_active: state.phase.is_active(),
    }
}

/// Format the visual prompt line for display.
///
/// Returns something like `"❯ user typing here"` or `"❯ "` (empty input).
pub fn format_prompt_line(p: &PromptState) -> String {
    format!("❯ {}", p.buffer)
}
