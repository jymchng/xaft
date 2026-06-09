//! Multi-line input widget for the xaft TUI.
//!
//! See `prds/29a_multi_line_input_design.md` for the full design.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const MAX_VISIBLE_ROWS: u16 = 8;
pub const PREFIX_WIDTH: usize = 2;
pub const DEFAULT_MIN_WRAP_WIDTH: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    Submit(String),
    BufferChanged,
    CursorMoved,
    NoOp,
}

#[derive(Debug, Clone)]
pub struct InputBar {
    lines: Vec<String>,
    cursor: Cursor,
    scroll_top: usize,
    max_visible_rows: u16,
    term_cols: u16,
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new(120)
    }
}

impl InputBar {
    pub fn new(term_cols: u16) -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Cursor::default(),
            scroll_top: 0,
            max_visible_rows: MAX_VISIBLE_ROWS,
            term_cols,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn scroll_top(&self) -> usize {
        self.scroll_top
    }
    pub fn max_visible_rows(&self) -> u16 {
        self.max_visible_rows
    }
    pub fn term_cols(&self) -> u16 {
        self.term_cols
    }

    /// Verify all internal invariants. See module-level [`invariants_hold`].
    pub fn check_invariants(&self) -> Result<(), String> {
        invariants_hold(self)
    }

    pub fn wrap_width(&self) -> usize {
        (self.term_cols as usize)
            .saturating_sub(PREFIX_WIDTH)
            .max(DEFAULT_MIN_WRAP_WIDTH)
    }

    pub fn visible_rows(&self) -> u16 {
        let wrapped: usize = self
            .lines
            .iter()
            .map(|l| wrap_rows(l, self.wrap_width()))
            .sum();
        wrapped.clamp(1, self.max_visible_rows as usize) as u16
    }

    pub fn on_resize(&mut self, term_cols: u16) {
        self.term_cols = term_cols;
        self.clamp_scroll();
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = Cursor::default();
        self.scroll_top = 0;
    }

    /// Programmatically place the cursor at `(row, col)`. Bytes are clamped
    /// to the line length and snapped to the nearest char boundary.
    /// Returns `true` if the cursor moved.
    pub fn set_cursor(&mut self, row: usize, col: usize) -> bool {
        let new_row = row.min(self.lines.len().saturating_sub(1));
        let new_col = snap_to_char_boundary(&self.lines[new_row], col);
        let changed = self.cursor.row != new_row || self.cursor.col != new_col;
        self.cursor.row = new_row;
        self.cursor.col = new_col;
        self.clamp_scroll();
        changed
    }

    pub fn set_text(&mut self, s: &str) -> InputAction {
        let new_lines: Vec<String> = if s.is_empty() {
            vec![String::new()]
        } else {
            s.split('\n')
                .map(|l| strip_carriage_returns(l))
                .map(String::from)
                .collect()
        };
        self.lines = new_lines;
        self.cursor = Cursor {
            row: self.lines.len().saturating_sub(1),
            col: self.lines.last().map(|l| l.len()).unwrap_or(0),
        };
        self.clamp_scroll();
        InputAction::BufferChanged
    }

    pub fn insert_str(&mut self, s: &str) -> InputAction {
        if s.is_empty() {
            return InputAction::NoOp;
        }
        let normalized = strip_carriage_returns(s);
        let mut first = true;
        for segment in normalized.split('\n') {
            if !first {
                self.insert_newline_internal();
            }
            for c in segment.chars() {
                self.insert_char_internal(c);
            }
            first = false;
        }
        self.clamp_scroll();
        InputAction::BufferChanged
    }

