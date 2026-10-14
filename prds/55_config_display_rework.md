# PRD 55 — `/config` Display Rework

## Status: Draft
## Supersedes: PRD 34 Feature B (Interactive Config Editor — retired)
## Depends on: PRD 09, PRD 10 (TUI architecture)

---

## 1. Problem

The Feature B implementation from PRD 34 attempted to build an interactive
config editor by rendering `ConfigEntry` rows as committed transcript lines and
then intercepting `↑/↓/Enter/Esc` key events to navigate and edit them.

This is **architecturally impossible** in xaft's append-only TUI:

> After `commit_line()` the line scrolls into terminal scrollback.  The
> renderer never redraws it.  There is no cursor, no selection, no highlight
> that can move between committed lines.

The result is a `ConfigEditorState` struct (with `selected`, `edit: Option<ConfigEditRow>`,
scroll tracking, etc.) whose state mutates silently on keypress but produces no
visible effect — users press `↑/↓` and nothing changes on screen.  The inline
`InputBar` inside `ConfigEditRow` is never rendered anywhere.  Every part of the
interactive layer is dead code.

Additionally, the current display is barely readable:

```
  [core]
     log_level        "info"                         string   default
     data_dir         "/root/.xaft"                  string   default
     telemetry        true                           bool     default
  [agent.default]
  ...
```

There are no visual separators, the column widths are rigid, long values are
silently truncated, and there is no way to know what a value controls without
cross-referencing the docs.

---

## 2. Goals

1. `/config` renders each config section as a clearly-labelled, formatted, **read-only** block in the transcript.  No interactive state, no navigation, no broken key handlers.
2. Sections are separated by `─` dividers so the user can scan the output.
3. Each row shows: key, current value (properly quoted/coloured), type hint, and source layer.  Values that differ from the default are highlighted.
4. `/config <section>` filters to one section (e.g. `/config tui`, `/config agent.default`).
5. `/config set <dotted.key> <value>` applies a session-layer patch.  It is a **separate sub-command variant**, not inline editing.
6. The broken `ConfigEditorState` / `ConfigEditRow` / navigation state machine is **removed entirely**.
7. The `CommandResult::ConfigEditor(Vec<ConfigEntry>)` variant is **renamed to `CommandResult::ConfigDisplay`** to clarify its purpose (display-only).

---

## 3. Non-Goals

- A full-screen interactive config panel (belongs in a future TUI overhaul with a proper alternate-screen mode).
- Persisting session patches to disk (session-layer only, same as before).
- Editing arrays or nested tables inline.
- Syntax-highlighted TOML rendering (use the existing `StyledLine` colour system).

---

## 4. Architecture

### 4.1 Remove the interactive layer entirely

Delete from `state.rs`:
- `ConfigEditorState` struct + impl
- `ConfigEditRow` struct
- The `if self.config_editor.visible { … }` key-routing block in `handle_key`
- The `config_editor: ConfigEditorState` field on `AppState`
- The `parse_config_value` helper (no longer needed at the state level)

Delete from `lib.rs`:
- `pub use state::{ConfigEditorState, ConfigEditRow, …}`

This eliminates ~120 lines of dead code that created phantom state with no
visible effect.

### 4.2 `CommandResult` rename + new variant

```rust
// crates/xaft-tui/src/slash/mod.rs

pub enum CommandResult {
    Lines(Vec<String>),
    StyledLines(Vec<StyledLine>),
    Handled,
    Error(String),
    /// Formatted config display (read-only).  Replaces the old `ConfigEditor`.
    ConfigDisplay(Vec<ConfigSection>),
}

/// A single named section in the config display.
pub struct ConfigSection {
    /// TOML section name, e.g. `"core"` or `"agent.default"`.
    pub name: String,
    /// Rows within this section.
    pub rows: Vec<ConfigRow>,
}

/// One key-value row within a config section.
pub struct ConfigRow {
    pub key: String,
    pub display_value: String,
    pub value_kind: ConfigValueKind,
    pub source_layer: ConfigLayer,
    /// True if the value differs from the compiled-in default.
    pub is_overridden: bool,
}
```

`apply_command_result` in `state.rs` handles `ConfigDisplay`:

```rust
CommandResult::ConfigDisplay(sections) => {
    for section in &sections {
        // Section header: ── [core] ─────────────────────
        let header = format!(
            "  ── [{}] {}",
            section.name,
            "─".repeat(54_usize.saturating_sub(section.name.len() + 6))
        );
        self.mutations.push(RenderMutation::CommitLine(
            StyledLine::new(header, LineKind::Separator)
        ));

        for row in &section.rows {
            let override_marker = if row.is_overridden { "*" } else { " " };
            let layer = row.source_layer.label();
            let kind  = match row.value_kind {
                ConfigValueKind::Str   => "str",
                ConfigValueKind::Int   => "int",
                ConfigValueKind::Float => "f64",
                ConfigValueKind::Bool  => "bool",
                ConfigValueKind::Array => "array",
                ConfigValueKind::Table => "table",
            };
            // Example:
            //   * log_level       "warn"              str   project
            //     telemetry       true                bool  default
            let line = format!(
                "  {override_marker} {key:<20} {val:<22} {kind:<6} {layer}",
                key  = row.key,
                val  = truncate_display(&row.display_value, 22),
            );
            let kind = if row.is_overridden {
                LineKind::Success  // accent colour for overridden values
            } else {
                LineKind::System   // dim for default values
            };
            self.mutations.push(RenderMutation::CommitLine(
                StyledLine::new(line, kind)
            ));
        }
    }

    // Footer hint.
    self.mutations.push(RenderMutation::CommitLine(
        StyledLine::new(
            "  * = overridden from default  ·  /config set <key> <value> to change",
            LineKind::System,
        )
    ));
}
```

