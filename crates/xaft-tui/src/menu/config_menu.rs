//! `ConfigurationMenu` — interactive config editor opened by `/config` (no args).
//!
//! Implements [`MenuWidget`] for an interactive, scrollable section/field
//! navigator that allows toggling booleans, editing strings and integers,
//! and saving to `.xaft.toml`.
//!
//! # Navigation
//! - Arrow Up/Down: move cursor
//! - Enter / Right: activate (toggle bool, open edit for string/int)
//! - Tab: expand/collapse section
//! - Left: collapse current section
//! - `s`: save to `.xaft.toml`
//! - Esc: close menu
//!
//! # Editing
//! - Any printable char: append to edit buffer
//! - Backspace: delete last char
//! - Enter: commit edit
//! - Esc: cancel edit

use std::io;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use xaft_config::XaftConfig;

use super::{MenuPayload, MenuResult, MenuWidget};

// ── ConfigValue ───────────────────────────────────────────────────────────────

/// The typed value of a single config field for the interactive menu.
///
/// This is distinct from [`crate::slash::ConfigValueKind`], which is used
/// only for the read-only static display.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    /// A UTF-8 string value.
    Str(String),
    /// An integer value.
    Int(i64),
    /// A floating-point value.
    Float(f64),
    /// A boolean value.
    Bool(bool),
    /// An API key or secret — displayed as `***`, never editable.
    Redacted,
    /// A complex value (HashMap, Vec) — displayed as `[ … ]`, never editable.
    Complex,
}

impl ConfigValue {
    /// Return the display string for this value.
    pub fn display(&self) -> String {
        match self {
            Self::Str(s) => format!("{s:?}"),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => format!("{f}"),
            Self::Bool(b) => b.to_string(),
            Self::Redacted => "***".to_string(),
            Self::Complex => "[ … ]".to_string(),
        }
    }

    /// Return `true` for types that can be edited inline (str, int, float).
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Str(_) | Self::Int(_) | Self::Float(_))
    }

    /// Return `true` for bool values (toggle with Enter).
    pub fn is_toggle(&self) -> bool {
        matches!(self, Self::Bool(_))
    }

    /// Return the value with the boolean flipped.  Panics on non-bool.
    pub fn toggled(&self) -> Self {
        match self {
            Self::Bool(b) => Self::Bool(!b),
            _ => self.clone(),
        }
    }
}

// ── ConfigRow ─────────────────────────────────────────────────────────────────

/// One editable row inside a [`ConfigSection`].
///
/// This is different from [`crate::slash::ConfigRow`], which is used for the
/// static, read-only display produced by `/config <filter>`.
pub struct ConfigRow {
    /// Dotted path, e.g. `"agent.default.max_turns"`.
    pub path: String,
    /// Human-readable label, e.g. `"max turns"`.
    pub label: String,
    /// Current (possibly edited) value.
    pub value: ConfigValue,
    /// Value from `XaftConfig::default()` for change detection.
    pub default: ConfigValue,
    /// Whether the user can edit this field inline.
    pub editable: bool,
}

impl ConfigRow {
    /// Return `true` if the current value differs from the compiled-in default.
    pub fn is_changed(&self) -> bool {
        self.value != self.default
    }
}

// ── ConfigSection ─────────────────────────────────────────────────────────────

/// A group of [`ConfigRow`]s under a named header.
///
/// Different from [`crate::slash::ConfigSection`], which is the static display type.
pub struct ConfigSection {
    /// Section name, e.g. `"core"`, `"agent.default"`.
    pub name: String,
    /// The rows within this section.
    pub rows: Vec<ConfigRow>,
    /// Whether this section is expanded (rows visible).
    pub expanded: bool,
}

// ── MenuState ─────────────────────────────────────────────────────────────────

/// Internal editing mode for the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuState {
    /// Normal cursor navigation mode.
    Navigate,
    /// Inline text-edit mode for a string/int/float field.
    Edit,
}

// ── ConfigurationMenu ─────────────────────────────────────────────────────────

/// Interactive configuration editor opened by `/config` with no arguments.
pub struct ConfigurationMenu {
    /// Snapshot of the current config at open time.  Mutated by edits.
    snapshot: XaftConfig,
    /// Sections built from `snapshot`.
    sections: Vec<ConfigSection>,
    /// Cursor position: `(section_idx, row_idx)`.
    /// `row_idx == -1` means the section header is focused.
    cursor: (usize, i32),
    /// Current interaction mode.
    state: MenuState,
    /// Buffer for the active inline edit.
    edit_buf: String,
    /// One-line status message shown below the menu.
    status_msg: String,
    /// First visible row index (for scrolling).
    scroll_offset: usize,
    /// Maximum rows to display at once.
    max_visible: usize,
    /// Working directory — save target is `working_dir/.xaft.toml`.
    working_dir: PathBuf,
}

impl ConfigurationMenu {
    /// Construct a new `ConfigurationMenu` from a config snapshot.
    pub fn new(config: &XaftConfig, working_dir: PathBuf) -> Self {
        let snapshot = config.clone();
        let sections = build_sections(&snapshot);
        Self {
            snapshot,
            sections,
            cursor: (0, -1),
            state: MenuState::Navigate,
            edit_buf: String::new(),
            status_msg: "↑↓ navigate  Enter toggle/edit  Tab expand  s save  Esc close".to_string(),
            scroll_offset: 0,
            max_visible: 12,
            working_dir,
        }
    }