    pub fn submit(&mut self) -> InputAction {
        let payload = self.text().trim().to_string();
        if payload.is_empty() {
            return InputAction::NoOp;
        }
        self.clear();
        InputAction::Submit(payload)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, KeyModifiers::SHIFT) | (KeyCode::Enter, KeyModifiers::ALT) => {
                self.insert_newline_internal();
                self.clamp_scroll();
                InputAction::BufferChanged
            }
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.insert_newline_internal();
                self.clamp_scroll();
                InputAction::BufferChanged
            }
            (KeyCode::Enter, KeyModifiers::NONE) | (KeyCode::Enter, KeyModifiers::CONTROL) => {
                self.submit()
            }
            (KeyCode::Enter, _) => {
                self.insert_newline_internal();
                self.clamp_scroll();
                InputAction::BufferChanged
            }
            (KeyCode::Backspace, _) => {
                let changed = self.backspace_internal();
                self.clamp_scroll();
                if changed {
                    InputAction::BufferChanged
                } else {
                    InputAction::NoOp
                }
            }
            (KeyCode::Delete, _) => {
                let changed = self.delete_forward_internal();
                self.clamp_scroll();
                if changed {
                    InputAction::BufferChanged
                } else {
                    InputAction::NoOp
                }
            }
            (KeyCode::Left, _) => {
                self.cursor_left_internal();
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Right, _) => {
                self.cursor_right_internal();
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Up, _) => {
                self.cursor_up_internal();
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Down, _) => {
                self.cursor_down_internal();
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Home, _) => {
                self.cursor.line_start();
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::End, _) => {
                self.cursor.line_end(&self.lines);
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.cursor.line_start();
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cursor.line_end(&self.lines);
                self.clamp_scroll();
                InputAction::CursorMoved
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let changed = self.clear_to_line_start_internal();
                self.clamp_scroll();
                if changed {
                    InputAction::BufferChanged
                } else {
                    InputAction::NoOp
                }
            }
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                let changed = self.clear_to_line_end_internal();
                self.clamp_scroll();
                if changed {
                    InputAction::BufferChanged
                } else {
                    InputAction::NoOp
                }
            }
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                let changed = self.delete_word_backward_internal();
                self.clamp_scroll();
                if changed {
                    InputAction::BufferChanged
                } else {
                    InputAction::NoOp
                }
            }
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                self.insert_char_internal(c);
                self.clamp_scroll();
                InputAction::BufferChanged
            }
            _ => InputAction::NoOp,
        }
    }
}

impl Cursor {
    pub fn line_start(&mut self) {
        self.col = 0;
    }
    pub fn line_end(&mut self, lines: &[String]) {
        if let Some(l) = lines.get(self.row) {
            self.col = l.len();
        }
    }
}

impl InputBar {
    fn insert_char_internal(&mut self, c: char) {
        let line = &mut self.lines[self.cursor.row];
        let col = snap_to_char_boundary(line, self.cursor.col);
        self.cursor.col = col;
        line.insert(col, c);
        self.cursor.col += c.len_utf8();
    }

    fn insert_newline_internal(&mut self) {
        let line = &mut self.lines[self.cursor.row];
        let col = snap_to_char_boundary(line, self.cursor.col);
        self.cursor.col = col;
        let tail = line.split_off(col);
        self.lines.insert(self.cursor.row + 1, tail);
        self.cursor.row += 1;
        self.cursor.col = 0;
    }

    pub fn insert_newline(&mut self) {
        self.insert_newline_internal();
        self.clamp_scroll();
    }

    pub fn first_line_text(&self) -> &str {
        self.lines.first().map(|s| s.as_str()).unwrap_or("")
    }

    fn backspace_internal(&mut self) -> bool {
        if self.cursor.col > 0 {
            let line = &mut self.lines[self.cursor.row];
            let col = snap_to_char_boundary(line, self.cursor.col - 1);
            let prev = snap_to_char_boundary(line, col);
            if self.cursor.col == prev {
                return false;
            }
            line.replace_range(prev..self.cursor.col, "");
            self.cursor.col = prev;
            return true;
        }
        if self.cursor.row > 0 {
            let removed = self.lines.remove(self.cursor.row);
            self.cursor.row -= 1;
            let line = &mut self.lines[self.cursor.row];
            self.cursor.col = line.len();
            line.push_str(&removed);
            return true;
        }
        false
    }

