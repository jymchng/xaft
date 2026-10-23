//! Menu widget system for the xaft TUI.
//!
//! Provides a `MenuWidget` trait for interactive overlays rendered below the
//! input bar, a `MenuDriver` that manages the active widget lifecycle, and a
//! `CommandMenuRegistry` that maps slash command names to widget factories.
//!
//! # Architecture
//!
//! ```text
//! CommandMenuRegistry  (HashMap<name, MenuFactory>)
//!       │
//!       └── On command match → create Box<dyn MenuWidget>
//!                                     │
//!                              MenuDriver::open(widget)
//!                                     │
//!                   AppState::handle_key → MenuDriver::handle_key
//!                                     │
//!                              MenuResult returned
//!                                     │
//!                   AppState::handle_menu_result
//! ```

pub mod config_menu;
pub mod dropdown;

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::KeyEvent;

// ── MenuPayload ───────────────────────────────────────────────────────────────

/// Data produced when a menu completes successfully.
#[derive(Debug)]
pub enum MenuPayload {
    /// A text string was selected (e.g. for insertion into the input bar).
    Selected(String),
    /// A key-value pair was submitted (e.g. config set).
    KeyValue { key: String, value: String },
    /// The menu closed after an action; commit this line to the transcript
    /// (e.g. `/loops` → Enter → "Loop … is due now.").
    Notice(String),
    /// No payload — the menu was acknowledged but produced no data.
    Empty,
}

// ── MenuResult ────────────────────────────────────────────────────────────────

/// Result returned from `MenuWidget::handle_key` (and forwarded by `MenuDriver`).
#[derive(Debug)]
pub enum MenuResult {
    /// The menu produced a result and should be closed.
    Done(MenuPayload),
    /// The user dismissed the menu without selecting anything.
    Cancel,
    /// The key was consumed; remain open with no payload yet.
    Continue,
}

// ── Overlay rows ──────────────────────────────────────────────────────────────

/// Rows an overlay may occupy below the prompt's bottom border.
///
/// The same budget the trigger dropdown uses (`MAX_TRIGGER` in
/// `renderer.rs`'s `MAX_PROMPT_RESERVED_ROWS`): the renderer reserves a fixed
/// block at the bottom of the terminal, and an overlay that drew more rows
/// would scroll into the transcript region. An overlay replaces the dropdown
/// while it is open — the two cannot be visible at once, because an open
/// overlay captures every key.
pub const OVERLAY_MAX_ROWS: usize = 11;

/// Visual weight of one overlay row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayStyle {
    /// The overlay's title row.
    Title,
    /// An ordinary row.
    Normal,
    /// The row under the selection cursor.
    Selected,
    /// Hints and secondary detail.
    Dim,
    /// A confirmation prompt or a refusal the user must notice.
    Warning,
}

/// One row of an overlay, drawn literally (no markup is interpreted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayLine {
    /// The row text. The renderer truncates it to the terminal width.
    pub text: String,
    /// How the renderer styles the row.
    pub style: OverlayStyle,
}