    /// Rebuild sections from the current snapshot (call after editing a value).
    fn rebuild_sections(&mut self) {
        // Carry over the expanded state.
        let expanded: Vec<bool> = self.sections.iter().map(|s| s.expanded).collect();
        let mut new_sections = build_sections(&self.snapshot);
        for (i, sec) in new_sections.iter_mut().enumerate() {
            if let Some(&exp) = expanded.get(i) {
                sec.expanded = exp;
            }
        }
        self.sections = new_sections;
    }

    /// Return a reference to the currently-focused `ConfigRow`, if any.
    fn focused_row(&self) -> Option<&ConfigRow> {
        let (si, ri) = self.cursor;
        if ri < 0 {
            return None;
        }
        self.sections.get(si).and_then(|s| s.rows.get(ri as usize))
    }

    /// Return a mutable reference to the currently-focused `ConfigRow`, if any.
    fn focused_row_mut(&mut self) -> Option<&mut ConfigRow> {
        let (si, ri) = self.cursor;
        if ri < 0 {
            return None;
        }
        self.sections
            .get_mut(si)
            .and_then(|s| s.rows.get_mut(ri as usize))
    }

    /// Enumerate all valid cursor positions (section_idx, row_idx).
    /// Headers are at row_idx == -1.  Collapsed sections show only the header.
    fn all_positions(&self) -> Vec<(usize, i32)> {
        let mut positions = Vec::new();
        for (si, sec) in self.sections.iter().enumerate() {
            positions.push((si, -1));
            if sec.expanded {
                for ri in 0..sec.rows.len() {
                    positions.push((si, ri as i32));
                }
            }
        }
        positions
    }

    /// Move cursor by `delta` positions (wraps around).
    fn move_cursor(&mut self, delta: i32) {
        let positions = self.all_positions();
        if positions.is_empty() {
            return;
        }
        let current = self.cursor;
        let idx = positions.iter().position(|&p| p == current).unwrap_or(0);
        let new_idx = (idx as i32 + delta).rem_euclid(positions.len() as i32) as usize;
        self.cursor = positions[new_idx];
        self.adjust_scroll();
    }

    /// Ensure the focused row is visible within the scroll window.
    fn adjust_scroll(&mut self) {
        let positions = self.all_positions();
        let idx = positions
            .iter()
            .position(|&p| p == self.cursor)
            .unwrap_or(0);
        if idx < self.scroll_offset {
            self.scroll_offset = idx;
        } else if idx >= self.scroll_offset + self.max_visible {
            self.scroll_offset = idx + 1 - self.max_visible;
        }
    }

    /// Handle Enter/Right on the currently-focused item.
    fn activate_focused(&mut self) {
        let (si, ri) = self.cursor;
        if ri == -1 {
            // Header — toggle section expansion.
            if let Some(sec) = self.sections.get_mut(si) {
                sec.expanded = !sec.expanded;
            }
            return;
        }

        let can_toggle = self
            .focused_row()
            .map(|r| r.value.is_toggle())
            .unwrap_or(false);
        let can_edit = self
            .focused_row()
            .map(|r| r.value.is_editable())
            .unwrap_or(false);

        if can_toggle {
            // Toggle boolean in place and apply to snapshot.
            let (path, toggled) = if let Some(row) = self.focused_row_mut() {
                let toggled = row.value.toggled();
                row.value = toggled.clone();
                (row.path.clone(), toggled)
            } else {
                return;
            };
            self.apply_value_to_snapshot(&path, &toggled);
            self.status_msg = format!("Toggled {path}");
        } else if can_edit {
            // Enter edit mode with current value pre-populated.
            let initial = match self.focused_row().map(|r| &r.value) {
                Some(ConfigValue::Str(s)) => s.clone(),
                Some(ConfigValue::Int(i)) => i.to_string(),
                Some(ConfigValue::Float(f)) => f.to_string(),
                _ => String::new(),
            };
            self.edit_buf = initial;
            self.state = MenuState::Edit;
            self.status_msg = "Enter to commit  Esc to cancel".to_string();
        }
    }

    /// Collapse the current section (or its parent if on a row).
    fn collapse_current_section(&mut self) {
        let (si, _ri) = self.cursor;
        if let Some(sec) = self.sections.get_mut(si) {
            sec.expanded = false;
        }
        self.cursor = (si, -1);
    }

    /// Toggle expansion of the focused section.
    fn toggle_expand_current_section(&mut self) {
        let (si, _ri) = self.cursor;
        if let Some(sec) = self.sections.get_mut(si) {
            sec.expanded = !sec.expanded;
        }
    }

