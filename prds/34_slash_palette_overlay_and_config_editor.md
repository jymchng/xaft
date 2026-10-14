# PRD 34 — Slash Palette Overlay + Interactive Config Editor

## Status: Draft
## Depends on: PRD 32a–32e (slash command system), PRD 09, PRD 10

---

## 1. Problem

### 1.1 Slash palette is computed but invisible

`AppState` already computes a `SlashPalette` when the user types `/`
(candidates, descriptions, args hints, selected index), but `build_prompt()`
never includes it in `PromptState`. The renderer therefore never draws it.
The `@`-mention path works identically — `AutocompleteDropdown` IS included
in `PromptState` and the renderer displays it — but slash commands have no
visual equivalent.

```
  ╔═══════════════════════════════════════════════════════════╗
  ║ /clear     Clear the transcript                           ║
  ║ /cls       Alias for /clear                               ║  ← exists in SlashPalette data
  ║ /config    Show the resolved configuration  [key.path]    ║  ← but never rendered
  ║ /cost      Show token usage and cost                      ║
  ╚═══════════════════════════════════════════════════════════╝
❯ /co_                                                         ← only thing rendered today
```

### 1.2 `/config` dumps unnavigable raw TOML

`/config` serialises the entire merged config to TOML and prints every
line indented by two spaces. For the typical ~120-line config this fills
the transcript, there is no syntax highlighting, the user cannot navigate
to a specific section quickly, and there is no way to change a value without
editing the TOML file on disk and reloading.

---

## 2. Goals

**Feature A — Slash palette overlay**

1. When the user types `/` (or `/` + partial text), a styled overlay appears
   below the input bar showing matching commands with names, descriptions,
   and args hints — identical in feel to the `@` autocomplete dropdown.
2. `↑/↓` navigates the list; `Tab` completes the typed prefix to the
   selected command; `Enter` (when palette is open and a command is
   selected) executes the selected command immediately.
3. `Esc` or typing a space / non-slash character dismisses the palette.
4. At most 8 rows are shown; the list scrolls if there are more candidates.

**Feature B — Interactive `/config` editor**

1. `/config` renders the merged config in a navigable, syntax-highlighted
   panel rather than a raw TOML dump.
2. `↑/↓` navigates rows; section headers are skipped automatically.
3. `Enter` on a row opens an inline edit field pre-populated with the
   current value; `Enter` again applies the new value; `Esc` cancels.
4. Applied values take effect immediately (live patch) for the running
   session without restarting.
5. The source layer for each value (cli / env / session / project / user /
   default) is shown in dimmed text.

---

## 3. Non-Goals

- Persisting config patches made via the TUI to disk (session-layer only).
- Supporting array or table values in the inline edit field (read-only
  for complex types; write `[section]` objects only via the file).
- A separate "settings" pane outside the transcript.
- Vim keybinding mode for the edit field (plain insert mode only).

---

## 4. Feature A Design — Slash Palette Overlay

### 4.1 `PromptState` change

Add the slash palette snapshot so the renderer can see it without holding
an `Arc` to `AppState`:

```rust
// crates/xaft-tui/src/prompt.rs

/// Snapshot of one candidate row for the slash palette overlay.
#[derive(Debug, Clone)]
pub struct SlashPaletteRow {
    pub trigger: String,         // e.g. "config"
    pub description: String,     // e.g. "Show the resolved configuration"
    pub args_hint: Option<String>, // e.g. "[key.path]"
}

pub struct PromptState {
    // ... existing fields ...
    /// Slash-command autocomplete overlay; `None` when not typing a command.
    pub slash_palette: Option<SlashPaletteSnapshot>,
}

#[derive(Debug, Clone)]
pub struct SlashPaletteSnapshot {
    pub rows: Vec<SlashPaletteRow>,
    pub selected: usize,
}
```

`build_prompt()` maps `AppState::slash_palette` to `SlashPaletteSnapshot`:

```rust
slash_palette: state.slash_palette.as_ref().map(|p| SlashPaletteSnapshot {
    rows: p.candidates.iter().enumerate().map(|(i, t)| SlashPaletteRow {
        trigger: t.to_string(),
        description: p.descriptions[i].to_string(),
        args_hint: p.args_hints[i].map(|s| s.to_string()),
    }).collect(),
    selected: p.selected,
}),
```

### 4.2 Renderer overlay

In `surface.rs` (or the widget responsible for drawing the input area),
if `prompt.slash_palette.is_some()`, draw the overlay below the input
border, inside the conversation area width:

