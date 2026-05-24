# XAFT Keyboard Workflows

## Keyboard-First Design Philosophy

xaft is designed for developers who live in the terminal. Every action is accessible
via keyboard. The mouse is supported for resize and click navigation, but the primary
interaction model is entirely keyboard-driven, inspired by vim, tmux, and Emacs.

### Design Principles

1. **Home row优先**: Most common actions map to keys reachable without moving the right hand from the home row
2. **Modal editing**: Different keybindings in different modes (Normal, Insert, Command)
3. **Discoverable**: Command palette (:) and which-key popup show available bindings
4. **Consistent**: Same key always does the same conceptual action across panes
5. **Configurable**: All keybindings are user-configurable via TOML

## Complete Keybinding Reference

### Global Keybindings (All Modes)

| Key | Action | Description |
|---|---|---|
| `Esc` | Cancel / Close overlay | Returns to previous mode or closes dialog |
| `Ctrl+C` | Interrupt agent | Sends interrupt signal to the running agent |
| `Ctrl+D` | Quit xaft | Confirms quit if agent is running |
| `Ctrl+L` | Toggle log console | Shows/hides the LogConsole pane |
| `Ctrl+T` | Toggle timeline | Shows/hides the Timeline pane |
| `Ctrl+Z` | Suspend xaft | SIGTSTP — suspends to background |
| `F1` | Help | Shows keybinding reference |
| `F2` | Rename session | Edits the session name |
| `F5` | Refresh file tree | Force-refreshes the FileTree pane |
| `F12` | Debug info | Shows TUI debug overlay (fps, buffer size) |

### Normal Mode Keybindings

Normal mode is the default. Most navigation and action keys work here.

#### Pane Navigation

| Key | Action | Description |
|---|---|---|
| `Tab` | Next pane | Cycle focus to next pane |
| `Shift+Tab` | Previous pane | Cycle focus to previous pane |
| `Ctrl+H` | Focus left | Move focus to the pane to the left |
| `Ctrl+J` | Focus down | Move focus to the pane below |
| `Ctrl+K` | Focus up | Move focus to the pane above |
| `Ctrl+L` | Focus right | Move focus to the pane to the right |
| `1`-`9` | Jump to pane | Focus pane by index (1=Chat, 2=Diff, 3=Agents, etc.) |
| `0` | Focus chat | Quick jump to chat pane (primary) |

#### Pane Resize

| Key | Action | Description |
|---|---|---|
| `Alt+H` | Shrink left | Decrease left pane width by 3% |
| `Alt+L` | Expand right | Increase left pane width by 3% |
| `Alt+J` | Expand down | Increase top pane height by 3% |
| `Alt+K` | Shrink up | Decrease top pane height by 3% |
| `Alt+=` | Equalize splits | Reset all split ratios to 50/50 |
| `Alt+0` | Reset layout | Restore default layout preset |

#### Layout Presets

| Key | Action | Description |
|---|---|---|
| `Alt+1` | Default layout | Chat 65% + sidebar 35% |
| `Alt+2` | Focus layout | Chat 90% + minimal sidebar |
| `Alt+3` | Review layout | Chat 40% + Diff 60% |
| `Alt+4` | Debug layout | Chat 40% + Log 30% + Diff 30% |
| `Alt+5` | Monitor layout | Agents 40% + Tokens 30% + Log 30% |

#### Chat Pane

| Key | Action | Description |
|---|---|---|
| `i` | Enter insert mode | Start typing a message to the agent |
| `Enter` | Enter insert mode | Same as `i` (when not on an actionable element) |
| `j` / `↓` | Scroll down | Scroll chat history down |
| `k` / `↑` | Scroll up | Scroll chat history up |
| `G` | Scroll to bottom | Jump to latest message |
| `gg` | Scroll to top | Jump to first message |
| `Ctrl+U` | Scroll half-page up | Scroll by 50% of visible area |
| `Ctrl+D` | Scroll half-page down | Scroll by 50% of visible area |
| `y` | Yank message | Copy last agent message to clipboard |
| `Y` | Yank code block | Copy the last code block from agent output |
| `c` | Continue | Send "continue" to resume agent |
| `r` | Retry | Retry the last failed agent action |
| `/` | Search | Search within chat history |
| `n` | Next search result | Jump to next match |
| `N` | Previous search result | Jump to previous match |

