//! Prompt state and formatting for the input line.

use crate::input_bar::Cursor;
use crate::state::AppState;
use crate::trigger::ActiveTriggerSnapshot;

// ── PromptState ───────────────────────────────────────────────────────────────

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
    /// Snapshot of the active trigger dropdown for this render frame,
    /// or `None` when no trigger is open.
    ///
    /// Replaces the former `autocomplete` and `slash_palette` fields.
    pub active_trigger: Option<ActiveTriggerSnapshot>,
    /// Whether an interactive menu overlay is currently active.
    pub menu_active: bool,
    /// ANSI-coloured badge for the active mode, e.g. `"\x1b[33m[PLAN]\x1b[0m"`.
    /// `None` in Auto mode (badge hidden to keep the prompt clean).
    pub mode_badge: Option<String>,
    /// Permanent footer rendered below the prompt block on every frame.
    /// One-shot switch notification OR standing shift+tab hint.
    pub mode_footer: String,
}

/// Build a `PromptState` from the current `AppState`.
pub fn build_prompt(state: &mut AppState) -> PromptState {
    let total = state.input_bar.line_count();
    let max_vis = state.input_bar.max_visible_rows() as usize;
    let cursor_row = state.input_bar.cursor().row;

    // The input_bar tracks scroll_top in *logical* lines (max 8 lines visible).
    // The renderer caps *visual* rows at MAX_VISIBLE_ROWS. When logical lines
    // soft-wrap, the two windows diverge: the cursor's visual row can exceed
    // MAX_VISIBLE_ROWS even though the cursor is "visible" in logical-line terms.
    // To prevent that divergence, clamp scroll_top so the cursor's visual row
    // (relative to scroll_top) is within MAX_VISIBLE_ROWS.
    let wrap_width = (state.terminal_size.0 as usize)
        .saturating_sub(crate::input_bar::PREFIX_WIDTH)
        .max(1);
    let lines = state.input_bar.lines();
    let mut scroll_top = state.input_bar.scroll_top();
    loop {
        let vis_row = cursor_vis_row_from(lines, cursor_row, scroll_top, wrap_width);
        if vis_row < crate::input_bar::MAX_VISIBLE_ROWS as usize {
            break;
        }
        scroll_top = scroll_top.saturating_add(1);
        // Safety: if scroll_top has caught up to cursor_row, stop to avoid
        // an infinite loop (degenerate case where a single line wraps > MAX_VISIBLE_ROWS).
        if scroll_top >= cursor_row {
            break;
        }
    }
    // Also ensure the cursor isn't above the visible window.
    if cursor_row < scroll_top {
        scroll_top = cursor_row;
    }

    let visible = total.min(max_vis).max(1);
    let hidden_above = scroll_top;
    let visible_end = scroll_top + visible;
    let hidden_below = total.saturating_sub(visible_end);

    // Build unified trigger snapshot from AppState::active_trigger.
    let active_trigger = state.active_trigger.as_ref().and_then(|at| {
        let handler = state.trigger_registry.get(at.scan.trigger_char)?;
        let hint = at.selected_item().and_then(|item| item.hint.clone());
        Some(ActiveTriggerSnapshot {
            trigger_char: at.scan.trigger_char,
            items: at.items.clone(),
            selected: at.selected,
            scroll_top: at.scroll_top,
            max_visible: handler.max_visible(),
            hint,
        })
    });

    PromptState {
        lines: lines.to_vec(),
        cursor: state.input_bar.cursor(),
        scroll_top,
        agent_active: state.phase.is_active(),
        is_empty: state.input_bar.is_empty(),
        hidden_above,
        hidden_below,
        active_trigger,
        menu_active: state.menu_driver.is_active(),
        mode_badge: {
            // Show an ANSI badge for every mode except auto (hidden to keep
            // the prompt clean). Reflects the active mode's label + colour.
            let mode = state.mode_manager.active();
            if mode.name == "auto" {
                None
            } else {
                Some(mode.ansi_badge())
            }
        },
        mode_footer: {
            let notification = state.mode_notification.take();
            let cancel_requested = state.cancel_requested;
            let mode_name = state.mode_manager.active().name.clone();
            let mode_label = state.mode_manager.active().label.clone();
            let mode_desc = state.mode_manager.active().description.clone();
            if let Some(n) = notification {
                // Explicit one-shot notification (e.g. resume hint on second Ctrl+C).
                n
            } else if cancel_requested {
                "Press CTRL+C again to exit".into()
            } else if mode_name == "auto" {
                "⏵⏵ Auto  (shift+tab to cycle)".into()
            } else {
                let preview_len = mode_desc
                    .char_indices()
                    .nth(50)
                    .map(|(i, _)| i)
                    .unwrap_or(mode_desc.len());
                format!(
                    "⏵⏵ [{}] {} — {}  (shift+tab to cycle)",
                    mode_label,
                    mode_name,
                    &mode_desc[..preview_len]
                )
            }
        },
    }
}