```
  ┌─────────────────────────────────────────────────────────┐
  │ /clear   Clear the transcript                           │
  │ /cls     Alias for /clear                               │
  │▶/config  Show the resolved configuration  [key.path]    │  ← selected row (inverted/bold)
  │ /cost    Show token usage and cost                      │
  └─────────────────────────────────────────────────────────┘
❯ /co_
```

Column layout per row:
- 2 cols indent
- `▶` or ` ` (1 col) — selection indicator
- `/trigger` (left-aligned, 14 cols, bold when selected)
- description (fills remaining width, normal weight, truncated with `…`)
- args_hint (right-aligned, dimmed, e.g. `[key.path]`)

The overlay is drawn with a box border (`┌─┐ │ └─┘`) and uses the theme's
`overlay_bg` / `overlay_fg` colours.

### 4.3 Key routing when palette is open

In `AppState::handle_key_event`, the slash palette intercepts keys before
the input bar does (already partially wired in state.rs at line 1576).
Extend to cover `Tab` and `Enter`:

| Key | Action |
|-----|--------|
| `↑` / `↓` | `select_prev` / `select_next` |
| `Tab` | Complete: replace typed partial with selected trigger + `/` + selected trigger text in input bar (cursor after the trigger) |
| `Enter` | Execute selected command immediately (call `execute_slash_command`), clear palette |
| `Esc` | Dismiss palette, do not change input bar |
| Any printable | Pass to input bar (palette auto-updates or dismisses) |

`Tab` completion behaviour:
1. Drain the input bar.
2. Set input to `/<selected_trigger> ` (trigger followed by a space to
   accept args).
3. If `args_hint` is `None`, submit immediately (no args needed).

### 4.4 Keyboard shortcut hint

While palette is open, show a 1-line hint in the overlay footer:
```
  ↑/↓ navigate · Tab complete · Enter execute · Esc dismiss
```

---

## 5. Feature B Design — Interactive Config Editor

### 5.1 Config flattening

`ConfigHandler::execute` is replaced by a new code path that produces a
flat list of config entries rather than a raw TOML dump.

```rust
// crates/xaft-tui/src/slash/commands/config.rs

pub struct ConfigEntry {
    pub section: String,          // e.g. "agent.default"
    pub key: String,              // e.g. "max_turns"
    pub display_value: String,    // formatted for display
    pub raw_value: String,        // the string the edit box starts with
    pub value_kind: ConfigValueKind,
    pub source_layer: ConfigLayer, // which of the 6 layers set this value
    pub editable: bool,           // false for table/array values
}

pub enum ConfigValueKind {
    Str,
    Int,
    Float,
    Bool,
    Array,  // read-only in editor
    Table,  // read-only in editor
}

pub enum ConfigLayer {
    CliFlag,
    EnvVar,
    Session,
    Project,
    User,
    Default,
}

impl ConfigLayer {
    pub fn label(self) -> &'static str {
        match self {
            Self::CliFlag  => "cli",
            Self::EnvVar   => "env",
            Self::Session  => "session",
            Self::Project  => "project",
            Self::User     => "user",
            Self::Default  => "default",
        }
    }
}
```

`ConfigLayer` detection: compare the merged value against the per-layer
values from the six config sources. The outermost layer where the value
differs from the next layer wins. This can be approximated without full
per-layer tracking by comparing against the default value (anything
different from default has been explicitly set; annotate as `project` if
`project_config.toml` exists and contains the key, else `session`, etc.).

### 5.2 `ConfigEditorState`

Stored in `AppState` alongside the transcript:

```rust
pub struct ConfigEditorState {
    pub visible: bool,
    pub entries: Vec<ConfigEntry>,
    pub selected: usize,             // index into `entries` (skips section headers)
    pub edit: Option<ConfigEditRow>, // `Some` when editing a value inline
    pub scroll_top: usize,
}

pub struct ConfigEditRow {
    pub entry_index: usize,
    pub original: String,
    pub input: InputBar,  // reuse InputBar for single-line text editing
}
```

### 5.3 `/config` command output

`ConfigHandler::execute` returns `CommandResult::ConfigEditor(entries)`.

Add a new `CommandResult` variant:

```rust
pub enum CommandResult {
    // ... existing variants ...
    ConfigEditor(Vec<ConfigEntry>),
}
```

`AppState::apply_command_result` handles `ConfigEditor(entries)` by:
1. Populating `self.config_editor.entries = entries`.
2. Setting `self.config_editor.visible = true`.
3. Pushing `RenderMutation::UpdatePrompt(build_prompt(self))` to trigger
   a re-render.