    fn delete_forward_internal(&mut self) -> bool {
        let line = &mut self.lines[self.cursor.row];
        let col = snap_to_char_boundary(line, self.cursor.col);
        self.cursor.col = col;
        if col < line.len() {
            let next_col = next_char_boundary(line, col);
            line.replace_range(col..next_col, "");
            return true;
        }
        if self.cursor.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor.row + 1);
            self.lines[self.cursor.row].push_str(&next);
            return true;
        }
        false
    }

    fn cursor_left_internal(&mut self) {
        if self.cursor.col > 0 {
            let line = &self.lines[self.cursor.row];
            self.cursor.col = snap_to_char_boundary(line, self.cursor.col - 1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
            self.cursor.col = self.lines[self.cursor.row].len();
        }
    }

    fn cursor_right_internal(&mut self) {
        let line = &self.lines[self.cursor.row];
        if self.cursor.col < line.len() {
            self.cursor.col = next_char_boundary(line, self.cursor.col);
        } else if self.cursor.row + 1 < self.lines.len() {
            self.cursor.row += 1;
            self.cursor.col = 0;
        }
    }

    fn cursor_up_internal(&mut self) {
        if self.cursor.row == 0 {
            return;
        }
        self.cursor.row -= 1;
        let line = &self.lines[self.cursor.row];
        self.cursor.col = snap_to_char_boundary(line, self.cursor.col);
    }

    fn cursor_down_internal(&mut self) {
        if self.cursor.row + 1 >= self.lines.len() {
            return;
        }
        self.cursor.row += 1;
        let line = &self.lines[self.cursor.row];
        self.cursor.col = snap_to_char_boundary(line, self.cursor.col);
    }

    fn clear_to_line_start_internal(&mut self) -> bool {
        if self.cursor.col == 0 {
            return false;
        }
        let col = self.cursor.col;
        self.lines[self.cursor.row].replace_range(0..col, "");
        self.cursor.col = 0;
        true
    }

    fn clear_to_line_end_internal(&mut self) -> bool {
        let line = &mut self.lines[self.cursor.row];
        let col = self.cursor.col;
        if col >= line.len() {
            return false;
        }
        line.replace_range(col..line.len(), "");
        true
    }

    fn delete_word_backward_internal(&mut self) -> bool {
        let initial_col = self.cursor.col;
        let initial_row = self.cursor.row;
        if self.cursor.col == 0 && self.cursor.row == 0 {
            return false;
        }
        if self.cursor.col == 0 {
            return self.backspace_internal();
        }
        let line = &self.lines[self.cursor.row];
        let mut new_col = self.cursor.col;
        while new_col > 0 {
            let prev = snap_to_char_boundary(line, new_col - 1);
            let ch = line[prev..new_col].chars().next_back().unwrap();
            if !ch.is_whitespace() {
                break;
            }
            new_col = prev;
        }
        while new_col > 0 {
            let prev = snap_to_char_boundary(line, new_col - 1);
            let ch = line[prev..new_col].chars().next_back().unwrap();
            if ch.is_whitespace() {
                break;
            }
            new_col = prev;
        }
        if new_col == initial_col && initial_row == 0 {
            return false;
        }
        self.lines[self.cursor.row].replace_range(new_col..self.cursor.col, "");
        self.cursor.col = new_col;
        true
    }

    fn clamp_scroll(&mut self) {
        let cursor_row = self.cursor.row;
        let max = self.max_visible_rows as usize;
        if cursor_row >= self.scroll_top + max {
            self.scroll_top = cursor_row + 1 - max;
        } else if cursor_row < self.scroll_top {
            self.scroll_top = cursor_row;
        }
        if self.lines.len() <= max {
            self.scroll_top = 0;
        }
    }
}

fn snap_to_char_boundary(line: &str, col: usize) -> usize {
    let mut c = col.min(line.len());
    while c > 0 && !line.is_char_boundary(c) {
        c -= 1;
    }
    c
}

fn next_char_boundary(line: &str, col: usize) -> usize {
    let mut c = col.min(line.len());
    if c < line.len() {
        c += 1;
        while c < line.len() && !line.is_char_boundary(c) {
            c += 1;
        }
    }
    c
}

fn strip_carriage_returns(s: &str) -> String {
    s.replace('\r', "")
}

pub fn wrap_rows(line: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let w = unicode_width::UnicodeWidthStr::width(line);
    if w == 0 { 1 } else { (w + width - 1) / width }
}

