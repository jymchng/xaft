---
title: "PRD-62: Command Menu Dispatch — Opening Menus in Response to Slash Commands"
status: draft
version: 0.1.0
created: 2026-06-13
depends-on: 61_menu_widget_system.md
---

# PRD-62: Command Menu Dispatch — Opening Menus in Response to Slash Commands

## 1. Problem

### 1.1 Slash commands are fire-and-forget

Today every slash command handler returns a `CommandResult` that is immediately
consumed by `AppState::apply_command_result()`, which converts it into a set of
committed `RenderMutation::CommitLine` items appended to the read-only
transcript. Once committed, those lines are permanent and inert — the user
cannot navigate them, edit a field, or react to the output without typing
another command.

For informational commands (`/cost`, `/help`, `/diff`) this is correct
behaviour. For configuration commands (`/config`) it is a dead end: the output
is a static wall of text that the user cannot act on.

### 1.2 `/config` is a read-only table

`ConfigHandler::execute()` returns `CommandResult::ConfigDisplay(Vec<ConfigSection>)`.
`apply_command_result()` renders it as a series of fixed-width `StyledLine`s in
the transcript. To change a value the user must edit a TOML file on disk,
restart xaft, and re-run `/config` to confirm the change — there is no
in-session editing path at all.

### 1.3 No mechanism for a command to open an interactive overlay

There is currently no `CommandResult` variant that tells `AppState` to open a
live, interactive widget. PRD-61 defines the `MenuWidget` trait and
`MenuDriver` that manage interactive overlays, but nothing in the slash command
dispatch path can produce or deliver a `Box<dyn MenuWidget>` to the driver.
The two layers — slash commands and interactive overlays — have no bridge.

---

## 2. Goals

1. Add `CommandResult::OpenMenu(Box<dyn MenuWidget>)` so any slash command
   handler can request that an interactive overlay opens.
2. Introduce `CommandMenuRegistry` — a string-keyed factory registry that
   maps command names to closures capable of constructing `MenuWidget`s from
   a `CommandMenuContext`.
3. Wire `CommandMenuRegistry` into `SlashCommandHandler` dispatch so that when
   a registered menu command is typed, the registry is consulted first and
   `OpenMenu` is produced automatically, without touching each handler.
4. Migrate `/config` from `ConfigDisplay` (static table) to
   `CommandResult::OpenMenu(Box::new(ConfigurationMenu::new(config)))`.
5. Ensure the `IncrementalRenderer` / `app.rs` require zero extra plumbing: the
   menu driver's `is_active()` flag already gates menu rendering, and
   `apply_command_result()` just needs to call `menu_driver.open(widget)`.
6. Support inline editing: when the menu driver is active and the focused row
   exposes an edit value, `build_prompt()` reflects that value in
   `PromptState::lines` instead of the normal input buffer.

## 3. Non-Goals

- Full `ConfigurationMenu` implementation (that is PRD-61 work).
- Persisting live-edited config values to disk (a separate config-write PRD).
- Menu widgets for commands other than `/config` in this PRD (the registry and
  `OpenMenu` variant make this trivially addable later).
- Changing how `IncrementalRenderer` draws menus (that is also PRD-61 work).

---

## 4. Architecture Overview

```
User types "/config" + Enter
         │
         ▼
AppState::handle_submit()
  → SlashCommandParser::parse() → Ok(SlashCommand::Config)
  → execute_slash_command()
         │
         ▼                          NEW: checked BEFORE SlashCommandRegistry
  CommandMenuRegistry::get("config")
         │  found
         ▼
  factory(CommandMenuContext { config, working_dir, session_id })
         → Box<dyn MenuWidget>  (ConfigurationMenu)
         │
         ▼
  CommandResult::OpenMenu(widget)
         │
         ▼
  AppState::apply_command_result()
    → self.menu_driver.open(widget)   ← PRD-61 MenuDriver
         │
         ▼
  Next frame: menu_driver.is_active() == true
    → renderer draws the menu overlay
    → input events routed to menu_driver
```