### 5.4 Config editor rendering

Rendered in-place in the transcript area (or as a modal overlay); uses a
fixed-width panel anchored below the `/config` separator line.

```
  ╌╌ /config ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  [core]
    log_level         "info"              string   default
  ▶ data_dir          "/root/.xaft"       string   default   ← selected
    telemetry         true                bool     default
  [agent.default]
    model             "claude-3-5-…"      string   project
    max_turns         25                  int      default
    temperature       0.0                 float    default
    allowed_tools     ["*"]               array    —
  [tui]
    theme             "dark"              string   user
    mouse             false               bool     default
  ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ↑/↓ navigate · Enter to edit · /config [section] to filter · Esc to dismiss
```

Column layout:
- 2 cols indent
- `▶` or ` ` (1 col)
- key (left-aligned, 18 cols)
- value (left-aligned, fills remaining minus last 22 cols, value truncated with `…` if long)
- value kind (8 cols, dimmed)
- source layer (8 cols, dimmed cyan)

Section header rows are rendered bold, not selectable.

Array/table rows are rendered with `—` in the edit column and are not
selectable (cursor skips them).

### 5.5 Edit mode

Pressing `Enter` on an editable row opens inline edit:

```
  ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
    data_dir  ›  /root/.xaft_
    (string · was: "/root/.xaft" · Enter to apply · Esc to cancel)
  ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
```

The `ConfigEditRow::input` is a single-row `InputBar` pre-populated with
`raw_value`. Key routing while in edit mode:

| Key | Action |
|-----|--------|
| `Enter` | Validate → apply → close edit row |
| `Esc` | Discard changes → close edit row |
| All other keys | Forwarded to `ConfigEditRow::input.handle_key()` |

### 5.6 Value validation

Before applying, parse the new value string to the target type:

```rust
fn parse_config_value(s: &str, kind: ConfigValueKind) -> Result<toml::Value, String> {
    match kind {
        ConfigValueKind::Str   => Ok(toml::Value::String(s.to_string())),
        ConfigValueKind::Int   => s.parse::<i64>()
            .map(toml::Value::Integer)
            .map_err(|_| format!("Expected integer, got {:?}", s)),
        ConfigValueKind::Float => s.parse::<f64>()
            .map(toml::Value::Float)
            .map_err(|_| format!("Expected float, got {:?}", s)),
        ConfigValueKind::Bool  => match s.trim() {
            "true" | "1" | "yes" => Ok(toml::Value::Boolean(true)),
            "false" | "0" | "no" => Ok(toml::Value::Boolean(false)),
            _ => Err(format!("Expected true/false, got {:?}", s)),
        },
        ConfigValueKind::Array | ConfigValueKind::Table => {
            Err("Complex values cannot be edited inline.".into())
        }
    }
}
```

On validation failure, replace the edit hint line with the error message
in red; the edit field stays open.

### 5.7 Live patching via config patch channel

`XaftConfig` is currently `Arc<XaftConfig>` (immutable after load). For
in-flight editing, add a `ConfigPatchTx`:

```rust
// crates/xaft-tui/src/state.rs
pub config_patch_tx: Option<mpsc::UnboundedSender<ConfigPatch>>,
```

```rust
// crates/xaft-config/src/types.rs
#[derive(Debug, Clone)]
pub struct ConfigPatch {
    pub dotted_key: String,   // e.g. "agent.default.max_turns"
    pub value: toml::Value,
}
```

`XaftRuntime` holds the receiving end (`mpsc::UnboundedReceiver<ConfigPatch>`)
and applies patches to a `RwLock<XaftConfig>` before the next agent run.
Patches are applied only to the **session layer** — they are not written
to any file on disk and are lost on TUI exit.

On successful apply, the config editor refreshes its entry list and the
edited row's source layer changes to `session`.

### 5.8 `/config [section]` filter

`/config agent.default` renders only the `[agent.default]` section (already
partially implemented in `ConfigHandler` via the `key_filter` heuristic;
this is preserved and works with the new panel too).

### 5.9 Dismissal

`Esc` while the config editor is focused sets `config_editor.visible = false`
and pushes `UpdatePrompt`. The editor entries are retained in state so
re-opening with `/config` is instant (no re-serialisation needed).

---

## 6. New `CommandResult` variant

```rust
pub enum CommandResult {
    Lines(Vec<String>),
    Error(String),
    Info(String),
    ConfigEditor(Vec<ConfigEntry>),  // ← new
}
```