#### Diff Pane

| Key | Action | Description |
|---|---|---|
| `j` / `↓` | Next line | Move down one line in the diff |
| `k` / `↑` | Previous line | Move up one line in the diff |
| `n` | Next hunk | Jump to next diff hunk |
| `N` | Previous hunk | Jump to previous diff hunk |
| `]` | Next file | Jump to next file in multi-file diff |
| `[` | Previous file | Jump to previous file |
| `Tab` | Toggle mode | Switch between unified and side-by-side |
| `a` | Approve hunk | Approve the current diff hunk (if pending) |
| `r` | Reject hunk | Reject the current diff hunk |
| `e` | Edit hunk | Open hunk in $EDITOR |
| `A` | Approve all | Approve all hunks in current file |
| `R` | Reject all | Reject all hunks in current file |
| `v` | View full file | Open the full file in a viewer |
| `=` / `-` | More/less context | Adjust context lines around hunks |
| `o` | Open in editor | Open the file in $EDITOR at hunk line |

#### Agent Activity Pane

| Key | Action | Description |
|---|---|---|
| `j` / `↓` | Next agent | Move selection down in agent tree |
| `k` / `↑` | Previous agent | Move selection up in agent tree |
| `Enter` | Expand/collapse | Toggle subtree visibility |
| `zc` | Collapse | Collapse selected subtree |
| `zo` | Open/expand | Expand selected subtree |
| `za` | Toggle fold | Toggle collapse/expand |
| `zM` | Collapse all | Collapse all subtrees |
| `zR` | Expand all | Expand all subtrees |
| `d` | Agent details | Show detailed info for selected agent |
| `Ctrl+C` | Cancel agent | Cancel the selected agent's current task |
| `s` | Switch to agent | Focus chat pane with context from selected agent |

#### File Tree Pane

| Key | Action | Description |
|---|---|---|
| `j` / `↓` | Next file | Move selection down |
| `k` / `↑` | Previous file | Move selection up |
| `Enter` / `l` | Open/expand | Expand directory or open file in diff |
| `h` | Collapse / parent | Collapse directory or go to parent dir |
| `o` | Open in editor | Open file in $EDITOR |
| `d` | Show diff | Show diff for this file (if modified) |
| `/` | Filter | Filter file tree by name pattern |
| `.` | Toggle hidden | Show/hide dotfiles |
| `r` | Refresh | Refresh file tree from disk |

#### Token Dashboard Pane

| Key | Action | Description |
|---|---|---|
| `B` | Set budget | Open budget configuration dialog |
| `s` | Sort by | Change sort order (cost, tokens, calls) |
| `h` | Toggle history | Show/hide cost history sparkline |
| `c` | Toggle compact | Switch between full and compact view |

### Insert Mode Keybindings

Insert mode is for typing messages to the agent. It uses standard text editing keys:

