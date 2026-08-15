//! Input Trigger System — generalised dropdown architecture.
//!
//! This module provides a registry-based approach to trigger characters in the
//! TUI input bar. Any single character can be registered as a trigger by
//! implementing [`TriggerHandler`] and calling [`TriggerRegistry::register()`].
//!
//! # Architecture
//!
//! ```text
//! TriggerRegistry  (IndexMap<char, Arc<dyn TriggerHandler>>)
//!       │
//!       ├── '@' → MentionTriggerHandler  (file/dir completions)
//!       ├── '/' → SlashCommandTriggerHandler  (slash palette)
//!       └── '$' → SkillTriggerHandler  (skill-only picker, agenthicc parity)
//!       ├── '$' → SkillTriggerHandler  (skill-only picker)
//!       └── '#' → HistoryTriggerHandler  (input history recall)
//!
//! AppState holds: active_trigger: Option<ActiveTrigger>
//!   - Exactly one open at a time
//!   - Replaced (not added) when a new trigger char is typed
//!
//! Key dispatch: handle_trigger_key() → single block regardless of trigger
//! Rendering:    draw_trigger_dropdown() → single helper regardless of trigger
//! ```
//!
//! # Adding a new trigger
//!
//! 1. Implement `TriggerHandler` for your struct (in a new file under `trigger/`).
//! 2. In `AppState::new()`, call `trigger_registry.register(Arc::new(MyHandler::new()))`.
//! 3. Done — zero changes to `handle_key()`, `refresh_trigger()`, `build_prompt()`,
//!    or `draw_prompt_block()`.

pub mod catalog;
pub mod history;
pub mod mention;
pub mod skill;
pub mod slash_command;

use std::sync::Arc;

use indexmap::IndexMap;

// ── MatchKind ─────────────────────────────────────────────────────────────────

/// Semantic category for a [`MatchItem`].
///
/// Used by the renderer to select the appropriate visual style for each row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchKind {
    /// A regular file (shown without trailing `/`).
    File,
    /// A directory (shown with trailing `/`).
    Directory,
    /// A slash command (e.g. `/clear`, `/model`).
    Command,
    /// An emoji or symbol (e.g. from `:` trigger).
    Emoji,
    /// Application-defined category. The `String` payload is used as a label.
    Custom(String),
}

// ── MatchItem ─────────────────────────────────────────────────────────────────

/// A single result row in the trigger dropdown.
///
/// Returned by [`TriggerHandler::matches()`] and stored in [`ActiveTrigger::items`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchItem {
    /// Text displayed in the left column of the dropdown row.
    ///
    /// For `@`-mention: `"src/main.rs"`. For slash: `"clear"`.
    pub display: String,

    /// Text inserted into the input buffer when this item is selected.
    ///
    /// May differ from `display` (e.g. display shows the bare name but insert
    /// is `"@src/main.rs "`).
    pub insert: String,

    /// Optional hint shown below the dropdown (e.g. argument syntax for `/model`).
    ///
    /// When `Some`, an extra line is appended after the last row.
    pub hint: Option<String>,

    /// Semantic category, used to select rendering style.
    pub kind: MatchKind,
}

// ── TriggerContext ────────────────────────────────────────────────────────────

/// Read-only context passed to every handler method.
///
/// Handlers must not mutate application state through this struct.
pub struct TriggerContext<'a> {
    /// Everything the user typed after the trigger character up to the cursor,
    /// with no leading trigger char. E.g. if the buffer contains `"foo @src/ma"`
    /// and the cursor is at the end, `prefix` is `"src/ma"`.
    pub prefix: &'a str,

    /// Directory component of `prefix` — everything up to and including the
    /// last `/`. Empty string (`""`) means the workspace root. Used by
    /// path-aware handlers like [`mention::MentionTriggerHandler`].
    pub dir_prefix: &'a str,

    /// Absolute path to the session working directory.
    pub working_dir: &'a std::path::Path,

    /// Current terminal width in columns. Handlers may use this to truncate
    /// display strings.
    pub terminal_cols: u16,
}

impl<'a> TriggerContext<'a> {
    /// Construct a minimal `TriggerContext` for tests.
    pub fn new_for_test(prefix: &'a str, _trigger_char: char) -> Self {
        use std::path::Path;
        Self {
            prefix,
            dir_prefix: "",
            working_dir: Path::new("/tmp"),
            terminal_cols: 80,
        }
    }
}

// ── TriggerHandler ────────────────────────────────────────────────────────────