Helper:
```rust
fn truncate_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() }
    else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}
```

### 4.3 `ConfigHandler` — build `ConfigSection`s instead of flat entries

```rust
// crates/xaft-tui/src/slash/commands/config.rs

fn run_config(&self, args: &str) -> CommandResult {
    let filter = args.trim().to_lowercase();
    // serialize → parse (as before)
    let mut sections: Vec<ConfigSection> = Vec::new();
    if let Tv::Table(ref top) = root {
        for (section_name, section_val) in top.iter() {
            // apply filter
            if !filter.is_empty() && !section_name.to_lowercase().contains(&filter) {
                continue;
            }
            let rows = collect_rows(section_name, section_val, &default_root);
            if !rows.is_empty() {
                sections.push(ConfigSection { name: section_name.clone(), rows });
            }
        }
    }
    if sections.is_empty() {
        return CommandResult::Error(
            format!("No config section matches '{}'.", args.trim())
        );
    }
    CommandResult::ConfigDisplay(sections)
}
```

### 4.4 `/config set` — new sub-command

`/config set core.log_level debug` parses as `SlashCommand::ConfigSet { key, value }`:

```rust
pub enum SlashCommand {
    // … existing …
    ConfigSet { key: String, value: String },
}
```

`ConfigSetHandler` implementation:
1. Parse the dotted key into a section + field path.
2. Call `parse_config_value(value_str, expected_kind)` to validate the type.
3. Send a `ConfigPatch { dotted_key, value }` on the `config_patch_tx` channel.
4. Emit `XaftConfigPatched` signal.
5. Return `CommandResult::Lines(vec!["  ✓ core.log_level = debug (session)".into()])`.

The `config_patch_tx` channel is wired in `app.rs` (PRD 34 already defined `ConfigPatch`; this PRD completes the wiring).

### 4.5 Visual format

Full `/config` output example (80-column terminal):

```
  ╌╌ /config ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ── [core] ────────────────────────────────────────────────────
    log_level            "info"                 str    default
  * data_dir             "/home/dev/.xaft"      str    project
    telemetry            true                   bool   default
  ── [agent.default] ───────────────────────────────────────────
  * model                "claude-3-5-sonnet"    str    project
    max_turns            25                     int    default
    temperature          0.0                    f64    default
  ── [tui] ─────────────────────────────────────────────────────
    theme                "dark"                 str    default
    mouse                false                  bool   default
  ── [compaction] ──────────────────────────────────────────────
    enabled              true                   bool   default
    threshold_pct        80                     int    default
    keep_recent_turns    4                      int    default
  * = overridden from default  ·  /config set <key> <value> to change
```

`/config tui` output:

```
  ╌╌ /config tui ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ── [tui] ─────────────────────────────────────────────────────
    theme                "dark"                 str    default
    mouse                false                  bool   default
    timestamps           true                   bool   default
    render_markdown      true                   bool   default
    max_background_tasks 2                      int    default
  * = overridden from default  ·  /config set <key> <value> to change
```

`/config set tui.theme light` output:

```
  ╌╌ /config set ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ✓ tui.theme = "light"  (session)
```

### 4.6 Section ordering

Sections are displayed in a stable, human-readable order. The handler sorts them:
```
core → agent → provider → tool → guardrail → tui → compaction → memory → mention → mcp → model_tiers → plugins
```
Unknown sections (from user config extensions) appear at the end, alphabetically.

---

## 5. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/slash/mod.rs` | Rename `ConfigEditor` → `ConfigDisplay`; add `ConfigSection`, `ConfigRow`; add `SlashCommand::ConfigSet` |
| `crates/xaft-tui/src/slash/commands/config.rs` | Rewrite to produce `ConfigDisplay(Vec<ConfigSection>)` |
| `crates/xaft-tui/src/slash/commands/mod.rs` | Add `ConfigSetHandler`, register `config_set` |
| `crates/xaft-tui/src/slash/parser.rs` | Parse `/config set <key> <value>` |
| `crates/xaft-tui/src/state.rs` | Remove `ConfigEditorState`, `ConfigEditRow`, navigation key routing; handle `ConfigDisplay` in `apply_command_result` |
| `crates/xaft-tui/src/lib.rs` | Remove `ConfigEditorState`, `ConfigEditRow` from public exports |
| `crates/xaft-tui/tests/config_editor_integration.rs` | Update tests to match new display-only behaviour |