---

## 5. New Types

### 5.1 `CommandMenuContext`

Lives in `crates/xaft-tui/src/menu/mod.rs` (or a new
`crates/xaft-tui/src/menu/registry.rs` sub-module).

```rust
use std::path::PathBuf;
use std::sync::Arc;
use xaft_config::XaftConfig;

/// Context passed to a `MenuFactory` when a slash command opens a menu.
///
/// Contains everything a menu widget needs to initialise itself without
/// requiring access to the full `AppState`.
pub struct CommandMenuContext {
    /// Fully-resolved application config (read-only snapshot for this session).
    pub config: Arc<XaftConfig>,
    /// Working directory of the current session (equals `AppState::working_dir`).
    pub working_dir: PathBuf,
    /// Active session ID, or `None` when no session is running.
    pub session_id: Option<String>,
}
```

### 5.2 `MenuFactory` type alias

```rust
/// A closure that constructs a `MenuWidget` from a `CommandMenuContext`.
///
/// `Send + Sync + 'static` are required because `CommandMenuRegistry` is
/// stored in `AppState` which crosses thread boundaries in the tokio
/// executor, and factories must be callable from any thread.
pub type MenuFactory = Box<dyn Fn(CommandMenuContext) -> Box<dyn MenuWidget> + Send + Sync + 'static>;
```

`MenuWidget` is defined by PRD-61. The trait itself must also be
`Send + 'static` for the constraint to compose.

### 5.3 `CommandMenuRegistry`