/// Protocol every trigger handler must implement.
///
/// Handlers are stored as `Arc<dyn TriggerHandler>` in the registry and
/// must therefore be `Send + Sync`. All methods take `&self` — no mutation
/// of handler state is permitted; any caching must be interior-mutable and
/// thread-safe.
pub trait TriggerHandler: Send + Sync {
    /// The single character that activates this handler.
    ///
    /// Must be unique across all registered handlers. The registry enforces
    /// this at [`TriggerRegistry::register()`] time.
    fn trigger_char(&self) -> char;

    /// Return the list of matching rows for the current `prefix`.
    ///
    /// Called on every `InputAction::BufferChanged` or `InputAction::CursorMoved`
    /// while this trigger is active. Should be cheap (no async I/O). Return an
    /// empty `Vec` to show a "no matches" state (dropdown hidden or greyed out,
    /// handler's choice).
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem>;

    /// Return the string to insert into the buffer when `item` is selected
    /// (Tab / Enter). The returned string replaces everything from the trigger
    /// character position to the current cursor.
    ///
    /// Default: return `item.insert.clone()`.
    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    /// Whether the user can submit the raw typed text even when it doesn't
    /// match any item (Enter without a selection dismisses the dropdown and
    /// sends the raw text as-is).
    ///
    /// `true` for the slash-command handler (bare `/` with no match submits
    /// the literal text to the agent). `false` for the `@`-mention handler
    /// (Enter picks the highlighted candidate or is a no-op when the dropdown
    /// is open).
    fn allows_free_text(&self) -> bool {
        false
    }

    /// Maximum number of rows to show in the dropdown at once.
    ///
    /// Default: `8`.
    fn max_visible(&self) -> usize {
        8
    }

    /// When `true`, the suggestion is rendered inline with the cursor rather
    /// than in a dropdown below the bottom border.
    ///
    /// Reserved for future use (e.g. ghost-text completion). Default: `false`.
    fn is_inline(&self) -> bool {
        false
    }
}

// ── TriggerRegistry ───────────────────────────────────────────────────────────

/// Registry of all registered trigger handlers, keyed by trigger character.
///
/// Built once in `AppState::new()` and stored as a plain (non-`Arc`) field.
/// Handlers themselves are `Arc`-wrapped so they can be shared cheaply.
///
/// Insertion order is preserved by [`IndexMap`], ensuring deterministic
/// iteration when scanning for the active trigger.
pub struct TriggerRegistry {
    /// Preserves insertion order for deterministic iteration.
    handlers: IndexMap<char, Arc<dyn TriggerHandler>>,
}

impl TriggerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: IndexMap::new(),
        }
    }

    /// Register a handler.
    ///
    /// # Panics
    ///
    /// Panics if a handler for `handler.trigger_char()` is already registered.
    /// This is a programming error (duplicate registration), not a runtime condition.
    pub fn register(&mut self, handler: Arc<dyn TriggerHandler>) {
        let ch = handler.trigger_char();
        if self.handlers.contains_key(&ch) {
            panic!(
                "TriggerRegistry: duplicate handler for trigger char {:?}",
                ch
            );
        }
        self.handlers.insert(ch, handler);
    }

    /// Look up the handler for a trigger character, if registered.
    pub fn get(&self, ch: char) -> Option<&Arc<dyn TriggerHandler>> {
        self.handlers.get(&ch)
    }

    /// Replace the handler for `ch`.
    ///
    /// # Panics
    ///
    /// Panics if no handler for `ch` is currently registered. Use
    /// [`register`](Self::register) for new chars.
    pub fn replace(&mut self, ch: char, handler: Arc<dyn TriggerHandler>) {
        assert!(
            self.handlers.contains_key(&ch),
            "TriggerRegistry::replace: no handler for {:?}",
            ch
        );
        self.handlers.insert(ch, handler);
    }

    /// Scan `line` left of `cursor_col` for the nearest registered trigger
    /// character that is followed by no whitespace up to the cursor.
    ///
    /// Returns `None` when the cursor is not immediately after a trigger token,
    /// or when the trigger character found has no registered handler.
    ///
    /// Only the *rightmost* eligible trigger character is considered. When the
    /// user types `"git @src/ma"`, the `@` is found because there is no
    /// whitespace between it and the cursor. When they type `"@ foo"`, the space
    /// after `@` breaks the token and `None` is returned.
    pub fn active_for(&self, line: &str, cursor_col: usize) -> Option<ActiveTriggerScan> {
        let slice = &line[..cursor_col.min(line.len())];

        // Walk backwards to find the rightmost registered trigger char
        // with no intervening whitespace.
        for (byte_pos, ch) in slice.char_indices().rev() {
            if ch.is_whitespace() {
                // Whitespace before finding a trigger char — stop.
                break;
            }
            if self.handlers.contains_key(&ch) {
                let after = &slice[byte_pos + ch.len_utf8()..];
                // Guard: there must be no whitespace between the trigger
                // char and the cursor (already guaranteed by the loop above,
                // but kept explicit for clarity).
                if after.contains(char::is_whitespace) {
                    break;
                }
                let prefix = after.to_owned();
                let dir_prefix = match prefix.rfind('/') {
                    Some(slash) => prefix[..slash + 1].to_owned(),
                    None => String::new(),
                };
                return Some(ActiveTriggerScan {
                    trigger_char: ch,
                    prefix,
                    dir_prefix,
                    trigger_byte_pos: byte_pos,
                });
            }
        }
        None
    }
}