    /// Commit the current edit buffer to the focused row.
    fn commit_edit(&mut self) {
        let buf = self.edit_buf.clone();
        let (path, new_value) = if let Some(row) = self.focused_row_mut() {
            let new_value = match &row.value {
                ConfigValue::Int(_) => buf
                    .parse::<i64>()
                    .map(ConfigValue::Int)
                    .unwrap_or_else(|_| row.value.clone()),
                ConfigValue::Float(_) => buf
                    .parse::<f64>()
                    .map(ConfigValue::Float)
                    .unwrap_or_else(|_| row.value.clone()),
                ConfigValue::Str(_) => ConfigValue::Str(buf.clone()),
                _ => row.value.clone(),
            };
            row.value = new_value.clone();
            (row.path.clone(), new_value)
        } else {
            self.state = MenuState::Navigate;
            self.status_msg =
                "↑↓ navigate  Enter toggle/edit  Tab expand  s save  Esc close".to_string();
            return;
        };

        self.apply_value_to_snapshot(&path, &new_value);
        self.state = MenuState::Navigate;
        self.status_msg = format!("Updated {path}");
    }

    /// Cancel the current edit without applying changes.
    fn cancel_edit(&mut self) {
        self.edit_buf.clear();
        self.state = MenuState::Navigate;
        self.status_msg =
            "↑↓ navigate  Enter toggle/edit  Tab expand  s save  Esc close".to_string();
    }

    /// Apply a `ConfigValue` back to the relevant field in `self.snapshot`.
    ///
    /// Uses the dotted path to identify the field.  Only a well-known subset
    /// of fields is handled (the ones enumerated in [`build_sections`]).
    fn apply_value_to_snapshot(&mut self, path: &str, value: &ConfigValue) {
        match path {
            // ── core ──────────────────────────────────────────────────────
            "core.log_level" => {
                if let ConfigValue::Str(s) = value {
                    if let Ok(level) = s.parse::<xaft_config::LogLevel>() {
                        self.snapshot.core.log_level = level;
                    }
                }
            }
            "core.telemetry" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.core.telemetry = *b;
                }
            }
            "core.agents_md_enabled" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.core.agents_md_enabled = *b;
                }
            }
            "core.agents_md_max_bytes" => {
                if let ConfigValue::Int(i) = value {
                    self.snapshot.core.agents_md_max_bytes = *i as usize;
                }
            }
            // ── agent.default ────────────────────────────────────────────
            "agent.default.model" => {
                if let ConfigValue::Str(s) = value {
                    if let Some(preset) = self.snapshot.agent.get_mut("default") {
                        preset.model = s.clone();
                    }
                }
            }
            "agent.default.provider" => {
                if let ConfigValue::Str(s) = value {
                    if let Some(preset) = self.snapshot.agent.get_mut("default") {
                        preset.provider = s.clone();
                    }
                }
            }
            "agent.default.max_turns" => {
                if let ConfigValue::Int(i) = value {
                    if let Some(preset) = self.snapshot.agent.get_mut("default") {
                        preset.max_turns = *i as u32;
                    }
                }
            }
            "agent.default.temperature" => {
                if let ConfigValue::Float(f) = value {
                    if let Some(preset) = self.snapshot.agent.get_mut("default") {
                        preset.temperature = *f as f32;
                    }
                }
            }
            "agent.default.top_p" => {
                if let ConfigValue::Float(f) = value {
                    if let Some(preset) = self.snapshot.agent.get_mut("default") {
                        preset.top_p = *f as f32;
                    }
                }
            }
            // ── tui ───────────────────────────────────────────────────────
            "tui.theme" => {
                if let ConfigValue::Str(s) = value {
                    self.snapshot.tui.theme = match s.to_lowercase().as_str() {
                        "light" => xaft_config::TuiTheme::Light,
                        "solarized" => xaft_config::TuiTheme::Solarized,
                        _ => xaft_config::TuiTheme::Dark,
                    };
                }
            }
            "tui.mouse" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.tui.mouse = *b;
                }
            }
            "tui.timestamps" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.tui.timestamps = *b;
                }
            }
            "tui.render_markdown" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.tui.render_markdown = *b;
                }
            }
            "tui.preserve_output_on_exit" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.tui.preserve_output_on_exit = *b;
                }
            }
            "tui.show_exit_summary" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.tui.show_exit_summary = *b;
                }
            }
            "tui.max_background_tasks" => {
                if let ConfigValue::Int(i) = value {
                    self.snapshot.tui.max_background_tasks = *i as usize;
                }
            }
            // ── compaction ────────────────────────────────────────────────
            "compaction.enabled" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.compaction.enabled = *b;
                }
            }
            "compaction.threshold_pct" => {
                if let ConfigValue::Int(i) = value {
                    self.snapshot.compaction.threshold_pct = (*i).clamp(1, 99) as u8;
                }
            }
            "compaction.keep_recent_turns" => {
                if let ConfigValue::Int(i) = value {
                    self.snapshot.compaction.keep_recent_turns = *i as usize;
                }
            }
            // ── guardrail ─────────────────────────────────────────────────
            "guardrail.file_destruction" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.guardrail.file_destruction = *b;
                }
            }
            "guardrail.secret_leakage" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.guardrail.secret_leakage = *b;
                }
            }
            "guardrail.cost_limit" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.guardrail.cost_limit = *b;
                }
            }
            "guardrail.command_approval" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.guardrail.command_approval = *b;
                }
            }
            // ── memory ────────────────────────────────────────────────────
            "memory.enabled" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.memory.enabled = *b;
                }
            }
            "memory.auto_remember" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.memory.auto_remember = *b;
                }
            }
            "memory.auto_summarize" => {
                if let ConfigValue::Bool(b) = value {
                    self.snapshot.memory.auto_summarize = *b;
                }
            }
            _ => {
                // Unknown path — silently ignore.
            }
        }
    }

    /// Write the snapshot to `working_dir/.xaft.toml`.
    fn save_to_toml(&mut self) {
        let target = self.working_dir.join(".xaft.toml");
        match toml::to_string_pretty(&self.snapshot) {
            Ok(content) => match std::fs::write(&target, content) {
                Ok(()) => {
                    self.status_msg = format!("Saved to {}", target.display());
                }
                Err(e) => {
                    self.status_msg = format!("Save failed: {e}");
                }
            },
            Err(e) => {
                self.status_msg = format!("Serialize failed: {e}");
            }
        }
    }

    /// Build the list of display strings for visible rows (before slicing by
    /// `scroll_offset`/`max_visible`).
    fn build_visible_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        for (si, sec) in self.sections.iter().enumerate() {
            let header_focused = self.cursor == (si, -1);
            let expanded_indicator = if sec.expanded { "▼" } else { "▶" };
            let cursor_marker = if header_focused { ">" } else { " " };
            rows.push(format!(
                "  {cursor_marker} {expanded_indicator} \x1b[1m{}\x1b[0m",
                sec.name
            ));

            if sec.expanded {
                for (ri, row) in sec.rows.iter().enumerate() {
                    let row_focused = self.cursor == (si, ri as i32);
                    let cursor_marker = if row_focused { ">" } else { " " };
                    let changed = if row.is_changed() { "*" } else { " " };

                    // In edit mode, show the edit buffer for the focused row.
                    let value_str = if row_focused && self.state == MenuState::Edit {
                        format!("\x1b[7m{}_\x1b[0m", self.edit_buf)
                    } else {
                        let disp: String = row.value.display().chars().take(28).collect();
                        let ellipsis = if row.value.display().chars().count() > 28 {
                            "…"
                        } else {
                            ""
                        };
                        let color = match &row.value {
                            ConfigValue::Bool(true) => "\x1b[32m",
                            ConfigValue::Bool(false) => "\x1b[33m",
                            ConfigValue::Redacted => "\x1b[2m",
                            _ => "\x1b[36m",
                        };
                        format!("{color}{disp}{ellipsis}\x1b[0m")
                    };

                    let editable_hint = match &row.value {
                        ConfigValue::Bool(_) => "[toggle]",
                        v if v.is_editable() => "[edit]  ",
                        _ => "[view]  ",
                    };

                    rows.push(format!(
                        "  {cursor_marker} {changed}  {label:<24} {value_str}  \x1b[2m{editable_hint}\x1b[0m",
                        label = row.label,
                    ));
                }
            }
        }
        rows
    }

    /// Handle key events in Navigate mode.
    fn handle_nav_key(&mut self, key: KeyEvent) -> MenuResult {
        match key.code {
            KeyCode::Esc => return MenuResult::Cancel,
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Enter | KeyCode::Right => self.activate_focused(),
            KeyCode::Left => self.collapse_current_section(),
            KeyCode::Tab => self.toggle_expand_current_section(),
            KeyCode::Char('s') if key.modifiers == KeyModifiers::NONE => {
                self.save_to_toml();
            }
            _ => {}
        }
        MenuResult::Continue
    }

    /// Handle key events in Edit mode.
    fn handle_edit_key(&mut self, key: KeyEvent) -> MenuResult {
        match key.code {
            KeyCode::Esc => self.cancel_edit(),
            KeyCode::Enter => self.commit_edit(),
            KeyCode::Backspace => {
                self.edit_buf.pop();
            }
            KeyCode::Char(c) => {
                self.edit_buf.push(c);
            }
            _ => {}
        }
        MenuResult::Continue
    }
}