```rust
use std::collections::HashMap;
use std::sync::Arc;

/// Maps slash command trigger names (without the leading `/`) to
/// factory closures that construct interactive `MenuWidget`s.
///
/// Checked by `execute_slash_command()` before the standard
/// `SlashCommandRegistry` handler path.  When a factory is found, the
/// command produces `CommandResult::OpenMenu` instead of running the
/// usual handler.
pub struct CommandMenuRegistry {
    factories: HashMap<String, Arc<MenuFactory>>,
}

impl CommandMenuRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a factory for `command` (trigger name, no leading `/`).
    ///
    /// Replaces any previously registered factory for the same name.
    pub fn register(&mut self, command: &str, factory: MenuFactory) {
        self.factories.insert(command.to_string(), Arc::new(factory));
    }

    /// Look up the factory for `command`.
    ///
    /// Returns `None` when no factory is registered; the caller then
    /// falls through to the standard `SlashCommandRegistry` handler.
    pub fn get(&self, command: &str) -> Option<Arc<MenuFactory>> {
        self.factories.get(command).cloned()
    }

    /// Return the sorted list of command names that have registered menus.
    ///
    /// Used by `/help` to annotate menu-backed commands with an indicator.
    pub fn commands(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.factories.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Returns `true` if no factories are registered.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

impl Default for CommandMenuRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

---

## 6. `CommandResult::OpenMenu` Variant

### 6.1 Adding the variant

`CommandResult` is defined in `crates/xaft-tui/src/slash/mod.rs`:

```rust
// BEFORE
#[derive(Debug, Clone)]
pub enum CommandResult {
    Lines(Vec<String>),
    StyledLines(Vec<StyledLine>),
    Handled,
    Error(String),
    ConfigDisplay(Vec<ConfigSection>),
}
```

```rust
// AFTER
pub enum CommandResult {
    Lines(Vec<String>),
    StyledLines(Vec<StyledLine>),
    Handled,
    Error(String),
    ConfigDisplay(Vec<ConfigSection>),
    /// Open an interactive menu overlay driven by `MenuDriver`.
    ///
    /// # Clone constraints
    ///
    /// `Box<dyn MenuWidget>` is NOT `Clone` — trait objects behind `Box`
    /// cannot implement `Clone` unless the trait itself bounds on it
    /// (which `MenuWidget` does not, to keep it object-safe).
    ///
    /// Therefore `CommandResult` MUST NOT derive `Clone`. Remove the
    /// `#[derive(Clone)]` attribute and implement `Clone` manually for
    /// all non-`OpenMenu` variants, or use a `NonCloneCommandResult`
    /// wrapper pattern. The recommended approach is explicit `impl Clone`:
    ///
    /// ```rust
    /// impl Clone for CommandResult {
    ///     fn clone(&self) -> Self {
    ///         match self {
    ///             Self::Lines(v) => Self::Lines(v.clone()),
    ///             Self::StyledLines(v) => Self::StyledLines(v.clone()),
    ///             Self::Handled => Self::Handled,
    ///             Self::Error(s) => Self::Error(s.clone()),
    ///             Self::ConfigDisplay(v) => Self::ConfigDisplay(v.clone()),
    ///             Self::OpenMenu(_) => {
    ///                 // MenuWidget is not Clone. Callers that need to clone a
    ///                 // CommandResult must not produce OpenMenu on the source.
    ///                 // Panicking here makes accidental clones loud rather than silent.
    ///                 panic!("CommandResult::OpenMenu cannot be cloned — \
    ///                         do not clone results that may carry a MenuWidget")
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// In practice `CommandResult` is produced, consumed once by
    /// `apply_command_result()`, and discarded — it is never cloned in
    /// the production path. The `Clone` impl exists only because
    /// `CommandResult` is stored temporarily in test helpers that require
    /// `Clone`. If tests are updated to avoid that pattern the impl can
    /// be removed entirely.
    OpenMenu(Box<dyn MenuWidget>),
}
```

### 6.2 `Debug` for `OpenMenu`

`Box<dyn MenuWidget>` cannot derive `Debug` without an explicit bound.
Either add `Debug` to the `MenuWidget` supertrait (preferred, matches the
PRD-61 bound already anticipated there), or implement `Debug` manually:

```rust
impl std::fmt::Debug for CommandResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lines(v) => f.debug_tuple("Lines").field(v).finish(),
            Self::StyledLines(v) => f.debug_tuple("StyledLines").field(v).finish(),
            Self::Handled => write!(f, "Handled"),
            Self::Error(s) => f.debug_tuple("Error").field(s).finish(),
            Self::ConfigDisplay(v) => f.debug_tuple("ConfigDisplay").field(v).finish(),
            Self::OpenMenu(_) => write!(f, "OpenMenu(<dyn MenuWidget>)"),
        }
    }
}
```

---

## 7. `AppState` Changes

### 7.1 New field: `menu_registry`

Add to `AppState` in `crates/xaft-tui/src/state.rs`:

```rust
// ── Menu system ───────────────────────────────────────────────────────────────
/// Maps slash command names to menu widget factories.
/// Checked before `slash_registry` in `execute_slash_command()`.
pub menu_registry: CommandMenuRegistry,
/// Driver for the active interactive menu overlay (PRD-61).
pub menu_driver: MenuDriver,
```

### 7.2 `AppState::new()` — registry initialisation

Inside `AppState::new()` after the `slash_registry` block:

```rust
let mut menu_registry = CommandMenuRegistry::new();

// /config → ConfigurationMenu (navigable, inline-editable config table).
menu_registry.register(
    "config",
    Box::new(|ctx: CommandMenuContext| -> Box<dyn MenuWidget> {
        Box::new(ConfigurationMenu::new(ctx.config))
    }),
);