impl Default for TriggerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── ActiveTriggerScan ─────────────────────────────────────────────────────────

/// Result of a successful scan for a trigger character in the current input.
///
/// Produced by [`TriggerRegistry::active_for()`] and stored inside
/// [`ActiveTrigger`].
#[derive(Debug, Clone)]
pub struct ActiveTriggerScan {
    /// The trigger character found (e.g. `'@'`, `'/'`).
    pub trigger_char: char,

    /// Everything the user typed after the trigger character up to the cursor.
    /// Empty string means the cursor is immediately after the trigger char.
    pub prefix: String,

    /// Directory portion of `prefix` — everything up to and including the
    /// last `/`. Empty string means the workspace root.
    pub dir_prefix: String,

    /// Byte offset of the trigger character within the current line.
    /// Used by `on_select` insertion logic to replace the right slice.
    pub trigger_byte_pos: usize,
}

// ── ActiveTrigger ─────────────────────────────────────────────────────────────

/// Live state for the currently-open trigger dropdown.
///
/// `AppState` holds at most one `Option<ActiveTrigger>` at a time.
/// Opening a new trigger (by typing a different trigger char) replaces the
/// existing one.
#[derive(Debug, Clone)]
pub struct ActiveTrigger {
    /// Scan result that produced this active trigger.
    pub scan: ActiveTriggerScan,

    /// Items returned by the handler's most recent `matches()` call.
    pub items: Vec<MatchItem>,

    /// Index of the currently highlighted item (0-based, wraps).
    pub selected: usize,

    /// Index of the first visible item (scroll offset).
    pub scroll_top: usize,
}

impl ActiveTrigger {
    /// The currently highlighted [`MatchItem`], if any.
    pub fn selected_item(&self) -> Option<&MatchItem> {
        self.items.get(self.selected)
    }

    /// Advance selection to the next item (wraps around). Adjusts `scroll_top`
    /// to keep the selected row visible within `max_visible` rows.
    pub fn select_next(&mut self, max_visible: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
        self.clamp_scroll(max_visible);
    }

    /// Move selection to the previous item (wraps around). Adjusts `scroll_top`.
    pub fn select_prev(&mut self, max_visible: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = self.selected.checked_sub(1).unwrap_or(self.items.len() - 1);
        self.clamp_scroll(max_visible);
    }

    /// Clamp `scroll_top` so the selected row is always within the visible window.
    pub fn clamp_scroll(&mut self, max_visible: usize) {
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        } else if self.selected >= self.scroll_top + max_visible {
            self.scroll_top = self.selected + 1 - max_visible;
        }
    }
}

// ── ActiveTriggerSnapshot ─────────────────────────────────────────────────────

/// Renderer-visible snapshot of the active trigger dropdown.
///
/// Produced by `build_prompt()` from `AppState::active_trigger`.
/// Contains no `Arc`s and no references — fully owned, safe to clone across
/// render boundaries.
#[derive(Debug, Clone)]
pub struct ActiveTriggerSnapshot {
    /// Which trigger character is active (used to select rendering style).
    pub trigger_char: char,

    /// Items to display (cloned from [`ActiveTrigger::items`]).
    pub items: Vec<MatchItem>,

    /// Index of the highlighted item.
    pub selected: usize,

    /// First visible item index (scroll offset).
    pub scroll_top: usize,

    /// Maximum number of rows to show (from [`TriggerHandler::max_visible()`]).
    pub max_visible: usize,

    /// Hint text to show below the last visible row (from the selected item's
    /// [`MatchItem::hint`] field, or `None`).
    pub hint: Option<String>,
}