// ── MenuWidget impl ───────────────────────────────────────────────────────────

impl MenuWidget for ConfigurationMenu {
    fn title(&self) -> &str {
        "Configuration"
    }

    fn edit_field_value(&self) -> Option<&str> {
        if self.state == MenuState::Edit {
            Some(&self.edit_buf)
        } else {
            None
        }
    }

    fn render(
        &self,
        out: &mut dyn io::Write,
        size: (u16, u16),
        prev_rows: usize,
    ) -> io::Result<usize> {
        // Build the full frame into a Vec<u8> buffer first, then flush in one
        // write.  crossterm's `queue!` macro requires a concrete Write (not
        // `dyn Write`) because it calls `.by_ref()`, so we use raw ANSI escape
        // sequences instead.
        #[allow(unused_imports)]
        use std::io::Write;

        let cols = size.0 as usize;
        let sep_len = cols.min(72);
        let sep = "─".repeat(sep_len);

        let mut buf: Vec<u8> = Vec::with_capacity(4096);

        // Erase previous frame using ANSI escapes.
        // \x1B[nA = cursor up n, \x1B[2K = erase line, \x1B[B = cursor down 1.
        if prev_rows > 0 {
            write!(buf, "\x1B[{}A", prev_rows)?;
            for _ in 0..prev_rows {
                write!(buf, "\x1B[2K\x1B[B")?;
            }
            write!(buf, "\x1B[{}A", prev_rows)?;
        }

        let mut rows_written = 0usize;

        // Top separator + title.
        write!(
            buf,
            "  \x1b[2m{sep}\x1b[0m  \x1b[1m{}\x1b[0m\r\n",
            self.title()
        )?;
        rows_written += 1;

        // Visible rows.
        let visible = self.build_visible_rows();
        for row in visible
            .iter()
            .skip(self.scroll_offset)
            .take(self.max_visible)
        {
            write!(buf, "{row}\r\n")?;
            rows_written += 1;
        }

        // Scroll indicator (when content overflows).
        let total = visible.len();
        if total > self.max_visible {
            let shown_end = (self.scroll_offset + self.max_visible).min(total);
            write!(
                buf,
                "  \x1b[2m  [{}-{} of {}]\x1b[0m\r\n",
                self.scroll_offset + 1,
                shown_end,
                total
            )?;
            rows_written += 1;
        }

        // Bottom separator + status line.
        write!(buf, "  \x1b[2m{sep}\x1b[0m\r\n")?;
        write!(buf, "  \x1b[2m{}\x1b[0m\r\n", self.status_msg)?;
        rows_written += 2;

        out.write_all(&buf)?;
        out.flush()?;
        Ok(rows_written)
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        match self.state {
            MenuState::Navigate => self.handle_nav_key(key),
            MenuState::Edit => self.handle_edit_key(key),
        }
    }
}