/// Verify all internal invariants of the bar. Returns `Ok(())` if healthy,
/// or a description of the first violated invariant otherwise.
///
/// Invariants checked:
/// - `lines` is non-empty.
/// - `cursor.row < lines.len()`.
/// - `cursor.col` is a valid char boundary within `lines[cursor.row]`.
/// - `scroll_top` is `0` when `lines.len() <= max_visible_rows`.
/// - `scroll_top + max_visible_rows >= cursor.row + 1` (caret always visible).
pub fn invariants_hold(bar: &InputBar) -> Result<(), String> {
    if bar.lines.is_empty() {
        return Err("lines must be non-empty".into());
    }
    if bar.cursor.row >= bar.lines.len() {
        return Err(format!(
            "cursor.row {} >= lines.len() {}",
            bar.cursor.row,
            bar.lines.len()
        ));
    }
    let line = &bar.lines[bar.cursor.row];
    if bar.cursor.col > line.len() || !line.is_char_boundary(bar.cursor.col) {
        return Err(format!(
            "cursor.col {} not a char boundary of {:?} (len {})",
            bar.cursor.col,
            line,
            line.len()
        ));
    }
    if bar.lines.len() <= bar.max_visible_rows as usize && bar.scroll_top != 0 {
        return Err(format!(
            "scroll_top {} non-zero when lines.len() {} <= max_visible_rows {}",
            bar.scroll_top,
            bar.lines.len(),
            bar.max_visible_rows
        ));
    }
    let vis_end = bar.scroll_top + bar.max_visible_rows as usize;
    if bar.cursor.row + 1 > vis_end {
        return Err(format!(
            "caret row {} not visible: scroll_top {} + max_visible_rows {} < caret + 1",
            bar.cursor.row, bar.scroll_top, bar.max_visible_rows
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventKind};

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        k(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn shift(c: char) -> KeyEvent {
        k(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn bar() -> InputBar {
        InputBar::new(40)
    }

    // ── G1: Shift+Enter inserts newline ─────────────────────────────
    #[test]
    fn shift_enter_inserts_newline() {
        let mut b = bar();
        b.handle_key(char_key('h'));
        b.handle_key(char_key('i'));
        let act = b.handle_key(k(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(act, InputAction::BufferChanged);
        assert_eq!(b.text(), "hi\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.cursor(), Cursor { row: 1, col: 0 });
    }

    // ── G2: Alt+Enter fallback ──────────────────────────────────────
    #[test]
    fn alt_enter_inserts_newline_fallback() {
        let mut b = bar();
        b.handle_key(char_key('a'));
        let act = b.handle_key(k(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(act, InputAction::BufferChanged);
        assert_eq!(b.text(), "a\n");
        assert_eq!(b.line_count(), 2);
    }

    // ── G3: Ctrl+J universal fallback ───────────────────────────────
    #[test]
    fn ctrl_j_inserts_newline_universal() {
        let mut b = bar();
        b.handle_key(char_key('x'));
        let act = b.handle_key(k(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(act, InputAction::BufferChanged);
        assert_eq!(b.text(), "x\n");
    }

    // ── G4: Enter submits multi-line buffer ─────────────────────────
    #[test]
    fn enter_submits_multi_line_buffer() {
        let mut b = bar();
        for c in "first".chars() {
            b.handle_key(char_key(c));
        }
        b.handle_key(k(KeyCode::Enter, KeyModifiers::SHIFT));
        for c in "second".chars() {
            b.handle_key(char_key(c));
        }
        b.handle_key(k(KeyCode::Enter, KeyModifiers::SHIFT));
        for c in "third".chars() {
            b.handle_key(char_key(c));
        }
        let act = b.handle_key(k(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(act, InputAction::Submit("first\nsecond\nthird".to_string()));
        assert!(b.is_empty());
    }

    // ── G5: paste preserves newlines ────────────────────────────────
    #[test]
    fn pasted_text_preserves_newlines() {
        let mut b = bar();
        let act = b.insert_str("line1\nline2\nline3");
        assert_eq!(act, InputAction::BufferChanged);
        assert_eq!(b.text(), "line1\nline2\nline3");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.cursor(), Cursor { row: 2, col: 5 });
    }

    // ── G5b: paste in middle of buffer splits correctly ────────────
    #[test]
    fn paste_in_middle_inserts_lines() {
        let mut b = bar();
        for c in "AB".chars() {
            b.handle_key(char_key(c));
        }
        b.cursor = Cursor { row: 0, col: 1 };
        b.insert_str("X\nY");
        // "AB" with cursor at col 1, paste "X\nY" → "AX" + newline + "YB"
        assert_eq!(b.text(), "AX\nYB");
        assert_eq!(b.line_count(), 2);
    }

    // ── G5c: CRLF normalized to LF ─────────────────────────────────
    #[test]
    fn paste_crlf_normalized() {
        let mut b = bar();
        b.insert_str("a\r\nb\r\nc");
        assert_eq!(b.text(), "a\nb\nc");
        assert_eq!(b.line_count(), 3);
    }

    // ── G6: cursor moves across lines ───────────────────────────────
    #[test]
    fn cursor_moves_across_lines() {
        let mut b = bar();
        b.insert_str("ab\ncd");
        b.cursor = Cursor { row: 0, col: 0 };
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 1, col: 0 });
        b.handle_key(k(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 0, col: 2 });
        b.handle_key(k(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 0, col: 1 });
    }

    // ── Up/Down/Left/Right boundary tests ──────────────────────────
    #[test]
    fn cursor_up_clamps_to_line_length() {
        let mut b = bar();
        b.insert_str("hi\nhello");
        b.cursor = Cursor { row: 1, col: 5 };
        b.handle_key(k(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn cursor_down_at_last_line_is_noop() {
        let mut b = bar();
        b.insert_str("a\nb");
        b.cursor = Cursor { row: 1, col: 1 };
        b.handle_key(k(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 1, col: 1 });
    }

    #[test]
    fn cursor_left_wraps_to_prev_line() {
        let mut b = bar();
        b.insert_str("ab\ncd");
        b.cursor = Cursor { row: 1, col: 0 };
        b.handle_key(k(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn cursor_right_wraps_to_next_line() {
        let mut b = bar();
        b.insert_str("ab\ncd");
        b.cursor = Cursor { row: 0, col: 2 };
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor { row: 1, col: 0 });
    }

    #[test]
    fn home_and_end_keys() {
        let mut b = bar();
        for c in "abc".chars() {
            b.handle_key(char_key(c));
        }
        b.handle_key(k(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(b.cursor().col, 0);
        b.handle_key(k(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(b.cursor().col, 3);
    }

    #[test]
    fn ctrl_a_and_ctrl_e() {
        let mut b = bar();
        for c in "xyz".chars() {
            b.handle_key(char_key(c));
        }
        b.handle_key(k(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(b.cursor().col, 0);
        b.handle_key(k(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(b.cursor().col, 3);
    }

    // ── Backspace across lines ──────────────────────────────────────
    #[test]
    fn backspace_at_line_start_merges_with_prev() {
        let mut b = bar();
        b.insert_str("ab\ncd");
        b.cursor = Cursor { row: 1, col: 0 };
        b.handle_key(k(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn backspace_at_origin_is_noop() {
        let mut b = bar();
        let act = b.handle_key(k(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(act, InputAction::NoOp);
        assert!(b.is_empty());
    }

    #[test]
    fn backspace_removes_multibyte_char() {
        let mut b = bar();
        b.insert_str("héllo");
        // "héllo" = 6 bytes: h(0) é(1-2) l(3) l(4) o(5)
        b.cursor = Cursor { row: 0, col: 6 };
        b.handle_key(k(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(b.text(), "héll");
    }

    // ── Delete forward ──────────────────────────────────────────────
    #[test]
    fn delete_at_line_end_merges_with_next() {
        let mut b = bar();
        b.insert_str("ab\ncd");
        b.cursor = Cursor { row: 0, col: 2 };
        b.handle_key(k(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.line_count(), 1);
    }

    // ── Ctrl+U / Ctrl+K / Ctrl+W ────────────────────────────────────
    #[test]
    fn ctrl_u_clears_to_line_start() {
        let mut b = bar();
        for c in "abcde".chars() {
            b.handle_key(char_key(c));
        }
        b.cursor = Cursor { row: 0, col: 3 };
        b.handle_key(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(b.text(), "de");
        assert_eq!(b.cursor().col, 0);
    }

    #[test]
    fn ctrl_k_clears_to_line_end() {
        let mut b = bar();
        for c in "abcde".chars() {
            b.handle_key(char_key(c));
        }
        b.cursor = Cursor { row: 0, col: 2 };
        b.handle_key(k(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn ctrl_w_deletes_word_backward() {
        let mut b = bar();
        b.insert_str("hello world");
        b.cursor = Cursor { row: 0, col: 11 };
        b.handle_key(k(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(b.text(), "hello ");
    }

    #[test]
    fn ctrl_w_at_word_start_strips_trailing_space() {
        let mut b = bar();
        b.insert_str("hello world");
        // readline behavior: ctrl+w at word start deletes back to previous
        // word boundary, which includes the leading space → "world" remains
        b.cursor = Cursor { row: 0, col: 6 };
        b.handle_key(k(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(b.text(), "world");
    }

    // ── G8: buffer grows then scrolls at max rows ──────────────────
    #[test]
    fn buffer_grows_then_scrolls_at_max_rows() {
        let mut b = bar();
        for i in 0..15 {
            if i > 0 {
                b.handle_key(k(KeyCode::Enter, KeyModifiers::SHIFT));
            }
            for c in format!("L{i}").chars() {
                b.handle_key(char_key(c));
            }
        }
        assert_eq!(b.line_count(), 15);
        // cursor at row 14, scroll_top should be 14 - 8 + 1 = 7
        assert_eq!(b.scroll_top(), 7);
    }

    #[test]
    fn scroll_resets_when_buffer_shrinks() {
        let mut b = bar();
        for i in 0..15 {
            if i > 0 {
                b.handle_key(k(KeyCode::Enter, KeyModifiers::SHIFT));
            }
            for c in format!("L{i}").chars() {
                b.handle_key(char_key(c));
            }
        }
        assert!(b.scroll_top() > 0);
        b.clear();
        assert_eq!(b.scroll_top(), 0);
    }

    // ── G9: collapses after submit ─────────────────────────────────
    #[test]
    fn buffer_collapses_after_submit() {
        let mut b = bar();
        b.insert_str("a\nb\nc");
        let act = b.submit();
        assert!(matches!(act, InputAction::Submit(ref s) if s == "a\nb\nc"));
        assert!(b.is_empty());
        assert_eq!(b.line_count(), 1);
    }

    // ── Submit on empty is noop ─────────────────────────────────────
    #[test]
    fn submit_empty_returns_noop() {
        let mut b = bar();
        let act = b.handle_key(k(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(act, InputAction::NoOp);
    }

    // ── Submit on whitespace-only is noop ───────────────────────────
    #[test]
    fn submit_whitespace_only_returns_noop() {
        let mut b = bar();
        b.insert_str("   \n  \t ");
        let act = b.submit();
        assert_eq!(act, InputAction::NoOp);
    }

    // ── Submit preserves internal whitespace ────────────────────────
    #[test]
    fn submit_preserves_internal_whitespace() {
        let mut b = bar();
        b.insert_str("  hello  \n  world  ");
        let act = b.submit();
        assert!(matches!(act, InputAction::Submit(ref s) if s == "  hello  \n  world  ".trim()));
    }

    // ── on_resize recomputes viewport ───────────────────────────────
    #[test]
    fn on_resize_updates_term_cols() {
        let mut b = bar();
        b.on_resize(80);
        assert_eq!(b.term_cols(), 80);
        assert_eq!(b.wrap_width(), 78);
    }

    #[test]
    fn on_resize_clamps_small_term_cols() {
        let mut b = bar();
        b.on_resize(2);
        assert_eq!(b.wrap_width(), DEFAULT_MIN_WRAP_WIDTH);
    }

    // ── set_text / clear ────────────────────────────────────────────
    #[test]
    fn set_text_replaces_buffer() {
        let mut b = bar();
        b.insert_str("old");
        b.set_text("new\nstuff");
        assert_eq!(b.text(), "new\nstuff");
        assert_eq!(b.cursor(), Cursor { row: 1, col: 5 });
    }

    #[test]
    fn set_text_empty_resets() {
        let mut b = bar();
        b.insert_str("old");
        b.set_text("");
        assert!(b.is_empty());
    }

    // ── visible_rows / wrap_rows ────────────────────────────────────
    #[test]
    fn wrap_rows_short_line_is_one() {
        assert_eq!(wrap_rows("hello", 10), 1);
    }

    #[test]
    fn wrap_rows_exact_boundary_is_one() {
        assert_eq!(wrap_rows("1234567890", 10), 1);
    }

    #[test]
    fn wrap_rows_over_boundary_is_two() {
        assert_eq!(wrap_rows("12345678901", 10), 2);
    }

    #[test]
    fn visible_rows_clamped_to_max() {
        let mut b = bar();
        for i in 0..50 {
            if i > 0 {
                b.handle_key(k(KeyCode::Enter, KeyModifiers::SHIFT));
            }
            for c in "x".repeat(60).chars() {
                b.handle_key(char_key(c));
            }
        }
        assert!(b.visible_rows() <= MAX_VISIBLE_ROWS);
    }

    // ── Unicode handling ────────────────────────────────────────────
    #[test]
    fn unicode_round_trip() {
        let mut b = bar();
        b.insert_str("héllo 🌍");
        assert_eq!(b.text(), "héllo 🌍");
        assert_eq!(b.cursor().col, "héllo 🌍".len());
    }

    #[test]
    fn unicode_backspace_removes_full_char() {
        let mut b = bar();
        b.insert_str("日本");
        b.handle_key(k(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(b.text(), "日");
    }

    #[test]
    fn unicode_cursor_movement() {
        let mut b = bar();
        b.insert_str("a日本b");
        // a(1) 日(3) 本(3) b(1) → byte offsets: 0,1,4,7,8
        b.cursor = Cursor { row: 0, col: 0 };
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(b.cursor().col, 1);
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(b.cursor().col, 4);
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(b.cursor().col, 7);
        b.handle_key(k(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(b.cursor().col, 8);
    }

    // ── Empty / boundary conditions ─────────────────────────────────
    #[test]
    fn fresh_bar_is_empty() {
        let b = bar();
        assert!(b.is_empty());
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.cursor(), Cursor::default());
    }

    #[test]
    fn enter_on_empty_is_noop_not_submit() {
        let mut b = bar();
        let act = b.handle_key(k(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(act, InputAction::NoOp);
        assert!(b.is_empty());
    }

    #[test]
    fn type_uppercase_letter_works() {
        let mut b = bar();
        b.handle_key(shift('A'));
        assert_eq!(b.text(), "A");
    }

    #[test]
    fn backspace_with_ctrl_modifier_still_deletes() {
        let mut b = bar();
        b.insert_str("abc");
        b.handle_key(k(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn clear_resets_fully() {
        let mut b = bar();
        b.insert_str("a\nb\nc");
        b.cursor = Cursor { row: 2, col: 0 };
        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.cursor(), Cursor::default());
        assert_eq!(b.scroll_top(), 0);
    }

    // ── Multi-line cursor at end ────────────────────────────────────
    #[test]
    fn cursor_at_end_after_typing_multi_line() {
        let mut b = bar();
        b.insert_str("first\nsecond\nthird");
        assert_eq!(b.cursor(), Cursor { row: 2, col: 5 });
    }

    // ── insert_str with empty string ────────────────────────────────
    #[test]
    fn insert_empty_str_is_noop() {
        let mut b = bar();
        b.insert_str("hi");
        let act = b.insert_str("");
        assert_eq!(act, InputAction::NoOp);
        assert_eq!(b.text(), "hi");
    }

    // ── submit returns trimmed payload ─────────────────────────────
    #[test]
    fn submit_trims_outer_whitespace() {
        let mut b = bar();
        b.insert_str("  hello world  ");
        let act = b.submit();
        assert!(matches!(act, InputAction::Submit(ref s) if s == "hello world"));
    }

    // ── Property test: random key sequences preserve invariants ─────────────
    //
    // §12.1 "cursor_invariants_hold_after_random_keys" from PRD 29b.
    // Runs 1000 randomized scenarios: each starts with a randomly seeded
    // buffer, then applies 50 random key events. After every step we assert
    // all invariants hold (char boundaries, cursor in range, scroll makes
    // caret visible, etc.). The bar must NEVER panic or violate invariants,
    // regardless of key sequence.

    /// Build a KeyEvent from a deterministic test seed.
    fn key_from_index(i: usize) -> KeyEvent {
        use crossterm::event::KeyEventState;
        // 16 key shapes — covers all branches in handle_key.
        const SHAPES: &[(KeyCode, KeyModifiers)] = &[
            (KeyCode::Char('a'), KeyModifiers::NONE),
            (KeyCode::Char('Z'), KeyModifiers::SHIFT),
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
            (KeyCode::Enter, KeyModifiers::NONE),
            (KeyCode::Enter, KeyModifiers::SHIFT),
            (KeyCode::Enter, KeyModifiers::ALT),
            (KeyCode::Backspace, KeyModifiers::NONE),
            (KeyCode::Delete, KeyModifiers::NONE),
            (KeyCode::Left, KeyModifiers::NONE),
            (KeyCode::Right, KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
            (KeyCode::Home, KeyModifiers::NONE),
            (KeyCode::End, KeyModifiers::NONE),
            (KeyCode::Char('u'), KeyModifiers::CONTROL),
            (KeyCode::Char('w'), KeyModifiers::CONTROL),
        ];
        let (code, mods) = SHAPES[i % SHAPES.len()];
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn pseudo_seed(seed: u64) -> u64 {
        // Simple LCG; deterministic, no external dep.
        seed.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
    }

    #[test]
    fn cursor_invariants_hold_after_random_keys() {
        // 1000 iterations × 50 key events per iteration. Stays fast (< 1s)
        // and gives strong coverage of key combinations.
        const ITERATIONS: usize = 1000;
        const STEPS_PER_ITER: usize = 50;

        let mut seed: u64 = 0xCAFEBABEu64;
        for iter in 0..ITERATIONS {
            let mut b = InputBar::new(40);
            // Seed the buffer with a small randomized prefix to avoid the
            // trivial-empty-buffer state.
            seed = pseudo_seed(seed);
            if seed % 3 == 0 {
                b.insert_str("seed");
            } else if seed % 3 == 1 {
                b.insert_str("héllo 🌍");
            }

            for step in 0..STEPS_PER_ITER {
                seed = pseudo_seed(seed);
                let idx = (seed as usize).wrapping_add(step);
                let key = key_from_index(idx);
                let _ = b.handle_key(key);
                b.check_invariants()
                    .unwrap_or_else(|e| panic!("iter {iter} step {step} after {key:?}: {e}"));
            }
        }
    }

    /// Property: a 200-char line on an 80-col terminal occupies at least 3
    /// visible rows (capped, may be more).
    #[test]
    fn visible_rows_accounts_for_soft_wrap() {
        let mut b = InputBar::new(80);
        b.insert_str(&"x".repeat(200));
        let vis = b.visible_rows();
        // wrap_width = 78, 200 / 78 = 3 rows
        assert!(vis >= 3, "expected ≥3 visible rows, got {vis}");
    }

    /// Property: round-trip a known multi-line buffer through `text()` and
    /// `set_text()` to ensure no content loss or re-ordering.
    #[test]
    fn text_set_text_round_trip() {
        let cases = [
            "",
            "hello",
            "a\nb",
            "\n\n\n",
            "line1\nline2\nline3",
            "héllo 🌍\n日本語\n🇺🇸",
        ];
        for case in cases {
            let mut b = InputBar::new(40);
            b.set_text(case);
            assert_eq!(b.text(), case, "round-trip failed for {case:?}");
        }
    }

    /// Regression: visible_rows is clamped at MAX_VISIBLE_ROWS even for huge
    /// buffers.
    #[test]
    fn visible_rows_clamped_above_max() {
        use crossterm::event::KeyEventState;
        let mut b = InputBar::new(40);
        // 100 lines, each short enough to fit on one row
        for i in 0..100 {
            if i > 0 {
                b.handle_key(KeyEvent {
                    code: KeyCode::Enter,
                    modifiers: KeyModifiers::SHIFT,
                    kind: KeyEventKind::Press,
                    state: KeyEventState::NONE,
                });
            }
            b.insert_str(&format!("L{i}"));
        }
        // visible_rows is clamped to MAX_VISIBLE_ROWS = 8
        assert!(b.visible_rows() <= MAX_VISIBLE_ROWS);
        assert!(b.line_count() == 100);
    }
}