`apply_command_result` in `state.rs` handles the new variant.

---

## 7. New Signal

```rust
/// Emitted when the user applies a config patch via the TUI editor.
#[derive(Debug, Clone)]
pub struct XaftConfigPatched {
    pub key: String,
    pub old_value: String,
    pub new_value: String,
    pub layer: String,  // always "session" for TUI-sourced patches
}
```

---

## 8. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/prompt.rs` | Add `SlashPaletteSnapshot`, `SlashPaletteRow` to `PromptState`; populate in `build_prompt()` |
| `crates/xaft-tui/src/surface.rs` | Render slash palette overlay below input border |
| `crates/xaft-tui/src/state.rs` | Wire `Tab` / `Enter` in palette; add `ConfigEditorState`, `config_patch_tx`; handle `CommandResult::ConfigEditor` |
| `crates/xaft-tui/src/slash/commands/config.rs` | Flatten config to `Vec<ConfigEntry>`, return `CommandResult::ConfigEditor` |
| `crates/xaft-tui/src/slash/mod.rs` | Add `CommandResult::ConfigEditor` variant and `ConfigEntry` types |
| `crates/xaft-config/src/types.rs` | Add `ConfigPatch` struct |
| `crates/xaft-runtime/src/runtime.rs` | Hold `RwLock<XaftConfig>`, listen for `ConfigPatch` messages |
| `crates/xaft-agent/src/signals.rs` | `XaftConfigPatched` signal |
| `crates/xaft-tui/tests/slash_palette_integration.rs` | Palette overlay rendering tests |
| `crates/xaft-tui/tests/config_editor_integration.rs` | Config editor navigation and edit tests |

---

## 9. Acceptance Criteria

### Feature A — Slash palette overlay

| # | Criterion |
|---|-----------|
| A1 | Typing `/` with an empty bar shows a palette with all registered commands (at least 20 rows). |
| A2 | Typing `/co` filters to commands starting with "co" (e.g. `config`, `cost`). |
| A3 | `↓/↑` changes the highlighted row; the visual indicator moves. |
| A4 | `Tab` replaces the input bar text with `/<selected_trigger> ` and dismisses the palette. |
| A5 | `Enter` while the palette is open executes the highlighted command (e.g. `/help`). |
| A6 | `Esc` dismisses the palette without executing anything and without clearing the input. |
| A7 | Palette shows at most 8 rows; scrolls if there are more candidates. |
| A8 | Description and args hint are visible for each row. |

### Feature B — Interactive config editor

| # | Criterion |
|---|-----------|
| B1 | `/config` renders a navigable panel, not a raw TOML dump. |
| B2 | `↑/↓` moves the selection between editable rows; section headers are skipped. |
| B3 | `Enter` on a `string` row opens an inline edit field pre-populated with the current value. |
| B4 | `Enter` in edit mode with a valid value applies the patch; source layer shows `session`. |
| B5 | `Esc` in edit mode discards the change and returns to navigation. |
| B6 | Typing an invalid value (e.g. "abc" for an `int` field) shows a validation error; the edit field stays open. |
| B7 | `/config agent.default` shows only the `[agent.default]` section. |
| B8 | `XaftConfigPatched` signal is emitted after a successful patch. |
| B9 | `Esc` while in navigation mode dismisses the config editor panel. |
| B10 | Array and table rows are visible but not editable (no cursor stop, `—` in edit column). |

---

## 10. Tests

### `crates/xaft-tui/tests/slash_palette_integration.rs`

```rust
#[tokio::test]
async fn palette_appears_when_typing_slash() { ... }

#[tokio::test]
async fn palette_filters_on_partial_input() { ... }

#[tokio::test]
async fn tab_completes_selected_trigger() { ... }

#[tokio::test]
async fn enter_executes_selected_command() { ... }

#[tokio::test]
async fn esc_dismisses_without_executing() { ... }
```

### `crates/xaft-tui/tests/config_editor_integration.rs`

```rust
#[tokio::test]
async fn config_command_opens_editor_panel() { ... }

#[tokio::test]
async fn arrow_keys_navigate_editor_rows() { ... }

#[tokio::test]
async fn enter_opens_edit_field_for_string_row() { ... }

#[tokio::test]
async fn valid_edit_applies_patch_and_emits_signal() { ... }

#[tokio::test]
async fn invalid_int_value_shows_error() { ... }

#[tokio::test]
async fn esc_in_edit_mode_cancels() { ... }

#[tokio::test]
async fn config_section_filter_shows_only_matching_section() { ... }
```