impl OverlayLine {
    /// A row with the given style.
    pub fn new(text: impl Into<String>, style: OverlayStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

// ── MenuWidget ────────────────────────────────────────────────────────────────

/// An interactive overlay widget rendered below the input bar.
///
/// # Object safety
///
/// The trait is object-safe — `render` takes `&mut dyn Write` rather than
/// `&mut impl TermWriter` so that `Box<dyn MenuWidget>` is well-formed.
pub trait MenuWidget: Send + Sync {
    /// Draw the widget into `out`, given terminal `size` and the number of rows
    /// that were drawn last frame (`prev_rows`). Returns the number of rows drawn.
    fn render(&self, out: &mut dyn Write, size: (u16, u16), prev_rows: usize) -> io::Result<usize>;

    /// Handle a key event. Returns a `MenuResult` describing what happened.
    fn handle_key(&mut self, key: KeyEvent) -> MenuResult;

    /// If the widget is editing a field, return the current field value so the
    /// renderer can mirror it in the input bar's visual placeholder.
    fn edit_field_value(&self) -> Option<&str> {
        None
    }

    /// Human-readable title for the overlay header.
    fn title(&self) -> &str;

    /// The rows to draw in the prompt's bottom block, at most `max_rows`.
    ///
    /// This is how an overlay becomes visible in the append-only renderer:
    /// `build_prompt` copies these rows into `PromptState::overlay` and the
    /// renderer draws them below the input bar, where the trigger dropdown
    /// would otherwise go. `None` (the default) means the widget is not drawn
    /// by the renderer at all.
    fn overlay_lines(&self, cols: u16, max_rows: usize) -> Option<Vec<OverlayLine>> {
        let _ = (cols, max_rows);
        None
    }
}

// ── MenuDriver ────────────────────────────────────────────────────────────────

/// Manages the lifecycle of the active `MenuWidget`.
///
/// At most one widget is active at a time. `open` replaces any existing widget.
/// The driver automatically closes the widget when it returns `Done` or `Cancel`.
pub struct MenuDriver {
    widget: Option<Box<dyn MenuWidget>>,
    last_rows: usize,
}

impl Default for MenuDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuDriver {
    /// Create a new, inactive driver.
    pub fn new() -> Self {
        Self {
            widget: None,
            last_rows: 0,
        }
    }

    /// Open a widget, replacing any existing one.
    pub fn open(&mut self, widget: Box<dyn MenuWidget>) {
        self.widget = Some(widget);
        self.last_rows = 0;
    }

    /// Close the active widget (if any).
    pub fn close(&mut self) {
        self.widget = None;
        self.last_rows = 0;
    }

    /// Returns `true` when a widget is currently active.
    pub fn is_active(&self) -> bool {
        self.widget.is_some()
    }

    /// Forward a key event to the active widget. Returns `None` when no widget
    /// is open. Automatically closes the widget when it returns `Done` or `Cancel`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<MenuResult> {
        let widget = self.widget.as_mut()?;
        let result = widget.handle_key(key);
        match &result {
            MenuResult::Done(_) | MenuResult::Cancel => {
                self.close();
            }
            MenuResult::Continue => {}
        }
        Some(result)
    }

    /// Render the active widget into `out`. No-op when no widget is open.
    pub fn render(&mut self, out: &mut dyn Write, size: (u16, u16)) -> io::Result<()> {
        if let Some(widget) = &self.widget {
            let rows = widget.render(out, size, self.last_rows)?;
            self.last_rows = rows;
        }
        Ok(())
    }

    /// Number of rows drawn by the last `render` call.
    pub fn last_rows(&self) -> usize {
        self.last_rows
    }

    /// Borrow the active widget (if any).
    pub fn widget(&self) -> Option<&dyn MenuWidget> {
        self.widget.as_deref()
    }
}

// ── CommandMenuContext ────────────────────────────────────────────────────────

/// Context passed to a `MenuFactory` when a menu command is invoked.
pub struct CommandMenuContext {
    /// Resolved application config at the time of invocation.
    pub config: Arc<xaft_config::XaftConfig>,
    /// Working directory for the current session.
    pub working_dir: PathBuf,
    /// Active session ID, if any.
    pub session_id: Option<String>,
}

// ── CommandMenuRegistry ───────────────────────────────────────────────────────

/// A factory closure that creates a `MenuWidget` from a `CommandMenuContext`.
pub type MenuFactory =
    Box<dyn Fn(CommandMenuContext) -> Box<dyn MenuWidget> + Send + Sync + 'static>;

/// Maps command names to `MenuFactory` closures.
///
/// When a command name is found in the registry, its factory is called to
/// produce a `Box<dyn MenuWidget>` which is then opened via `MenuDriver`.
pub struct CommandMenuRegistry {
    factories: HashMap<String, Arc<MenuFactory>>,
}

impl Default for CommandMenuRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandMenuRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a factory for `command`. Replaces any existing entry.
    pub fn register(&mut self, command: &str, factory: MenuFactory) {
        self.factories
            .insert(command.to_string(), Arc::new(factory));
    }

    /// Look up a factory by command name.
    pub fn get(&self, command: &str) -> Option<Arc<MenuFactory>> {
        self.factories.get(command).cloned()
    }

