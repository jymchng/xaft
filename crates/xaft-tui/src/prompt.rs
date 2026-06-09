//! Prompt state and formatting for the input line.

use crate::input_bar::Cursor;
use crate::state::AppState;

// ── Slash palette snapshot ────────────────────────────────────────────────────

/// One row in the slash-command autocomplete palette overlay.
#[derive(Debug, Clone)]
pub struct SlashPaletteRow {
    pub trigger: String,
    pub description: String,
    pub args_hint: Option<String>,
}

/// Renderer-side snapshot of the slash palette (no `Arc`, no `'static`).
#[derive(Debug, Clone)]
pub struct SlashPaletteSnapshot {
    pub rows: Vec<SlashPaletteRow>,
    /// Index of the highlighted row (into `rows`).
    pub selected: usize,
    /// First visible row index (scroll offset).
    pub scroll_top: usize,
}

// ── @-mention autocomplete ────────────────────────────────────────────────────

/// A filtered autocomplete list shown below the input border when the user
/// is typing an `@`-mention path.
#[derive(Debug, Clone)]
pub struct AutocompleteDropdown {
    /// The full path text typed after `@` (e.g., `./src/ma`).
    pub prefix: String,
    /// The directory component of `prefix` — the text up to and including the
    /// last `/`. Empty string means the workspace root.
    pub dir_prefix: String,
    /// Bare entry names within `dir_prefix` that match the filename part.
    /// Directory entries have a trailing `/`.
    pub candidates: Vec<String>,
    /// Index into `candidates` that is currently highlighted (0-based).
    pub selected: usize,
}

impl AutocompleteDropdown {
    /// The highlighted candidate, if any.
    pub fn selected_candidate(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(String::as_str)
    }
}

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
    /// Number of lines scrolled out of view above the visible region
    /// (drives the `▲ N more lines above` indicator). `0` when nothing is
    /// scrolled off.
    pub hidden_above: usize,
    /// Number of lines scrolled out of view below the visible region.
    /// `0` when the cursor is on the last line.
    pub hidden_below: usize,
    /// Active @-mention autocomplete dropdown. `None` when the cursor is
    /// not inside a `@<path>` token or the workspace has no matches.
    pub autocomplete: Option<AutocompleteDropdown>,
    /// Slash-command autocomplete overlay. `None` when not typing a `/` command.
    pub slash_palette: Option<SlashPaletteSnapshot>,
}

/// Build a `PromptState` from the current `AppState`.
pub fn build_prompt(state: &AppState) -> PromptState {
    let total = state.input_bar.line_count();
    let max_vis = state.input_bar.max_visible_rows() as usize;
    let scroll_top = state.input_bar.scroll_top();
    let cursor_row = state.input_bar.cursor().row;
    let visible = total.min(max_vis).max(1);
    let hidden_above = scroll_top;
    let visible_end = scroll_top + visible;
    let hidden_below = total.saturating_sub(visible_end);
    // Suppress the indicator when there's nothing useful to communicate.
    let _ = cursor_row;
    PromptState {
        lines: state.input_bar.lines().to_vec(),
        cursor: state.input_bar.cursor(),
        scroll_top,
        agent_active: state.phase.is_active(),
        is_empty: state.input_bar.is_empty(),
        hidden_above,
        hidden_below,
        autocomplete: state.mention_autocomplete.clone(),
        slash_palette: state.slash_palette.as_ref().map(|p| SlashPaletteSnapshot {
            rows: p
                .candidates
                .iter()
                .enumerate()
                .map(|(i, t)| SlashPaletteRow {
                    trigger: t.to_string(),
                    description: p.descriptions.get(i).copied().unwrap_or("").to_string(),
                    args_hint: p
                        .args_hints
                        .get(i)
                        .copied()
                        .flatten()
                        .map(|s| s.to_string()),
                })
                .collect(),
            selected: p.selected,
            scroll_top: p.scroll_top,
        }),
    }
}

/// Format the visual prompt line for display (single-line, legacy).
///
/// Returns something like `"❯ user typing here"` or `"❯ "` (empty input).
pub fn format_prompt_line(p: &PromptState) -> String {
    format!("❯ {}", p.lines.join("\n"))
}

/// Render the `▲ N more lines above` indicator (or `None` if `n == 0`).
pub fn scroll_indicator_above(p: &PromptState) -> Option<String> {
    if p.hidden_above == 0 {
        None
    } else if p.hidden_above == 1 {
        Some("▲ 1 more line above — Shift+↑ to scroll".to_string())
    } else {
        Some(format!(
            "▲ {} more lines above — Shift+↑ to scroll",
            p.hidden_above
        ))
    }
}

/// Render the empty-buffer hint line shown to the user.
///
/// Reminds the user of the available newline-insertion keybindings.
pub fn empty_buffer_hint() -> &'static str {
    "Shift+Enter / Alt+Enter / Ctrl+J → newline   ·   Enter → send"
}