// Future registrations (e.g. /model, /theme) added here.
```

`CommandMenuRegistry` and `MenuDriver` are constructed here so that
`AppState::new_for_test()` (which delegates to `new()`) automatically
includes both — test coverage requires no extra setup.

### 7.3 `apply_command_result()` — `OpenMenu` arm

```rust
pub fn apply_command_result(&mut self, result: CommandResult) {
    match result {
        // … existing arms …

        CommandResult::OpenMenu(widget) => {
            // Hand the widget to the menu driver.  On the next frame,
            // menu_driver.is_active() == true and the renderer draws the
            // overlay.  No transcript mutation is produced; the menu is
            // purely ephemeral.
            self.menu_driver.open(widget);
            // Trigger a prompt re-render so the input bar transitions to
            // edit-field mode immediately (see §9).
            self.mutations
                .push(RenderMutation::UpdatePrompt(build_prompt(self)));
        }
    }
}
```

Note: the signature changes from `&CommandResult` to `CommandResult` (owned)
because `OpenMenu` contains a non-`Copy` `Box`. Callers pass `result` directly
rather than a reference. All existing call sites pass a freshly-produced value,
so this is a pure type-signature change with no behavioural impact.

### 7.4 `execute_slash_command()` — registry check

Insert a new guard at the top of `execute_slash_command()` in `state.rs`,
before the `slash_registry.get(&cmd)` call:

```rust
fn execute_slash_command(&mut self, cmd: SlashCommand, raw: &str) {
    let started = std::time::Instant::now();

    // /quit is always handled directly.
    if matches!(cmd, SlashCommand::Quit) { /* … existing … */ }

    // ── NEW: CommandMenuRegistry check ────────────────────────────────────
    // If this command has a registered menu factory, produce OpenMenu and
    // skip the standard SlashHandler entirely.
    let cmd_name = cmd.trigger_name().to_string();
    if let Some(factory) = self.menu_registry.get(&cmd_name) {
        let ctx = CommandMenuContext {
            config: Arc::clone(&self.xaft_config),
            working_dir: self.working_dir.clone(),
            session_id: self.session.as_ref().map(|s| s.id.to_string()),
        };
        let widget = factory(ctx);
        self.push_slash_separator(raw);
        self.apply_command_result(CommandResult::OpenMenu(widget));
        self.slash_history.push(raw.to_string());
        let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        emit_signal_if_bus(
            self.signal_bus.clone(),
            SlashCommandExecuted {
                name: cmd_name,
                args: String::new(),
                success: true,
                duration_ms,
            },
        );
        return;  // Do not fall through to SlashCommandRegistry.
    }
    // ── END NEW ───────────────────────────────────────────────────────────

    // … existing args extraction, handler lookup, execute, apply, history …
}
```

---

## 8. `/config` Command Migration

### 8.1 `ConfigHandler` becomes a no-op stub

Once `CommandMenuRegistry` intercepts `/config`, `ConfigHandler::execute()` is
never reached when a menu is registered. However:

- It must remain in `SlashCommandRegistry` because `validate()` asserts that
  every `SlashCommand` variant has a handler.
- It serves as the fallback if `CommandMenuRegistry` is not populated (e.g. in
  tests that construct `AppState` in reduced configurations).

No changes are required to `ConfigHandler` itself. The migration is achieved
purely by registering a factory in `AppState::new()`.

### 8.2 Conceptual diff

```
// BEFORE (always executed via SlashCommandRegistry)
ConfigHandler::execute(ctx) → CommandResult::ConfigDisplay(sections)
  → apply_command_result() → RenderMutation::CommitLine × N (static, permanent)

// AFTER (factory fires first via CommandMenuRegistry)
CommandMenuContext { config, working_dir, session_id }
  → ConfigurationMenu::new(ctx.config) : Box<dyn MenuWidget>
  → CommandResult::OpenMenu(widget)
  → apply_command_result() → menu_driver.open(widget)
  → next frame: renderer draws navigable overlay