    /// Returns `true` when no commands are registered.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// Sorted list of registered command names.
    pub fn commands(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.factories.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::*;

    // ── Minimal test widget ───────────────────────────────────────────────────

    /// A widget that:
    /// - Returns `Continue` on most keys.
    /// - Returns `Done(Empty)` on Enter.
    /// - Returns `Cancel` on Esc.
    pub(crate) struct EchoWidget {
        title: &'static str,
    }

    impl EchoWidget {
        pub(crate) fn new() -> Self {
            Self { title: "echo" }
        }
    }

    impl MenuWidget for EchoWidget {
        fn render(
            &self,
            out: &mut dyn Write,
            _size: (u16, u16),
            _prev_rows: usize,
        ) -> io::Result<usize> {
            out.write_all(b"[EchoWidget]\n")?;
            Ok(1)
        }

        fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
            match key.code {
                KeyCode::Enter => MenuResult::Done(MenuPayload::Empty),
                KeyCode::Esc => MenuResult::Cancel,
                _ => MenuResult::Continue,
            }
        }

        fn title(&self) -> &str {
            self.title
        }
    }

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    // ── MenuDriver tests ──────────────────────────────────────────────────────

    #[test]
    fn test_menu_driver_starts_inactive() {
        let driver = MenuDriver::new();
        assert!(!driver.is_active());
    }

    #[test]
    fn test_menu_driver_open_makes_active() {
        let mut driver = MenuDriver::new();
        driver.open(Box::new(EchoWidget::new()));
        assert!(driver.is_active());
    }

    #[test]
    fn test_menu_driver_close() {
        let mut driver = MenuDriver::new();
        driver.open(Box::new(EchoWidget::new()));
        driver.close();
        assert!(!driver.is_active());
    }

    #[test]
    fn test_menu_driver_handle_key_continues() {
        let mut driver = MenuDriver::new();
        driver.open(Box::new(EchoWidget::new()));
        let result = driver.handle_key(make_key(KeyCode::Char('a')));
        assert!(matches!(result, Some(MenuResult::Continue)));
        assert!(
            driver.is_active(),
            "driver should remain active on Continue"
        );
    }

    #[test]
    fn test_menu_driver_auto_closes_on_done() {
        let mut driver = MenuDriver::new();
        driver.open(Box::new(EchoWidget::new()));
        let result = driver.handle_key(make_key(KeyCode::Enter));
        assert!(matches!(result, Some(MenuResult::Done(_))));
        assert!(!driver.is_active(), "driver should be closed after Done");
    }

    #[test]
    fn test_menu_driver_auto_closes_on_cancel() {
        let mut driver = MenuDriver::new();
        driver.open(Box::new(EchoWidget::new()));
        let result = driver.handle_key(make_key(KeyCode::Esc));
        assert!(matches!(result, Some(MenuResult::Cancel)));
        assert!(!driver.is_active(), "driver should be closed after Cancel");
    }

    #[test]
    fn test_menu_driver_no_active_returns_none() {
        let mut driver = MenuDriver::new();
        let result = driver.handle_key(make_key(KeyCode::Enter));
        assert!(result.is_none(), "no widget => None");
    }

    // ── CommandMenuRegistry tests ─────────────────────────────────────────────

    #[test]
    fn test_command_menu_registry_register_get() {
        let mut reg = CommandMenuRegistry::new();
        reg.register(
            "test",
            Box::new(|_ctx| Box::new(EchoWidget::new()) as Box<dyn MenuWidget>),
        );
        assert!(reg.get("test").is_some());
    }

    #[test]
    fn test_command_menu_registry_missing_returns_none() {
        let reg = CommandMenuRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_command_menu_registry_commands_sorted() {
        let mut reg = CommandMenuRegistry::new();
        reg.register(
            "zeta",
            Box::new(|_ctx| Box::new(EchoWidget::new()) as Box<dyn MenuWidget>),
        );
        reg.register(
            "alpha",
            Box::new(|_ctx| Box::new(EchoWidget::new()) as Box<dyn MenuWidget>),
        );
        reg.register(
            "mu",
            Box::new(|_ctx| Box::new(EchoWidget::new()) as Box<dyn MenuWidget>),
        );
        let cmds = reg.commands();
        assert_eq!(cmds, vec!["alpha", "mu", "zeta"]);
    }
}