// ── build_sections ────────────────────────────────────────────────────────────

/// Build the interactive menu sections from a `XaftConfig` snapshot.
///
/// Enumerates the well-known fields of each sub-struct manually so we can
/// assign human-readable labels and correctly type each value.
pub fn build_sections(config: &XaftConfig) -> Vec<ConfigSection> {
    let defaults = XaftConfig::default();
    let mut sections = Vec::new();

    // ── core ──────────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "core".to_string(),
        expanded: true,
        rows: vec![
            ConfigRow {
                path: "core.log_level".to_string(),
                label: "log level".to_string(),
                value: ConfigValue::Str(config.core.log_level.to_string()),
                default: ConfigValue::Str(defaults.core.log_level.to_string()),
                editable: true,
            },
            ConfigRow {
                path: "core.telemetry".to_string(),
                label: "telemetry".to_string(),
                value: ConfigValue::Bool(config.core.telemetry),
                default: ConfigValue::Bool(defaults.core.telemetry),
                editable: false,
            },
            ConfigRow {
                path: "core.agents_md_enabled".to_string(),
                label: "agents.md enabled".to_string(),
                value: ConfigValue::Bool(config.core.agents_md_enabled),
                default: ConfigValue::Bool(defaults.core.agents_md_enabled),
                editable: false,
            },
            ConfigRow {
                path: "core.agents_md_max_bytes".to_string(),
                label: "agents.md max bytes".to_string(),
                value: ConfigValue::Int(config.core.agents_md_max_bytes as i64),
                default: ConfigValue::Int(defaults.core.agents_md_max_bytes as i64),
                editable: true,
            },
        ],
    });

    // ── agent.default ─────────────────────────────────────────────────────────
    if let Some(preset) = config.agent.get("default") {
        let def_preset = defaults.agent.get("default").cloned().unwrap_or_default();
        sections.push(ConfigSection {
            name: "agent.default".to_string(),
            expanded: true,
            rows: vec![
                ConfigRow {
                    path: "agent.default.model".to_string(),
                    label: "model".to_string(),
                    value: ConfigValue::Str(preset.model.clone()),
                    default: ConfigValue::Str(def_preset.model.clone()),
                    editable: true,
                },
                ConfigRow {
                    path: "agent.default.provider".to_string(),
                    label: "provider".to_string(),
                    value: ConfigValue::Str(preset.provider.clone()),
                    default: ConfigValue::Str(def_preset.provider.clone()),
                    editable: true,
                },
                ConfigRow {
                    path: "agent.default.max_turns".to_string(),
                    label: "max turns".to_string(),
                    value: ConfigValue::Int(preset.max_turns as i64),
                    default: ConfigValue::Int(def_preset.max_turns as i64),
                    editable: true,
                },
                ConfigRow {
                    path: "agent.default.temperature".to_string(),
                    label: "temperature".to_string(),
                    value: ConfigValue::Float(preset.temperature as f64),
                    default: ConfigValue::Float(def_preset.temperature as f64),
                    editable: true,
                },
                ConfigRow {
                    path: "agent.default.top_p".to_string(),
                    label: "top_p".to_string(),
                    value: ConfigValue::Float(preset.top_p as f64),
                    default: ConfigValue::Float(def_preset.top_p as f64),
                    editable: true,
                },
            ],
        });
    }

    // ── provider.anthropic ────────────────────────────────────────────────────
    for (name, provider) in &config.provider {
        let def_provider = defaults.provider.get(name).cloned().unwrap_or_default();
        sections.push(ConfigSection {
            name: format!("provider.{name}"),
            expanded: false,
            rows: vec![
                ConfigRow {
                    path: format!("provider.{name}.type"),
                    label: "type".to_string(),
                    value: ConfigValue::Str(format!("{:?}", provider.provider_type).to_lowercase()),
                    default: ConfigValue::Str(
                        format!("{:?}", def_provider.provider_type).to_lowercase(),
                    ),
                    editable: false,
                },
                ConfigRow {
                    path: format!("provider.{name}.api_key_env"),
                    label: "api_key_env".to_string(),
                    value: ConfigValue::Redacted,
                    default: ConfigValue::Redacted,
                    editable: false,
                },
                ConfigRow {
                    path: format!("provider.{name}.base_url"),
                    label: "base_url".to_string(),
                    value: ConfigValue::Str(provider.base_url.clone()),
                    default: ConfigValue::Str(def_provider.base_url.clone()),
                    editable: false,
                },
                ConfigRow {
                    path: format!("provider.{name}.max_retries"),
                    label: "max retries".to_string(),
                    value: ConfigValue::Int(provider.max_retries as i64),
                    default: ConfigValue::Int(def_provider.max_retries as i64),
                    editable: false,
                },
                ConfigRow {
                    path: format!("provider.{name}.timeout_secs"),
                    label: "timeout (s)".to_string(),
                    value: ConfigValue::Int(provider.timeout_secs as i64),
                    default: ConfigValue::Int(def_provider.timeout_secs as i64),
                    editable: false,
                },
            ],
        });
    }

    // ── tui ───────────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "tui".to_string(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "tui.theme".to_string(),
                label: "theme".to_string(),
                value: ConfigValue::Str(format!("{:?}", config.tui.theme).to_lowercase()),
                default: ConfigValue::Str(format!("{:?}", defaults.tui.theme).to_lowercase()),
                editable: true,
            },
            ConfigRow {
                path: "tui.mouse".to_string(),
                label: "mouse support".to_string(),
                value: ConfigValue::Bool(config.tui.mouse),
                default: ConfigValue::Bool(defaults.tui.mouse),
                editable: false,
            },
            ConfigRow {
                path: "tui.timestamps".to_string(),
                label: "timestamps".to_string(),
                value: ConfigValue::Bool(config.tui.timestamps),
                default: ConfigValue::Bool(defaults.tui.timestamps),
                editable: false,
            },
            ConfigRow {
                path: "tui.render_markdown".to_string(),
                label: "render markdown".to_string(),
                value: ConfigValue::Bool(config.tui.render_markdown),
                default: ConfigValue::Bool(defaults.tui.render_markdown),
                editable: false,
            },
            ConfigRow {
                path: "tui.preserve_output_on_exit".to_string(),
                label: "preserve on exit".to_string(),
                value: ConfigValue::Bool(config.tui.preserve_output_on_exit),
                default: ConfigValue::Bool(defaults.tui.preserve_output_on_exit),
                editable: false,
            },
            ConfigRow {
                path: "tui.show_exit_summary".to_string(),
                label: "show exit summary".to_string(),
                value: ConfigValue::Bool(config.tui.show_exit_summary),
                default: ConfigValue::Bool(defaults.tui.show_exit_summary),
                editable: false,
            },
            ConfigRow {
                path: "tui.max_background_tasks".to_string(),
                label: "max bg tasks".to_string(),
                value: ConfigValue::Int(config.tui.max_background_tasks as i64),
                default: ConfigValue::Int(defaults.tui.max_background_tasks as i64),
                editable: true,
            },
        ],
    });

    // ── compaction ────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "compaction".to_string(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "compaction.enabled".to_string(),
                label: "enabled".to_string(),
                value: ConfigValue::Bool(config.compaction.enabled),
                default: ConfigValue::Bool(defaults.compaction.enabled),
                editable: false,
            },
            ConfigRow {
                path: "compaction.threshold_pct".to_string(),
                label: "threshold %".to_string(),
                value: ConfigValue::Int(config.compaction.threshold_pct as i64),
                default: ConfigValue::Int(defaults.compaction.threshold_pct as i64),
                editable: true,
            },
            ConfigRow {
                path: "compaction.keep_recent_turns".to_string(),
                label: "keep recent turns".to_string(),
                value: ConfigValue::Int(config.compaction.keep_recent_turns as i64),
                default: ConfigValue::Int(defaults.compaction.keep_recent_turns as i64),
                editable: true,
            },
        ],
    });

    // ── guardrail ─────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "guardrail".to_string(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "guardrail.file_destruction".to_string(),
                label: "file destruction".to_string(),
                value: ConfigValue::Bool(config.guardrail.file_destruction),
                default: ConfigValue::Bool(defaults.guardrail.file_destruction),
                editable: false,
            },
            ConfigRow {
                path: "guardrail.secret_leakage".to_string(),
                label: "secret leakage".to_string(),
                value: ConfigValue::Bool(config.guardrail.secret_leakage),
                default: ConfigValue::Bool(defaults.guardrail.secret_leakage),
                editable: false,
            },
            ConfigRow {
                path: "guardrail.cost_limit".to_string(),
                label: "cost limit".to_string(),
                value: ConfigValue::Bool(config.guardrail.cost_limit),
                default: ConfigValue::Bool(defaults.guardrail.cost_limit),
                editable: false,
            },
            ConfigRow {
                path: "guardrail.command_approval".to_string(),
                label: "command approval".to_string(),
                value: ConfigValue::Bool(config.guardrail.command_approval),
                default: ConfigValue::Bool(defaults.guardrail.command_approval),
                editable: false,
            },
        ],
    });

    // ── memory ────────────────────────────────────────────────────────────────
    sections.push(ConfigSection {
        name: "memory".to_string(),
        expanded: false,
        rows: vec![
            ConfigRow {
                path: "memory.enabled".to_string(),
                label: "enabled".to_string(),
                value: ConfigValue::Bool(config.memory.enabled),
                default: ConfigValue::Bool(defaults.memory.enabled),
                editable: false,
            },
            ConfigRow {
                path: "memory.auto_remember".to_string(),
                label: "auto remember".to_string(),
                value: ConfigValue::Bool(config.memory.auto_remember),
                default: ConfigValue::Bool(defaults.memory.auto_remember),
                editable: false,
            },
            ConfigRow {
                path: "memory.auto_summarize".to_string(),
                label: "auto summarize".to_string(),
                value: ConfigValue::Bool(config.memory.auto_summarize),
                default: ConfigValue::Bool(defaults.memory.auto_summarize),
                editable: false,
            },
            ConfigRow {
                path: "memory.backend".to_string(),
                label: "backend".to_string(),
                value: ConfigValue::Str(config.memory.backend.clone()),
                default: ConfigValue::Str(defaults.memory.backend.clone()),
                editable: true,
            },
            ConfigRow {
                path: "memory.max_search_results".to_string(),
                label: "max search results".to_string(),
                value: ConfigValue::Int(config.memory.max_search_results as i64),
                default: ConfigValue::Int(defaults.memory.max_search_results as i64),
                editable: true,
            },
        ],
    });

    sections
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn make_menu(dir: &TempDir) -> ConfigurationMenu {
        ConfigurationMenu::new(&XaftConfig::default(), dir.path().to_path_buf())
    }

    // ── Test 1: ConfigValue::display ─────────────────────────────────────────

    #[test]
    fn test_config_value_display() {
        assert_eq!(ConfigValue::Str("hello".to_string()).display(), "\"hello\"");
        assert_eq!(ConfigValue::Int(42).display(), "42");
        assert_eq!(ConfigValue::Float(3.14).display(), "3.14");
        assert_eq!(ConfigValue::Bool(true).display(), "true");
        assert_eq!(ConfigValue::Bool(false).display(), "false");
        assert_eq!(ConfigValue::Redacted.display(), "***");
        assert_eq!(ConfigValue::Complex.display(), "[ … ]");
    }

    // ── Test 2: ConfigValue::is_editable ─────────────────────────────────────

    #[test]
    fn test_config_value_is_editable() {
        assert!(ConfigValue::Str("x".to_string()).is_editable());
        assert!(ConfigValue::Int(1).is_editable());
        assert!(ConfigValue::Float(1.0).is_editable());
        assert!(!ConfigValue::Bool(true).is_editable());
        assert!(!ConfigValue::Redacted.is_editable());
        assert!(!ConfigValue::Complex.is_editable());
    }

    // ── Test 3: ConfigValue::is_toggle ───────────────────────────────────────

    #[test]
    fn test_config_value_is_toggle() {
        assert!(ConfigValue::Bool(true).is_toggle());
        assert!(ConfigValue::Bool(false).is_toggle());
        assert!(!ConfigValue::Str("x".to_string()).is_toggle());
        assert!(!ConfigValue::Int(1).is_toggle());
        assert!(!ConfigValue::Redacted.is_toggle());
    }

    // ── Test 4: ConfigRow::is_changed ────────────────────────────────────────

    #[test]
    fn test_config_row_is_changed() {
        let unchanged = ConfigRow {
            path: "x".to_string(),
            label: "x".to_string(),
            value: ConfigValue::Bool(true),
            default: ConfigValue::Bool(true),
            editable: false,
        };
        assert!(!unchanged.is_changed());

        let changed = ConfigRow {
            path: "x".to_string(),
            label: "x".to_string(),
            value: ConfigValue::Bool(false),
            default: ConfigValue::Bool(true),
            editable: false,
        };
        assert!(changed.is_changed());
    }

    // ── Test 5: build_sections has core ──────────────────────────────────────

    #[test]
    fn test_build_sections_has_core() {
        let sections = build_sections(&XaftConfig::default());
        let has_core = sections.iter().any(|s| s.name == "core");
        assert!(has_core, "build_sections must produce a 'core' section");
    }

    // ── Test 6: build_sections has agent.default ──────────────────────────────

    #[test]
    fn test_build_sections_has_agent_default() {
        let sections = build_sections(&XaftConfig::default());
        let has_agent = sections.iter().any(|s| s.name == "agent.default");
        assert!(
            has_agent,
            "build_sections must produce an 'agent.default' section"
        );
    }

    // ── Test 7: provider api_key_env is Redacted ──────────────────────────────

    #[test]
    fn test_build_sections_provider_fields_redacted() {
        let sections = build_sections(&XaftConfig::default());
        let provider_sections: Vec<&ConfigSection> = sections
            .iter()
            .filter(|s| s.name.starts_with("provider."))
            .collect();
        assert!(
            !provider_sections.is_empty(),
            "must have at least one provider section"
        );
        for sec in provider_sections {
            let api_key_row = sec.rows.iter().find(|r| r.label == "api_key_env");
            assert!(
                api_key_row.is_some(),
                "section {} must have api_key_env row",
                sec.name
            );
            assert_eq!(
                api_key_row.unwrap().value,
                ConfigValue::Redacted,
                "api_key_env must be Redacted"
            );
        }
    }

    // ── Test 8: navigate down moves cursor ───────────────────────────────────

    #[test]
    fn test_navigate_down_moves_cursor() {
        let tmp = TempDir::new().unwrap();
        let mut menu = make_menu(&tmp);
        let initial_cursor = menu.cursor;
        menu.move_cursor(1);
        assert_ne!(
            menu.cursor, initial_cursor,
            "cursor should move after move_cursor(1)"
        );
    }

    // ── Test 9: Enter on bool field toggles it ───────────────────────────────

    #[test]
    fn test_enter_on_bool_toggles() {
        let tmp = TempDir::new().unwrap();
        let mut menu = make_menu(&tmp);

        // Find the cursor position for a bool field (core.telemetry).
        let positions = menu.all_positions();
        let bool_pos = positions.iter().find(|&&(si, ri)| {
            ri >= 0
                && menu
                    .sections
                    .get(si)
                    .and_then(|s| s.rows.get(ri as usize))
                    .map(|r| r.value.is_toggle())
                    .unwrap_or(false)
        });
        let Some(&pos) = bool_pos else {
            panic!("no bool field found in default config menu");
        };
        menu.cursor = pos;

        let original_value = menu
            .focused_row()
            .map(|r| r.value.clone())
            .expect("must have focused row");
        let result = menu.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(result, MenuResult::Continue),
            "Enter on bool should return Continue"
        );
        assert_eq!(
            menu.state,
            MenuState::Navigate,
            "state should remain Navigate"
        );
        let new_value = menu
            .focused_row()
            .map(|r| r.value.clone())
            .expect("must have focused row");
        assert_ne!(new_value, original_value, "bool value should have toggled");
    }

    // ── Test 10: Enter on str field opens Edit mode ───────────────────────────

    #[test]
    fn test_enter_on_str_opens_edit() {
        let tmp = TempDir::new().unwrap();
        let mut menu = make_menu(&tmp);

        // Find a str field (core.log_level).
        let positions = menu.all_positions();
        let str_pos = positions.iter().find(|&&(si, ri)| {
            ri >= 0
                && menu
                    .sections
                    .get(si)
                    .and_then(|s| s.rows.get(ri as usize))
                    .map(|r| r.value.is_editable())
                    .unwrap_or(false)
        });
        let Some(&pos) = str_pos else {
            panic!("no editable field found in default config menu");
        };
        menu.cursor = pos;

        menu.handle_key(key(KeyCode::Enter));
        assert_eq!(
            menu.state,
            MenuState::Edit,
            "Enter on editable field should enter Edit mode"
        );
    }

    // ── Test 11: edit commit applies value ────────────────────────────────────

    #[test]
    fn test_edit_commit_applies_value() {
        let tmp = TempDir::new().unwrap();
        let mut menu = make_menu(&tmp);

        // Navigate to a string field (core.log_level is a Str).
        let positions = menu.all_positions();
        let str_pos = positions.iter().find(|&&(si, ri)| {
            ri >= 0
                && menu
                    .sections
                    .get(si)
                    .and_then(|s| s.rows.get(ri as usize))
                    .map(|r| matches!(r.value, ConfigValue::Str(_)))
                    .unwrap_or(false)
        });
        let Some(&pos) = str_pos else {
            panic!("no Str field found");
        };
        menu.cursor = pos;

        // Enter edit mode.
        menu.handle_key(key(KeyCode::Enter));
        assert_eq!(menu.state, MenuState::Edit);

        // Clear the buffer and type a new value.
        menu.edit_buf.clear();
        for c in "debug".chars() {
            menu.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(menu.edit_buf, "debug");

        // Commit.
        menu.handle_key(key(KeyCode::Enter));
        assert_eq!(menu.state, MenuState::Navigate);

        // Value should have updated in the row.
        let value = menu
            .focused_row()
            .map(|r| r.value.clone())
            .expect("must have row");
        assert_eq!(value, ConfigValue::Str("debug".to_string()));
    }

    // ── Test 12: edit cancel leaves unchanged ─────────────────────────────────

    #[test]
    fn test_edit_cancel_leaves_unchanged() {
        let tmp = TempDir::new().unwrap();
        let mut menu = make_menu(&tmp);

        // Navigate to a string field.
        let positions = menu.all_positions();
        let str_pos = positions.iter().find(|&&(si, ri)| {
            ri >= 0
                && menu
                    .sections
                    .get(si)
                    .and_then(|s| s.rows.get(ri as usize))
                    .map(|r| matches!(r.value, ConfigValue::Str(_)))
                    .unwrap_or(false)
        });
        let Some(&pos) = str_pos else {
            panic!("no Str field found");
        };
        menu.cursor = pos;

        let original_value = menu
            .focused_row()
            .map(|r| r.value.clone())
            .expect("must have row");

        // Enter edit mode and type something.
        menu.handle_key(key(KeyCode::Enter));
        menu.edit_buf.clear();
        for c in "garbage_input".chars() {
            menu.handle_key(key(KeyCode::Char(c)));
        }

        // Cancel.
        menu.handle_key(key(KeyCode::Esc));
        assert_eq!(menu.state, MenuState::Navigate);

        // Value should be unchanged.
        let current_value = menu
            .focused_row()
            .map(|r| r.value.clone())
            .expect("must have row");
        assert_eq!(
            current_value, original_value,
            "cancelled edit must not change the value"
        );
    }
}