| Key | Action | Description |
|---|---|---|
| `Esc` | Exit insert mode | Return to normal mode |
| `Enter` | Send message | Send the typed message to the agent |
| `Shift+Enter` | New line | Insert a newline (multi-line message) |
| `Ctrl+Enter` | Send message | Alternative send (for terminals where Shift+Enter doesn't work) |
| `Backspace` | Delete backward | Delete character before cursor |
| `Delete` | Delete forward | Delete character after cursor |
| `Ctrl+W` | Delete word backward | Delete previous word |
| `Ctrl+U` | Delete to start | Delete from cursor to start of line |
| `Ctrl+K` | Delete to end | Delete from cursor to end of line |
| `Left/Right` | Move cursor | Move cursor left/right |
| `Home/End` | Line start/end | Move cursor to start/end of line |
| `Ctrl+A` | Line start | Move cursor to start of line |
| `Ctrl+E` | Line end | Move cursor to end of line |
| `Alt+Backspace` | Delete word backward | Delete previous word (alternative) |
| `Ctrl+V` | Paste | Paste from system clipboard |
| `Up/Down` | History | Navigate message history |
| `Tab` | Autocomplete | Autocomplete file paths, tool names |

### Command Mode Keybindings

Command mode is entered by pressing `:` from normal mode. It provides a command palette
for less-frequent actions:

| Command | Action | Description |
|---|---|---|
| `:q` | Quit | Quit xaft |
| `:qa` | Quit all | Quit xaft (force, even with running agent) |
| `:w` | Save session | Save the current session to disk |
| `:budget <amount>` | Set budget | Set the session budget limit |
| `:model <name>` | Switch model | Change the default model |
| `:layout <preset>` | Set layout | Switch to a layout preset |
| `:approve-all` | Approve all | Approve all pending approvals |
| `:reject-all` | Reject all | Reject all pending approvals |
| `:clear` | Clear chat | Clear chat history display |
| `:reset` | Reset session | Reset the entire session |
| `:theme <name>` | Set theme | Change the color theme |
| `:keymap <file>` | Load keymap | Load a custom keybinding file |
| `:export <format>` | Export session | Export session (markdown, json) |
| `:help` | Help | Show help |
| `:debug` | Debug overlay | Toggle debug information |

## Modal Editing Implementation

### Mode State Machine

```rust
/// TUI interaction modes
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// Default navigation mode (vim-inspired)
    Normal,
    /// Typing a message to the agent
    Insert,
    /// Command palette mode (after pressing :)
    Command,
    /// Waiting for approval decision
    Approval,
    /// Searching within a pane
    Search,
}

/// Mode transition logic
impl AppState {
    pub fn transition_mode(&mut self, event: &KeyEvent) -> ModeTransition {
        let current = self.mode;

        match (current, event.code, event.modifiers) {
            // Normal → Insert
            (Mode::Normal, KeyCode::Char('i'), KeyModifiers::NONE) => {
                self.mode = Mode::Insert;
                ModeTransition::EnterMode(Mode::Insert)
            }
            (Mode::Normal, KeyCode::Enter, KeyModifiers::NONE)
                if self.focused_pane_type() == PaneType::Chat => {
                self.mode = Mode::Insert;
                ModeTransition::EnterMode(Mode::Insert)
            }

            // Normal → Command
            (Mode::Normal, KeyCode::Char(':'), KeyModifiers::NONE) => {
                self.mode = Mode::Command;
                self.command_buffer.clear();
                ModeTransition::EnterMode(Mode::Command)
            }

            // Normal → Search
            (Mode::Normal, KeyCode::Char('/'), KeyModifiers::NONE) => {
                self.mode = Mode::Search;
                self.search_buffer.clear();
                ModeTransition::EnterMode(Mode::Search)
            }

            // Normal → Approval (automatic, when approval arrives)
            (Mode::Normal, _, _) if self.approval_queue.has_pending() => {
                self.mode = Mode::Approval;
                ModeTransition::AutoEnter(Mode::Approval)
            }

            // Any → Normal (Esc)
            (_, KeyCode::Esc, KeyModifiers::NONE) => {
                self.mode = Mode::Normal;
                ModeTransition::EnterMode(Mode::Normal)
            }

            // Insert → Normal (Esc)
            (Mode::Insert, KeyCode::Esc, KeyModifiers::NONE) => {
                self.mode = Mode::Normal;
                ModeTransition::EnterMode(Mode::Normal)
            }

            // Command → Execute (Enter)
            (Mode::Command, KeyCode::Enter, KeyModifiers::NONE) => {
                let cmd = self.command_buffer.clone();
                self.mode = Mode::Normal;
                ModeTransition::ExecuteCommand(cmd)
            }

            // Search → Execute (Enter)
            (Mode::Search, KeyCode::Enter, KeyModifiers::NONE) => {
                let query = self.search_buffer.clone();
                self.mode = Mode::Normal;
                ModeTransition::ExecuteSearch(query)
            }

            // Approval → Normal (after resolving)
            (Mode::Approval, KeyCode::Char('a') | KeyCode::Char('r'), KeyModifiers::NONE) => {
                // Approval is resolved, return to normal
                self.mode = Mode::Normal;
                ModeTransition::EnterMode(Mode::Normal)
            }

            _ => ModeTransition::NoChange,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModeTransition {
    NoChange,
    EnterMode(Mode),
    AutoEnter(Mode),
    ExecuteCommand(String),
    ExecuteSearch(String),
}
```

### Mode Indicator in Status Bar

```
── NORMAL ──────────────────────────────────────────────── 12:34:56 ──
── INSERT ──────────────────────────────────────────────── 12:34:56 ──
── COMMAND ─────────────────────────────────────────────── 12:34:56 ──
── SEARCH ──────────────────────────────────────────────── 12:34:56 ──
── APPROVAL ────────────────────────────────────────────── 12:34:56 ──
```

```rust
/// Render mode indicator in status bar
fn render_mode_indicator(mode: Mode, area: Rect, buf: &mut Buffer) {
    let (label, color) = match mode {
        Mode::Normal   => ("NORMAL",   Color::Green),
        Mode::Insert   => ("INSERT",   Color::Cyan),
        Mode::Command  => ("COMMAND",  Color::Yellow),
        Mode::Search   => ("SEARCH",   Color::Magenta),
        Mode::Approval => ("APPROVAL", Color::Red),
    };

    let span = Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
    span.render(area, buf);
}
```

## Vim-Inspired Navigation

### gg (Double-Press g)

xaft supports vim-style double-key chords for certain actions:

```rust
/// Key chord handler for vim-style multi-key bindings
pub struct KeyChordHandler {
    /// First key in a potential chord
    pending: Option<KeyEvent>,

    /// Timeout for chord completion (default: 500ms)
    timeout: Duration,

    /// When the first key was pressed
    pending_time: Option<Instant>,
}

impl KeyChordHandler {
    pub fn new() -> Self {
        Self {
            pending: None,
            timeout: Duration::from_millis(500),
            pending_time: None,
        }
    }

    /// Process a key event. Returns the resolved action.
    pub fn process(&mut self, key: KeyEvent) -> ChordResult {
        // Check if pending key has expired
        if let Some(time) = self.pending_time {
            if time.elapsed() > self.timeout {
                self.pending = None;
                self.pending_time = None;
            }
        }

        // Check for chord completion
        if let Some(pending) = self.pending.take() {
            self.pending_time = None;

            // Match known chords
            match (pending.code, key.code) {
                (KeyCode::Char('g'), KeyCode::Char('g')) => return ChordResult::Action(Action::ScrollToTop),
                (KeyCode::Char('z'), KeyCode::Char('c')) => return ChordResult::Action(Action::Collapse),
                (KeyCode::Char('z'), KeyCode::Char('o')) => return ChordResult::Action(Action::Expand),
                (KeyCode::Char('z'), KeyCode::Char('a')) => return ChordResult::Action(Action::ToggleFold),
                (KeyCode::Char('z'), KeyCode::Char('M')) => return ChordResult::Action(Action::CollapseAll),
                (KeyCode::Char('z'), KeyCode::Char('R')) => return ChordResult::Action(Action::ExpandAll),
                _ => {
                    // Not a recognized chord — process both keys individually
                    return ChordResult::TwoActions(
                        self.key_to_action(pending),
                        self.key_to_action(key),
                    );
                }
            }
        }

        // Check if this key could start a chord
        match key.code {
            KeyCode::Char('g') | KeyCode::Char('z') => {
                self.pending = Some(key);
                self.pending_time = Some(Instant::now());
                ChordResult::Pending
            }
            _ => ChordResult::Action(self.key_to_action(key)),
        }
    }
}

pub enum ChordResult {
    /// Waiting for second key in chord
    Pending,
    /// Single action resolved
    Action(Action),
    /// Two individual actions (chord didn't match)
    TwoActions(Action, Action),
}
```

## Accessibility

### Screen Reader Considerations

xaft supports screen reader accessibility through:

1. **Descriptive status line**: The status bar always contains a text description of the current state
2. **Aria-like announcements**: State changes are written to a separate "announcement" buffer that screen readers can poll
3. **Keyboard-only operation**: No action requires a mouse
4. **Consistent navigation**: Tab/Shift+Tab always move predictably

```rust
/// Accessibility announcement buffer
pub struct AccessibilityAnnouncer {
    /// Announcements waiting to be read by screen reader
    queue: VecDeque<String>,

    /// Last announcement (to avoid repeats)
    last: String,
}

impl AccessibilityAnnouncer {
    /// Announce a state change
    pub fn announce(&mut self, message: &str) {
        if message != self.last {
            self.queue.push_back(message.to_string());
            self.last = message.to_string();
        }
    }

    /// Get the next announcement (called by screen reader integration)
    pub fn next(&mut self) -> Option<String> {
        self.queue.pop_front()
    }
}

// Example announcements:
// "Approval required for FileEditor on src/auth/token.rs. Risk: medium."
// "Agent file-editor-01 completed. Duration: 2.1 seconds."
// "Token count: 124,500. Cost: $1.87."
// "Mode changed to insert."
```

### High-Contrast Mode

```rust
/// High-contrast color palette for accessibility
pub mod high_contrast {
    use ratatui::style::Color;

    pub const BG: Color = Color::Black;
    pub const FG: Color = Color::White;
    pub const ACCENT: Color = Color::Yellow;
    pub const SUCCESS: Color = Color::Green;
    pub const WARNING: Color = Color::Yellow;
    pub const ERROR: Color = Color::Red;
    pub const MUTED: Color = Color::White;
    pub const DIM: Color = Color::Gray;

    // All colors are maximum contrast — no dark grays, no subtle backgrounds
    pub const DIFF_ADD_BG: Color = Color::Black;      // Use text color instead
    pub const DIFF_ADD_FG: Color = Color::Green;
    pub const DIFF_REMOVE_BG: Color = Color::Black;
    pub const DIFF_REMOVE_FG: Color = Color::Red;
    pub const BORDER: Color = Color::White;
    pub const FOCUS_BORDER: Color = Color::Yellow;
}

/// Detect if high-contrast mode should be enabled
pub fn should_use_high_contrast() -> bool {
    // Check environment variable
    if std::env::var("XAFT_HIGH_CONTRAST").is_ok() {
        return true;
    }

    // Check terminal preference (some terminals signal this)
    if std::env::var("NO_COLOR").is_ok() {
        return true;
    }

    // Check for Windows High Contrast mode
    #[cfg(windows)]
    {
        // Windows API check for high contrast
        // SystemParametersInfo(SPI_GETHIGHCONTRAST, ...)
    }

    false
}
```

## Keybinding Configuration

### Configuration File Format

```toml
# ~/.config/xaft/keybindings.toml

# Global keybindings (override defaults)
[global]
quit = "Ctrl+D"
cancel = "Esc"
interrupt = "Ctrl+C"
help = "F1"
command_palette = ":"
toggle_log = "Ctrl+L"
toggle_timeline = "Ctrl+T"

# Normal mode keybindings
[normal]
# Pane navigation
next_pane = "Tab"
prev_pane = "Shift+Tab"
focus_left = "Ctrl+H"
focus_down = "Ctrl+J"
focus_up = "Ctrl+K"
focus_right = "Ctrl+L"

# Pane resize (Alt+HJKL)
resize_left = "Alt+H"
resize_right = "Alt+L"
resize_up = "Alt+K"
resize_down = "Alt+J"
equalize = "Alt+="

# Chat
enter_insert = "i"
scroll_down = "j"
scroll_up = "k"
scroll_bottom = "G"
scroll_top = "gg"
send_continue = "c"

# Diff
next_hunk = "n"
prev_hunk = "N"
next_file = "]"
prev_file = "["
toggle_mode = "Tab"
approve_hunk = "a"
reject_hunk = "r"

# Insert mode keybindings
[insert]
exit_insert = "Esc"
send_message = "Enter"
new_line = "Shift+Enter"
delete_word_back = "Ctrl+W"
delete_to_start = "Ctrl+U"
paste = "Ctrl+V"

# Command mode keybindings
[command]
execute = "Enter"
cancel = "Esc"

# Custom user commands
[custom]
# Map Ctrl+S to save session
"Ctrl+S" = "session save"
# Map Ctrl+P to command palette (alternative to :)
"Ctrl+P" = "command_palette"
```

### Keybinding Resolution

```rust
/// Keybinding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeymapConfig {
    pub global: HashMap<String, KeyBinding>,
    pub normal: HashMap<String, KeyBinding>,
    pub insert: HashMap<String, KeyBinding>,
    pub command: HashMap<String, KeyBinding>,
    pub custom: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: KeyEvent,
    pub action: Action,
}

impl KeymapConfig {
    /// Load from file, falling back to defaults
    pub fn load() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"));
        let path = config_dir.join("xaft").join("keybindings.toml");

        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let raw: toml::Value = toml::from_str(&content)?;
            Self::from_toml(&raw)
        } else {
            Ok(Self::defaults())
        }
    }

    /// Default keybindings
    pub fn defaults() -> Self {
        let mut global = HashMap::new();
        global.insert("quit".into(), KeyBinding::from_str("Ctrl+D", Action::Quit).unwrap());
        global.insert("cancel".into(), KeyBinding::from_str("Esc", Action::Cancel).unwrap());
        global.insert("interrupt".into(), KeyBinding::from_str("Ctrl+C", Action::Interrupt).unwrap());
        // ... (all defaults from the tables above)

        Self {
            global,
            normal: Self::default_normal_bindings(),
            insert: Self::default_insert_bindings(),
            command: Self::default_command_bindings(),
            custom: HashMap::new(),
        }
    }
}

/// Parse a key binding string like "Ctrl+L" or "Alt+Shift+Enter"
impl KeyBinding {
    pub fn from_str(s: &str, action: Action) -> Result<Self> {
        let parts: Vec<&str> = s.rsplitn(2, '+').collect();
        let key_part = parts[0];
        let modifier_part = parts.get(1).copied().unwrap_or("");

        let code = parse_key_code(key_part)?;
        let modifiers = parse_modifiers(modifier_part)?;

        Ok(Self {
            key: KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE },
            action,
        })
    }
}

fn parse_key_code(s: &str) -> Result<KeyCode> {
    match s {
        "Enter" => Ok(KeyCode::Enter),
        "Esc" => Ok(KeyCode::Esc),
        "Tab" => Ok(KeyCode::Tab),
        "Backspace" => Ok(KeyCode::Backspace),
        "Up" => Ok(KeyCode::Up),
        "Down" => Ok(KeyCode::Down),
        "Left" => Ok(KeyCode::Left),
        "Right" => Ok(KeyCode::Right),
        "Home" => Ok(KeyCode::Home),
        "End" => Ok(KeyCode::End),
        "F1" => Ok(KeyCode::F(1)),
        // ... (F2-F12)
        c if c.len() == 1 => Ok(KeyCode::Char(c.chars().next().unwrap())),
        _ => Err(anyhow!("Unknown key: {}", s)),
    }
}

fn parse_modifiers(s: &str) -> Result<KeyModifiers> {
    let mut mods = KeyModifiers::NONE;
    for part in s.split('+') {
        match part {
            "Ctrl" => mods.insert(KeyModifiers::CONTROL),
            "Alt" => mods.insert(KeyModifiers::ALT),
            "Shift" => mods.insert(KeyModifiers::SHIFT),
            "Super" => mods.insert(KeyModifiers::SUPER),
            "" => {}
            _ => return Err(anyhow!("Unknown modifier: {}", part)),
        }
    }
    Ok(mods)
}
```

## Command Palette

### Visual Design

```
┌─ Command ────────────────────────────────────────────────────┐
│ : budget 5.00                                               │
│                                                              │
│  budget <amount>    Set session budget limit                  │
│  model <name>       Switch default model                     │
│  layout default     Switch to default layout                  │
│  layout focus       Switch to focus layout                   │
│  layout review      Switch to review layout                  │
│  approve-all        Approve all pending approvals            │
│  reject-all         Reject all pending approvals             │
│  clear              Clear chat display                       │
│  theme <name>       Change color theme                       │
│  export markdown    Export session as markdown                │
│  export json        Export session as JSON                   │
│  help               Show keybinding reference                │
│  debug              Toggle debug overlay                     │
│                                                              │
│  ↑/↓ navigate  Enter select  Esc cancel                     │
└──────────────────────────────────────────────────────────────┘
```

### Command Palette Implementation

```rust
/// Command palette state
pub struct CommandPalette {
    /// Current input buffer
    input: String,

    /// Cursor position in input
    cursor: usize,

    /// Available commands (filtered by current input)
    visible_commands: Vec<CommandEntry>,

    /// Selected command index
    selected: usize,

    /// All registered commands
    all_commands: Vec<CommandEntry>,
}

#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub action: CommandAction,
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone)]
pub enum CommandAction {
    SetBudget,
    SwitchModel,
    SetLayout,
    ApproveAll,
    RejectAll,
    ClearChat,
    SetTheme,
    Export { format: ExportFormat },
    Help,
    Debug,
    Quit,
    Reset,
    SessionSave,
}

#[derive(Debug, Clone)]
pub struct ArgSpec {
    pub name: String,
    pub required: bool,
    pub arg_type: ArgType,
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum ArgType {
    String,
    Number,
    Enum(Vec<String>),
}

impl CommandPalette {
    /// Register all built-in commands
    pub fn new() -> Self {
        let all_commands = vec![
            CommandEntry {
                name: "budget".into(),
                description: "Set session budget limit".into(),
                action: CommandAction::SetBudget,
                args: vec![ArgSpec {
                    name: "amount".into(),
                    required: true,
                    arg_type: ArgType::Number,
                    description: "Budget in USD".into(),
                }],
            },
            CommandEntry {
                name: "model".into(),
                description: "Switch default model".into(),
                action: CommandAction::SwitchModel,
                args: vec![ArgSpec {
                    name: "name".into(),
                    required: true,
                    arg_type: ArgType::Enum(vec![
                        "claude-sonnet".into(),
                        "claude-haiku".into(),
                        "gpt-4o".into(),
                        "gpt-4o-mini".into(),
                    ]),
                    description: "Model name".into(),
                }],
            },
            CommandEntry {
                name: "layout".into(),
                description: "Switch to a layout preset".into(),
                action: CommandAction::SetLayout,
                args: vec![ArgSpec {
                    name: "preset".into(),
                    required: true,
                    arg_type: ArgType::Enum(vec![
                        "default".into(),
                        "focus".into(),
                        "review".into(),
                        "debug".into(),
                        "monitor".into(),
                    ]),
                    description: "Layout preset name".into(),
                }],
            },
            CommandEntry {
                name: "approve-all".into(),
                description: "Approve all pending approvals".into(),
                action: CommandAction::ApproveAll,
                args: vec![],
            },
            CommandEntry {
                name: "reject-all".into(),
                description: "Reject all pending approvals".into(),
                action: CommandAction::RejectAll,
                args: vec![],
            },
            CommandEntry {
                name: "clear".into(),
                description: "Clear chat display".into(),
                action: CommandAction::ClearChat,
                args: vec![],
            },
            CommandEntry {
                name: "theme".into(),
                description: "Change color theme".into(),
                action: CommandAction::SetTheme,
                args: vec![ArgSpec {
                    name: "name".into(),
                    required: true,
                    arg_type: ArgType::Enum(vec![
                        "default".into(),
                        "high-contrast".into(),
                        "dracula".into(),
                        "solarized-dark".into(),
                        "nord".into(),
                    ]),
                    description: "Theme name".into(),
                }],
            },
            CommandEntry {
                name: "export".into(),
                description: "Export session".into(),
                action: CommandAction::Export { format: ExportFormat::Markdown },
                args: vec![ArgSpec {
                    name: "format".into(),
                    required: true,
                    arg_type: ArgType::Enum(vec!["markdown".into(), "json".into()]),
                    description: "Export format".into(),
                }],
            },
            CommandEntry {
                name: "help".into(),
                description: "Show keybinding reference".into(),
                action: CommandAction::Help,
                args: vec![],
            },
            CommandEntry {
                name: "debug".into(),
                description: "Toggle debug overlay".into(),
                action: CommandAction::Debug,
                args: vec![],
            },
            CommandEntry {
                name: "quit".into(),
                description: "Quit xaft".into(),
                action: CommandAction::Quit,
                args: vec![],
            },
            CommandEntry {
                name: "reset".into(),
                description: "Reset the session".into(),
                action: CommandAction::Reset,
                args: vec![],
            },
        ];

        Self {
            input: String::new(),
            cursor: 0,
            visible_commands: all_commands.clone(),
            selected: 0,
            all_commands,
        }
    }

    /// Filter commands by current input
    pub fn update_filter(&mut self) {
        let query = self.input.trim_start_matches(':').trim();
        self.visible_commands = if query.is_empty() {
            self.all_commands.clone()
        } else {
            self.all_commands.iter()
                .filter(|cmd| cmd.name.contains(query) || cmd.description.contains(query))
                .cloned()
                .collect()
        };
        self.selected = 0;
    }

    /// Handle key event in command palette
    pub fn handle_key(&mut self, key: KeyEvent) -> CommandPaletteResult {
        match key.code {
            KeyCode::Char(c) => {
                self.input.push(c);
                self.cursor += 1;
                self.update_filter();
                CommandPaletteResult::Continue
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.input.remove(self.cursor - 1);
                    self.cursor -= 1;
                    self.update_filter();
                }
                CommandPaletteResult::Continue
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                CommandPaletteResult::Continue
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.visible_commands.len().saturating_sub(1));
                CommandPaletteResult::Continue
            }
            KeyCode::Enter => {
                if let Some(cmd) = self.visible_commands.get(self.selected) {
                    CommandPaletteResult::Execute(cmd.action.clone())
                } else {
                    CommandPaletteResult::Cancel
                }
            }
            KeyCode::Esc => CommandPaletteResult::Cancel,
            _ => CommandPaletteResult::Continue,
        }
    }
}

pub enum CommandPaletteResult {
    Continue,
    Execute(CommandAction),
    Cancel,
}
```

### Which-Key Popup

When the user presses a key that starts a chord (like `g` or `z`), xaft shows a
which-key popup listing the possible completions:

```
┌─ Which Key ─────────────┐
│ g g  Scroll to top      │
│                         │
│ z c  Collapse           │
│ z o  Expand             │
│ z a  Toggle fold        │
│ z M  Collapse all       │
│ z R  Expand all         │
│                         │
│ Esc cancel              │
└─────────────────────────┘
```

```rust
/// Which-key popup handler
pub struct WhichKeyPopup {
    /// The pending key
    pending_key: KeyCode,

    /// Available completions
    bindings: Vec<(KeyCode, &'static str, &'static str)>, // (key, label, description)
}

impl WhichKeyPopup {
    pub fn for_key(key: KeyCode) -> Option<Self> {
        let bindings = match key {
            KeyCode::Char('g') => vec![
                (KeyCode::Char('g'), "gg", "Scroll to top"),
            ],
            KeyCode::Char('z') => vec![
                (KeyCode::Char('c'), "zc", "Collapse"),
                (KeyCode::Char('o'), "zo", "Expand"),
                (KeyCode::Char('a'), "za", "Toggle fold"),
                (KeyCode::Char('M'), "zM", "Collapse all"),
                (KeyCode::Char('R'), "zR", "Expand all"),
            ],
            _ => return None,
        };

        Some(Self { pending_key: key, bindings })
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup_height = self.bindings.len() as u16 + 3;
        let popup_width = 30u16;
        let x = area.x + 2;
        let y = area.bottom().saturating_sub(popup_height + 2);

        // Background
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Which Key ");
        let popup_area = Rect::new(x, y, popup_width, popup_height);
        block.render(popup_area, buf);

        // Binding rows
        for (i, (_, label, desc)) in self.bindings.iter().enumerate() {
            let row_y = y + 1 + i as u16;
            let line = Line::from(vec![
                Span::styled(format!(" {:<5}", label), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(*desc, Style::default().fg(Color::Gray)),
            ]);
            line.render(Rect::new(x + 1, row_y, popup_width - 2, 1), buf);
        }

        // Esc hint
        let esc_y = y + 1 + self.bindings.len() as u16;
        let esc_line = Line::from(Span::styled(" Esc cancel", Style::default().fg(Color::DarkGray)));
        esc_line.render(Rect::new(x + 1, esc_y, popup_width - 2, 1), buf);
    }
}
```

## Keybinding Conflict Detection

When the user configures custom keybindings, xaft checks for conflicts:

```rust
/// Detect keybinding conflicts
pub fn detect_conflicts(config: &KeymapConfig) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let mut seen: HashMap<KeyEvent, (String, String)> = HashMap::new();

    // Check all bindings
    for (mode_name, bindings) in [
        ("global", &config.global),
        ("normal", &config.normal),
        ("insert", &config.insert),
        ("command", &config.command),
    ] {
        for (action_name, binding) in bindings {
            if let Some((prev_mode, prev_action)) = seen.get(&binding.key) {
                conflicts.push(Conflict {
                    key: binding.key,
                    first_mode: prev_mode.clone(),
                    first_action: prev_action.clone(),
                    second_mode: mode_name.to_string(),
                    second_action: action_name.clone(),
                });
            } else {
                seen.insert(binding.key, (mode_name.to_string(), action_name.clone()));
            }
        }
    }

    conflicts
}

#[derive(Debug)]
pub struct Conflict {
    pub key: KeyEvent,
    pub first_mode: String,
    pub first_action: String,
    pub second_mode: String,
    pub second_action: String,
}
```

When conflicts are detected, xaft shows a warning on startup:

```
⚠ Keybinding conflict: Ctrl+L is mapped to both:
  - global.focus_right
  - global.toggle_log

  Edit ~/.config/xaft/keybindings.toml to resolve.
```