```

The static `ConfigDisplay` rendering path in `apply_command_result()` is
**retained** as a fallback for tests and for any code path where a handler
explicitly returns `CommandResult::ConfigDisplay`. It is not removed.

---

## 9. Edit-Field Mode for the Input Bar

When a menu is active and the focused row supports inline editing, the normal
input bar is repurposed as the edit field for that row. This creates a
consistent UX: the user edits a config value in the same bar they type commands
in.

### 9.1 `build_prompt()` integration

`build_prompt()` in `crates/xaft-tui/src/prompt.rs` is extended:

```rust
pub fn build_prompt(state: &AppState) -> PromptState {
    // ── NEW: menu edit-field override ────────────────────────────────────────
    // When the menu driver is active and the focused widget row has an edit
    // value, replace the input bar lines with the edit value so the renderer
    // shows it in the prompt instead of normal typed text.
    if state.menu_driver.is_active() {
        if let Some(edit_val) = state.menu_driver.active_widget_edit_value() {
            return PromptState {
                lines: vec![edit_val],
                cursor: Cursor { row: 0, col: edit_val_len },
                scroll_top: 0,
                agent_active: state.phase.is_active(),
                is_empty: false,
                hidden_above: 0,
                hidden_below: 0,
                active_trigger: None,
                // NEW field (PRD-61): signals the renderer to draw an edit-mode
                // border style around the input bar.
                menu_edit_mode: true,
            };
        }
    }
    // ── END NEW ──────────────────────────────────────────────────────────────

    // … existing logic …
}
```

`MenuDriver::active_widget_edit_value()` is defined in PRD-61 as:

```rust
impl MenuDriver {
    /// Returns the current edit-field value from the focused row of the
    /// active widget, or `None` when no menu is open or the focused row
    /// is not in edit mode.
    pub fn active_widget_edit_value(&self) -> Option<String>;
}
```

### 9.2 Key routing while menu is active

When `menu_driver.is_active()`, key events in `AppState::handle_key()` must be
forwarded to the menu driver before reaching the input bar:

```rust
fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
    // … escape dialog check …

    // ── NEW: menu driver key routing ─────────────────────────────────────
    if self.menu_driver.is_active() {
        if self.menu_driver.handle_key(key) {
            // Menu consumed the key — update the prompt to reflect any
            // cursor movement or edit value change.
            self.mutations
                .push(RenderMutation::UpdatePrompt(build_prompt(self)));
            // Check if the menu closed as a result of this key.
            if !self.menu_driver.is_active() {
                // Restore the input bar to its pre-menu state.
                self.mutations
                    .push(RenderMutation::UpdatePrompt(build_prompt(self)));
            }
            return;
        }
    }
    // ── END NEW ──────────────────────────────────────────────────────────

    // … existing key handling (approval queue, escape dialog, trigger keys, etc.) …
}
```

`MenuDriver::handle_key()` returns `true` if it consumed the key. `Esc` always
returns `true` and closes the menu; arrow keys and Enter return `true` when in
an active menu.

---

## 10. Renderer / `app.rs` Integration

### 10.1 No changes to `app.rs`

The `TuiApp::run_inner()` event loop in `crates/xaft-tui/src/app.rs` does not
need modification. It already:

1. Drains `state.mutations` and passes them to `renderer`.
2. Calls `renderer.update_ephemeral(ephemeral::build(state), &prompt, &theme)`
   each tick.

`ephemeral::build()` already accesses `AppState`; it will be extended in
PRD-61 to include menu overlay lines in `EphemeralState`. Nothing in the
dispatch path requires touching `app.rs`.

### 10.2 `IncrementalRenderer` renders the overlay automatically

`IncrementalRenderer::update_ephemeral()` receives `EphemeralState` which,
after PRD-61, includes an optional `MenuOverlayLines` block. If present the
renderer draws it above the prompt separator in the ephemeral region. Since the
menu is ephemeral (redrawn every tick), it disappears instantly when
`menu_driver.is_active()` returns `false`.

---

## 11. Visual Flow

```
┌─────────────────────────────────────────────────────────────────────────┐
│  xaft conversation transcript (scrollback)                              │
│                                                                         │
│  ◈ Planner                                                              │
│    Analysing the codebase structure…                                    │
│                                                                         │
│  ╌╌ /config ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌│
│                                                                         │
│ ─────────────────────────────────────────────────────────────────────── │  ← separator
│                                                                         │
│  [core]                                                                 │  ┐
│  ▶  max_turns          25          int    default                       │  │ menu overlay
│     model              "claude-…"  str    project  *                    │  │ (ephemeral)
│     cost_limit_usd     5.0         f64    default                       │  │
│  [tui]                                                                  │  │
│     theme              "dark"      str    user     *                    │  │
│     show_exit_summary  true        bool   default                       │  ┘
│                                                                         │
│ ─────────────────────────────────────────────────────────────────────── │  ← separator
│ ❯ _                                                                     │  ← normal prompt
└─────────────────────────────────────────────────────────────────────────┘