/// Compute how many visual rows precede `cursor_row` starting from `scroll_top`.
///
/// Used by `build_prompt` to find a scroll_top that keeps the cursor's visual
/// row within `MAX_VISIBLE_ROWS` even when lines soft-wrap.
fn cursor_vis_row_from(
    lines: &[String],
    cursor_row: usize,
    scroll_top: usize,
    wrap_width: usize,
) -> usize {
    let mut vis = 0usize;
    for (i, line) in lines.iter().enumerate().skip(scroll_top) {
        if i == cursor_row {
            return vis;
        }
        vis += crate::input_bar::wrap_rows(line, wrap_width).max(1);
    }
    vis
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_bar::{InputBar, MAX_VISIBLE_ROWS};
    use crate::state::AppState;

    // Helper: build a minimal AppState with lines set and cursor at the given row.
    fn state_with_lines(lines: &[&str], cursor_row: usize) -> AppState {
        let mut s = AppState::new("");
        // Reset the input_bar to match what we need.
        let text = lines.join("\n");
        s.input_bar.set_text(&text);
        // Move cursor to the desired row (end of that row).
        let col = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
        s.input_bar.set_cursor(cursor_row, col);
        s
    }

    // ── cursor_vis_row_from ────────────────────────────────────────────────────

    #[test]
    fn vis_row_zero_when_cursor_at_start() {
        let lines: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        // Cursor on first line → 0 visual rows precede it from scroll_top=0.
        assert_eq!(cursor_vis_row_from(&lines, 0, 0, 40), 0);
    }

    #[test]
    fn vis_row_counts_preceding_lines() {
        let lines: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        // cursor_row=2, each line is 1 visual row → 2 rows precede.
        assert_eq!(cursor_vis_row_from(&lines, 2, 0, 40), 2);
    }

    #[test]
    fn vis_row_accounts_for_soft_wrap() {
        // A 50-char line wraps to 2 rows at wrap_width=40.
        let long = "A".repeat(50);
        let lines: Vec<String> = vec![long, "b".into()];
        // cursor_row=1 → 2 rows (the wrapped line 0) precede it.
        assert_eq!(cursor_vis_row_from(&lines, 1, 0, 40), 2);
    }

    #[test]
    fn vis_row_respects_scroll_top() {
        let lines: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        // scroll_top=1 → iteration starts at line 1; cursor_row=2 has 1 row before it.
        assert_eq!(cursor_vis_row_from(&lines, 2, 1, 40), 1);
    }

    // ── build_prompt visual-row-aware scroll ──────────────────────────────────

    #[test]
    fn build_prompt_cursor_within_max_visible_rows_short_lines() {
        // With short lines (1 visual row each), cursor_vis_row < MAX_VISIBLE_ROWS.
        let mut s = state_with_lines(&["a", "b", "c"], 2);
        let p = build_prompt(&mut s);
        let wrap_width = (s.terminal_size.0 as usize)
            .saturating_sub(crate::input_bar::PREFIX_WIDTH)
            .max(1);
        let vis_row = cursor_vis_row_from(&p.lines, p.cursor.row, p.scroll_top, wrap_width);
        assert!(
            vis_row < MAX_VISIBLE_ROWS as usize,
            "cursor visual row {vis_row} must be < MAX_VISIBLE_ROWS {MAX_VISIBLE_ROWS}"
        );
    }

    #[test]
    fn build_prompt_adjusts_scroll_for_wrapped_cursor() {
        // Lines that are long enough to soft-wrap.  If the cursor's logical
        // line sits beyond max_in_view after wrapping, build_prompt must
        // advance scroll_top to bring the cursor into view.
        // terminal_size = (80, 40) → wrap_width ≈ 78.
        // A 79-char line wraps to 2 visual rows.
        let long = "A".repeat(79);
        let lines: Vec<&str> = std::iter::repeat(long.as_str()).take(5).collect();
        let mut s = state_with_lines(&lines, 4); // cursor on last of 5 wrapped lines
        s.terminal_size = (80, 40);
        let p = build_prompt(&mut s);
        let wrap_width = (s.terminal_size.0 as usize)
            .saturating_sub(crate::input_bar::PREFIX_WIDTH)
            .max(1);
        let vis_row = cursor_vis_row_from(&p.lines, p.cursor.row, p.scroll_top, wrap_width);
        assert!(
            vis_row < MAX_VISIBLE_ROWS as usize,
            "cursor vis_row {vis_row} must be < {MAX_VISIBLE_ROWS} after scroll adjustment"
        );
    }

    #[test]
    fn build_prompt_scroll_top_zero_when_fits() {
        // When all lines fit within MAX_VISIBLE_ROWS, scroll_top must be 0.
        let mut s = state_with_lines(&["a", "b"], 0);
        let p = build_prompt(&mut s);
        assert_eq!(p.scroll_top, 0, "no scroll needed for 2 short lines");
    }

    #[test]
    fn build_prompt_hidden_above_matches_scroll_top() {
        // hidden_above should equal scroll_top.
        let long = "A".repeat(20);
        let lines: Vec<&str> = std::iter::repeat(long.as_str()).take(10).collect();
        let mut s = state_with_lines(&lines, 9);
        s.terminal_size = (80, 40);
        let p = build_prompt(&mut s);
        assert_eq!(
            p.hidden_above, p.scroll_top,
            "hidden_above must equal scroll_top"
        );
    }

    #[test]
    fn build_prompt_active_trigger_none_by_default() {
        let mut s = state_with_lines(&["hello"], 0);
        let p = build_prompt(&mut s);
        assert!(p.active_trigger.is_none(), "no trigger active by default");
    }
}