---

## 6. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | `/config` outputs a section-grouped display with `─` dividers, no interactive state. |
| AC2 | `/config tui` shows only the `[tui]` section. |
| AC3 | `/config agent` shows all sections containing "agent" (e.g., `agent.default`). |
| AC4 | Values differing from defaults are prefixed with `*` and rendered in accent/success colour. |
| AC5 | Long values are truncated at 22 chars with `…` suffix, never causing line overflow. |
| AC6 | Array/table values display as `[ … ]` / `{ … }` (read-only summary). |
| AC7 | Source layer is shown for every row (`default`, `project`, `session`, etc.). |
| AC8 | Footer hint `* = overridden … /config set <key> <value> to change` appears after every invocation. |
| AC9 | `/config set tui.theme light` updates the session-layer config and prints `✓ tui.theme = "light" (session)`. |
| AC10 | `/config set core.max_turns abc` returns an error `Expected integer, got "abc"`. |
| AC11 | Pressing `↑/↓` after `/config` does NOT enter any navigation mode (keys go to the input bar as normal). |
| AC12 | `AppState` has NO `config_editor` field after this change. |
| AC13 | `ConfigEditorState` and `ConfigEditRow` are completely absent from the codebase. |
| AC14 | All existing config-integration tests pass after the rewrite. |

---

## 7. Tests

```rust
#[test]
fn config_command_produces_config_display_result() {
    let mut s = AppState::new("");
    let sections = vec![
        ConfigSection {
            name: "core".into(),
            rows: vec![
                ConfigRow { key: "log_level".into(), display_value: "\"info\"".into(),
                    value_kind: ConfigValueKind::Str, source_layer: ConfigLayer::Default,
                    is_overridden: false },
            ],
        },
    ];
    s.apply_command_result(&CommandResult::ConfigDisplay(sections));
    let texts: Vec<&str> = s.mutations.iter().filter_map(|m| match m {
        RenderMutation::CommitLine(l) => Some(l.text.as_str()), _ => None,
    }).collect();
    assert!(texts.iter().any(|t| t.contains("[core]")));
    assert!(texts.iter().any(|t| t.contains("log_level")));
    assert!(texts.iter().any(|t| t.contains("default")));
}

#[test]
fn config_display_marks_overridden_values() {
    let mut s = AppState::new("");
    let sections = vec![ConfigSection {
        name: "tui".into(),
        rows: vec![
            ConfigRow { key: "theme".into(), display_value: "\"light\"".into(),
                value_kind: ConfigValueKind::Str, source_layer: ConfigLayer::Project,
                is_overridden: true },
        ],
    }];
    s.apply_command_result(&CommandResult::ConfigDisplay(sections));
    let texts: Vec<&str> = s.mutations.iter().filter_map(|m| match m {
        RenderMutation::CommitLine(l) => Some(l.text.as_str()), _ => None,
    }).collect();
    assert!(texts.iter().any(|t| t.contains('*') && t.contains("theme")));
}

#[test]
fn config_up_down_keys_do_not_enter_navigation_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = AppState::new("");
    // Even after /config output, arrow keys must NOT activate any modal state.
    let down = KeyEvent { code: KeyCode::Down, modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press, state: KeyEventState::NONE };
    s.handle_event(crate::bridge::TuiEvent::Key(down));
    // No panic, no config_editor state. The field doesn't even exist.
    assert!(!s.should_quit);
}

#[test]
fn config_section_filter_shows_only_matching() {
    // Build a ConfigHandler and verify it returns only the tui section when
    // args = "tui".
    // ...
}

#[test]
fn config_set_valid_value_emits_success_line() {
    // ConfigSetHandler with valid "tui.theme light" → CommandResult::Lines with ✓
}

#[test]
fn config_set_invalid_int_returns_error() {
    // ConfigSetHandler "core.max_turns abc" → CommandResult::Error
}

#[test]
fn no_config_editor_state_field_on_appstate() {
    // Compile-time check: AppState::new("") must not have .config_editor field.
    // Verified by absence of compilation errors + grep.
    let s = AppState::new("test");
    let _ = s.should_quit; // field access compiles → struct is valid
    // If config_editor still existed, the struct size would be larger; this test
    // is documentation that the field must be absent.
}

#[test]
fn long_value_truncated_in_display() {
    let long = "a".repeat(50);
    let result = truncate_display(&long, 22);
    assert!(result.chars().count() <= 22);
    assert!(result.ends_with('…'));
}
```

---

## 8. Migration from PRD 34 Feature B

The PRD 34 Feature B implementation is retired in full. The `ConfigEditorState`
struct, its methods, and all key routing that references it are deleted. Tests in
`config_editor_integration.rs` are rewritten to test the display-only behaviour.

The `ConfigPatch` / `XaftConfigPatched` signal infrastructure introduced by PRD 34
is retained and used by the new `/config set` sub-command.