User presses ↓ twice (moves focus to "model" row):

│  ▶  model              "claude-…"  str    project  *                   │  ← focused row

User presses Enter (opens inline edit):

│ ─────────────────────────────────────────────────────────────────────── │
│ ❯ "claude-3-5-sonnet-20241022"_                                        │  ← edit value in
│ ─────────────────────────────────────────────────────────────────────── │    prompt bar

User types the new value and presses Enter — value applied, menu row updates.

User presses Esc:

│ ─────────────────────────────────────────────────────────────────────── │
│ ❯ _                                                                     │  ← normal prompt
│                                                                         │    menu closed
```

---

## 12. Tests

The following scenarios must be covered. Full code is left to the implementer;
test bodies follow the patterns established in `state.rs` tests and the
`slash/commands/*.rs` unit test modules.

### 12.1 `CommandMenuRegistry` unit tests

Location: `crates/xaft-tui/src/menu/registry.rs` `#[cfg(test)] mod tests`

| Scenario | Assert |
|----------|--------|
| `new()` → `is_empty()` | `true` |
| `register("config", factory)` → `get("config")` | `Some(_)` |
| `get("unknown")` | `None` |
| `commands()` with three registered names | sorted, all three present |
| Registering the same name twice keeps the latest | `get` returns the second factory's output |

### 12.2 `CommandResult::OpenMenu` and manual `Clone` impl

Location: `crates/xaft-tui/src/slash/mod.rs` `#[cfg(test)] mod tests`

| Scenario | Assert |
|----------|--------|
| `clone()` on `Lines`, `Error`, `ConfigDisplay` variants | success |
| `clone()` on `OpenMenu` | `#[should_panic]` test — panics with expected message |
| `Debug` format of `OpenMenu` | contains `"OpenMenu"` |

### 12.3 `apply_command_result()` — `OpenMenu` arm

Location: `crates/xaft-tui/src/state.rs` `#[cfg(test)] mod tests`

Construct a `MockMenuWidget` (trivial struct implementing `MenuWidget` for
tests) and verify:

| Scenario | Assert |
|----------|--------|
| `apply_command_result(OpenMenu(mock))` | `state.menu_driver.is_active() == true` |
| No transcript mutations are produced | `state.mutations` contains only `UpdatePrompt` |
| Applying a second `OpenMenu` replaces the first | only one widget active |

### 12.4 `execute_slash_command()` — registry intercept

Location: `crates/xaft-tui/src/state.rs` integration-style test

```rust
#[test]
fn slash_config_opens_menu_when_factory_registered() {
    let mut state = AppState::new("");
    // menu_registry already has "config" registered in new().

    // Simulate "/config" submit.
    state.handle_char('/');
    for c in "config".chars() { state.handle_char(c); }
    state.handle_event(TuiEvent::Key(enter_key()));

    assert!(state.menu_driver.is_active(),
        "menu_driver must be active after /config");
    // Transcript must NOT contain a ConfigDisplay commit — the static
    // rendering path must be bypassed entirely.
    assert!(
        !state.mutations.iter().any(|m| matches!(m, RenderMutation::CommitLine(l)
            if l.text.contains("max_turns"))),
        "static ConfigDisplay lines must not appear when menu is opened"
    );
}
```

### 12.5 `build_prompt()` — edit-field mode

Location: `crates/xaft-tui/src/prompt.rs` `#[cfg(test)] mod tests`

| Scenario | Assert |
|----------|--------|
| `menu_driver.is_active() == false` | `PromptState::lines` equals input bar content |
| `menu_driver.is_active() == true`, no edit value | `PromptState::lines` equals input bar content |
| `menu_driver.is_active() == true`, edit value `"foo"` | `PromptState::lines == vec!["foo"]`, `menu_edit_mode == true` |

### 12.6 Key routing while menu active

Location: `crates/xaft-tui/src/state.rs` `#[cfg(test)] mod tests`

| Scenario | Assert |
|----------|--------|
| `↓` while menu active | consumed; `state.mutations` contains `UpdatePrompt` |
| `Esc` while menu active | `menu_driver.is_active() == false` after event |
| Character key `'a'` while menu NOT active | forwarded to input bar normally |

### 12.7 Fallthrough to `SlashCommandRegistry`

| Scenario | Assert |
|----------|--------|
| `/help` (no factory registered) | `menu_driver.is_active() == false`; transcript has help lines |
| `/cost` (no factory registered) | uses standard handler path unchanged |

---

## 13. Key Rust-Specific Notes

### 13.1 `Box<dyn MenuWidget>` is not `Clone`

Rust trait objects (`dyn Trait`) do not implement `Clone` automatically. The
standard workaround — adding `fn clone_box(&self) -> Box<dyn Trait>` to the
trait — is unnecessary here because `CommandResult` is never intentionally
cloned in the production path. The `OpenMenu` arm of the manual `Clone` impl
panics to catch accidental clones early.

If in future `CommandResult` must be genuinely cloned (e.g. for undo history),
add `fn clone_box(&self) -> Box<dyn MenuWidget>` to `MenuWidget` and call it
in the `Clone` impl:

```rust
// Option A: if MenuWidget grows clone_box()
Self::OpenMenu(w) => Self::OpenMenu(w.clone_box()),
```

### 13.2 `MenuFactory` closure bounds

```rust
pub type MenuFactory =
    Box<dyn Fn(CommandMenuContext) -> Box<dyn MenuWidget> + Send + Sync + 'static>;
```

- `Send + Sync`: `CommandMenuRegistry` is stored in `AppState` which is
  accessed from async contexts. Both bounds are required for `Arc<MenuFactory>`
  to be `Send + Sync`.
- `'static`: closures that capture `Arc<XaftConfig>` are `'static` because
  `Arc<T>` is always `'static` when `T: 'static`. The `+ 'static` bound also
  ensures that no stack-borrowed data leaks into the factory.

### 13.3 `Arc<MenuFactory>` vs `MenuFactory`

`CommandMenuRegistry` stores `Arc<MenuFactory>` (not `MenuFactory`) so that
`get()` can return an `Arc` clone without moving the factory out of the map.
This allows the factory to be called without holding a mutable borrow on the
registry:

```rust
// CORRECT: get() returns Arc clone; borrow on registry ends here.
let factory = self.menu_registry.get(&cmd_name)?;
let widget = factory(ctx);       // factory called without borrow on self

// INCORRECT: storing MenuFactory directly would require either a borrow
// that outlives the call to apply_command_result(), or a remove+insert dance.
```

### 13.4 `apply_command_result()` signature change

The existing signature is:

```rust
pub fn apply_command_result(&mut self, result: &CommandResult)
```

It must become:

```rust
pub fn apply_command_result(&mut self, result: CommandResult)
```

because `OpenMenu(Box<dyn MenuWidget>)` cannot be taken by shared reference and
passed into `menu_driver.open()` (which consumes the widget). This is a
breaking change to any code that borrows a `CommandResult` and then calls
`apply_command_result`. In practice there is exactly one call site in
`execute_slash_command()`:

```rust
// BEFORE
let result = handler.execute(ctx);
self.apply_command_result(&result);

// AFTER
let result = handler.execute(ctx);
self.apply_command_result(result);
```

### 13.5 `MenuWidget` object safety

`MenuWidget` (defined in PRD-61) must be object-safe for `Box<dyn MenuWidget>`
to compile. The following patterns break object safety and must not appear in
`MenuWidget`:

```rust
// BREAKS object safety — generic method
fn render_to<W: Write>(&self, writer: W);

// BREAKS object safety — returns Self
fn clone(&self) -> Self;

// OK — returns Box<dyn Trait>
fn clone_box(&self) -> Box<dyn MenuWidget>;
```

### 13.6 `PromptState` — new `menu_edit_mode` field

The `PromptState` struct in `prompt.rs` gains a new boolean field. Since
`PromptState` derives `Default`, the field must also have a sensible default:

```rust
#[derive(Debug, Clone, Default)]
pub struct PromptState {
    // … existing fields …

    /// When `true`, the renderer draws the input bar with an edit-mode
    /// border style to signal that the user is editing a menu field value,
    /// not composing a new command.
    pub menu_edit_mode: bool,
}
```

This field is `false` by default, preserving all existing renderer behaviour
when no menu is open.

---

## 14. File Locations Summary

| New / changed | Path |
|---------------|------|
| `CommandMenuContext`, `MenuFactory`, `CommandMenuRegistry` | `crates/xaft-tui/src/menu/mod.rs` (new module, or `menu/registry.rs` under it) |
| `CommandResult::OpenMenu` variant + manual `Debug`/`Clone` | `crates/xaft-tui/src/slash/mod.rs` |
| `menu_registry`, `menu_driver` fields | `crates/xaft-tui/src/state.rs` |
| `execute_slash_command()` registry check | `crates/xaft-tui/src/state.rs` |
| `apply_command_result()` `OpenMenu` arm + signature | `crates/xaft-tui/src/state.rs` |
| `build_prompt()` edit-field override | `crates/xaft-tui/src/prompt.rs` |
| `handle_key()` menu routing | `crates/xaft-tui/src/state.rs` |
| `PromptState::menu_edit_mode` | `crates/xaft-tui/src/prompt.rs` |
| `pub mod menu;` declaration | `crates/xaft-tui/src/lib.rs` |

No changes are needed in `app.rs`, `renderer.rs`, `slash/registry.rs`, or any
of the other slash command handler files.

---

## 15. Implementation Order

1. PRD-61 (`MenuWidget`, `MenuDriver`) — this PRD hard-depends on both.
2. Add `CommandMenuContext`, `MenuFactory`, `CommandMenuRegistry` in
   `menu/registry.rs`. Write unit tests (§12.1).
3. Add `CommandResult::OpenMenu`, manual `Debug`/`Clone` impls. Write tests
   (§12.2).
4. Add `menu_registry` + `menu_driver` to `AppState`. Wire `AppState::new()`
   registration. Adjust `apply_command_result()` signature. Write tests
   (§12.3, §12.4).
5. Extend `execute_slash_command()` with registry check. Write integration test
   (§12.4).
6. Extend `build_prompt()` with edit-field override. Add `menu_edit_mode` field
   to `PromptState`. Write tests (§12.5).
7. Extend `handle_key()` with menu key routing. Write tests (§12.6, §12.7).
8. Remove `#[derive(Clone)]` from `CommandResult` and confirm all callers
   compile without explicit clones on `CommandResult`.
9. Run `cargo test --workspace` and `cargo clippy --workspace`.