// Compatibility alias — tests and pre-merge handler impls used this name.
pub use TriggerHandler as LocalTriggerHandler;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper trigger for unit tests ─────────────────────────────────────────

    struct EchoTrigger;

    impl TriggerHandler for EchoTrigger {
        fn trigger_char(&self) -> char {
            '!'
        }

        fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
            if ctx.prefix.is_empty() {
                return vec![];
            }
            vec![MatchItem {
                display: ctx.prefix.to_owned(),
                insert: format!("!{} ", ctx.prefix),
                hint: Some(format!("echo: {}", ctx.prefix)),
                kind: MatchKind::Custom("echo".into()),
            }]
        }
    }

    // ── TriggerRegistry::register / get ───────────────────────────────────────

    #[test]
    fn registry_register_and_get() {
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(EchoTrigger));
        assert!(reg.get('!').is_some());
        assert!(reg.get('@').is_none());
    }

    #[test]
    #[should_panic(expected = "duplicate handler")]
    fn registry_duplicate_panics() {
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(EchoTrigger));
        reg.register(Arc::new(EchoTrigger)); // second '!' — must panic
    }

    // ── TriggerRegistry::active_for ───────────────────────────────────────────

    #[test]
    fn active_for_finds_at_trigger() {
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(EchoTrigger));
        // "say !hello" — cursor after 'o' (col=10)
        let result = reg.active_for("say !hello", 10);
        assert!(result.is_some());
        let scan = result.unwrap();
        assert_eq!(scan.trigger_char, '!');
        assert_eq!(scan.prefix, "hello");
    }

    #[test]
    fn active_for_stops_at_whitespace() {
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(EchoTrigger));
        // "! hello" — whitespace immediately after '!'
        assert!(reg.active_for("! hello", 7).is_none());
    }

    #[test]
    fn active_for_returns_none_when_no_trigger() {
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(EchoTrigger));
        assert!(reg.active_for("no trigger here", 15).is_none());
    }

    #[test]
    fn active_for_returns_rightmost() {
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(EchoTrigger));
        // "!foo !bar" — cursor at end; rightmost '!' is the active one
        let result = reg.active_for("!foo !bar", 9);
        let scan = result.unwrap();
        assert_eq!(scan.prefix, "bar");
        assert_eq!(scan.trigger_byte_pos, 5);
    }

    #[test]
    fn active_for_slash_at_start() {
        struct SlashTrigger;
        impl TriggerHandler for SlashTrigger {
            fn trigger_char(&self) -> char {
                '/'
            }
            fn matches(&self, _ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
                vec![]
            }
        }
        let mut reg = TriggerRegistry::new();
        reg.register(Arc::new(SlashTrigger));
        let result = reg.active_for("/clear", 6);
        assert!(result.is_some());
        let scan = result.unwrap();
        assert_eq!(scan.trigger_char, '/');
        assert_eq!(scan.prefix, "clear");
        assert_eq!(scan.trigger_byte_pos, 0);
    }

    // ── ActiveTrigger::select_next / select_prev ──────────────────────────────

    #[test]
    fn select_next_wraps() {
        let items = vec![
            MatchItem {
                display: "a".into(),
                insert: "a".into(),
                hint: None,
                kind: MatchKind::File,
            },
            MatchItem {
                display: "b".into(),
                insert: "b".into(),
                hint: None,
                kind: MatchKind::File,
            },
        ];
        let mut at = ActiveTrigger {
            scan: ActiveTriggerScan {
                trigger_char: '!',
                prefix: "".into(),
                dir_prefix: "".into(),
                trigger_byte_pos: 0,
            },
            items,
            selected: 1,
            scroll_top: 0,
        };
        at.select_next(8); // wraps from index 1 back to 0
        assert_eq!(at.selected, 0);
    }

    #[test]
    fn select_prev_wraps() {
        let items = vec![
            MatchItem {
                display: "a".into(),
                insert: "a".into(),
                hint: None,
                kind: MatchKind::File,
            },
            MatchItem {
                display: "b".into(),
                insert: "b".into(),
                hint: None,
                kind: MatchKind::File,
            },
        ];
        let mut at = ActiveTrigger {
            scan: ActiveTriggerScan {
                trigger_char: '!',
                prefix: "".into(),
                dir_prefix: "".into(),
                trigger_byte_pos: 0,
            },
            items,
            selected: 0,
            scroll_top: 0,
        };
        at.select_prev(8); // wraps from index 0 to last (1)
        assert_eq!(at.selected, 1);
    }

    #[test]
    fn clamp_scroll_adjusts() {
        let items: Vec<MatchItem> = (0..10)
            .map(|i| MatchItem {
                display: i.to_string(),
                insert: i.to_string(),
                hint: None,
                kind: MatchKind::File,
            })
            .collect();
        let mut at = ActiveTrigger {
            scan: ActiveTriggerScan {
                trigger_char: '!',
                prefix: "".into(),
                dir_prefix: "".into(),
                trigger_byte_pos: 0,
            },
            items,
            selected: 0,
            scroll_top: 0,
        };
        // Select item 8 with max_visible=5 — scroll_top should advance.
        at.selected = 8;
        at.clamp_scroll(5);
        assert!(
            at.scroll_top > 0,
            "scroll_top should advance when selected is below view"
        );
        assert!(at.selected < at.scroll_top + 5, "selected must be in view");
    }
}
