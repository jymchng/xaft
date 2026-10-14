---
title: "PRD-60: Slash Command Trigger and Input History"
status: draft
version: 0.1.0
created: 2026-06-13
depends-on:
  - 59_input_trigger_system.md        # TriggerHandler, TriggerRegistry, TriggerContext, MatchItem, ActiveTrigger
  - 52_skills_system.md               # skill registration hooks
  - 57_dynamic_tool_creation.md       # XaftDynamicToolCreated signal; dynamic command registration
  - 32c_slash_command_registry.md     # SlashCommandRegistry foundation
  - 34_slash_palette_overlay_and_config_editor.md  # existing SlashPalette design
---

# PRD-60: Slash Command Trigger and Input History

## Executive Summary

PRD-59 (Input Trigger System) defines the generic `TriggerHandler` protocol, `TriggerRegistry`,
`TriggerContext`, `MatchItem`, and `ActiveTrigger`.  This PRD defines the **three concrete
implementations** that plug into that system for the xaft TUI:

- **Feature A — `SlashCommandTriggerHandler`**: Replaces the static `SlashPalette`/`COMMAND_TABLE`
  system with a live-registry-backed, grouped, fuzzy-searchable dropdown connected to the real
  `SlashCommandRegistry`.  Introduces `CommandCatalog` as the single source of truth for all
  slash commands — built-in, skill-sourced, and dynamically registered at runtime.

- **Feature B — `HistoryTriggerHandler`**: Adds a `#`-prefix trigger and Up-arrow shortcut that
  surfaces a searchable dropdown of all previous input bar submissions (both agent tasks and slash
  commands), with fuzzy filtering and human-readable time labels.

- **Feature C — Dynamic Command Registration**: Plumbs `XaftCommandRegistered` signals and an
  `AppState::register_command()` API so skills and dynamic tools (PRD-57) can contribute slash
  commands that appear in the autocomplete palette immediately, without restart.

---

## Goals

| ID | Goal |
|----|------|
| G1 | Typing `/` opens a grouped, fuzzy-filtered dropdown that reads from the live `SlashCommandRegistry`, not a static table. |
| G2 | Commands are visually grouped: Recent, Navigation, Agent, Git, Tools, Skills, Dynamic, Other. |
| G3 | The recently-used section shows the last N commands at the top of an empty-prefix search. |
| G4 | Fuzzy prefix matching finds partial matches across both canonical names and aliases. |
| G5 | Each row shows `/name` (bold when selected), description (truncated), and `[args_hint]` right-aligned. |
| G6 | Group headers are non-selectable and separated by a dim `─` line. |
| G7 | Skills registered via PRD-52 appear in the `Skills` group automatically. |
| G8 | Dynamic commands (PRD-57 `tool_factory`) register via signal and appear in the `Dynamic` group immediately. |
| G9 | Typing `#` (or pressing Up arrow in an empty input bar) opens a history dropdown of all prior submissions. |
| G10 | History entries show full text (first 60 chars), time-ago label, and submission kind label. |
| G11 | Selecting a history item restores the full text into the input bar (not just the preview). |
| G12 | `#prefix` filters history by substring search on the full text. |
| G13 | Up-arrow in an empty buffer activates the history trigger without requiring the `#` character. |
| G14 | `CommandCatalog` and `InputHistoryStore` are both `Arc<RwLock<…>>` so they can be updated from async signal handlers. |

---

## Background and Motivation

### Current problems with `SlashPalette`

`SlashPalette` (in `crates/xaft-tui/src/slash/palette.rs`) is created by `SlashPalette::new(partial)` which:

1. Calls `SlashCommandParser::completions(partial)` to match against `COMMAND_TABLE` — a static
   `&[(&str, CommandMeta)]` compiled into the binary.
2. Pulls `description` and `args_hint` from the same static table by linear scan.

This creates three problems that grow worse as xaft gains more dynamic surfaces:

**Problem 1 — Stale on dynamic registration.**  Skills (PRD-52) and dynamic tools (PRD-57) can
register new slash commands at runtime.  `COMMAND_TABLE` is `static` and cannot reflect these.
The `SlashCommandRegistry` already holds the live handlers, but the palette never reads from it.

**Problem 2 — No grouping.**  All 20+ commands appear in a single flat list sorted alphabetically.
As the list grows (skills may add 10–20 more), finding the right command becomes tedious.

**Problem 3 — No "recently used" section.**  Users repeatedly type the same 3–4 commands
(`/compact`, `/cost`, `/clear`, `/model`).  An empty-prefix search should surface these first.

### Why not just extend `SlashPalette`?

`SlashPalette` is tightly coupled to `COMMAND_TABLE` (static `&'static str` slices everywhere).
Extending it to support dynamic entries would require changing the lifetime of every field from
`&'static str` to `String`, re-architecting the search loop, and adding grouping on top of a type
that was designed as a thin filter over a sorted array.  Given that PRD-59 defines a clean
`TriggerHandler` abstraction, the correct approach is to build `SlashCommandTriggerHandler` that
implements that trait and retires `SlashPalette`.

---

## Feature A — Enhanced Slash Command Trigger

### A.1 `CommandEntry`

A richer command descriptor that carries all metadata needed for display and routing.  Unlike
`CommandMeta` (which uses `&'static str` and is tied to compile-time data), `CommandEntry` uses
owned `String`s so dynamic and skill-sourced commands can be represented equally.

```rust
// crates/xaft-tui/src/trigger/slash_command.rs

use std::time::Instant;

/// Owned descriptor for a single slash command.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    /// Canonical name without leading slash, e.g. `"compact"`.
    pub name: String,
    /// Additional names that resolve to this command, e.g. `["ctx"]`.
    pub aliases: Vec<String>,
    /// One-line description shown in the palette and `/help` output.
    pub description: String,
    /// Optional argument syntax hint, e.g. `"[session-id]"`.
    pub args_hint: Option<String>,
    /// Display group for the palette.
    pub group: CommandGroup,
    /// Where this command was registered from.
    pub source: CommandSource,
}
```

### A.2 `CommandGroup`

Controls both the palette section header and the display order.  The ordering is fixed: Recent
(virtual, computed at search time), then the groups below in order.

```rust
/// Categorisation of a slash command for grouped display in the palette.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandGroup {
    /// `/clear`, `/compact`, `/resume`, `/rewind`, `/quit`
    Navigation,
    /// `/agents`, `/model`, `/cost`, `/compact` (also Navigation)
    Agent,
    /// `/diff`, `/commit`, `/pr`
    Git,
    /// `/mcp`, `/permissions`, `/doctor`, `/init`
    Tools,
    /// Commands loaded from xaft-skills at session startup.
    Skills,
    /// Commands registered at runtime via `tool_factory` or MCP.
    Dynamic,
    /// Everything else: `/help`, `/config`, `/theme`, `/vim`, `/emacs`, `/login`, `/logout`, `/memory`
    Other,
}

impl CommandGroup {
    /// Display label shown as a group header in the palette.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Agent      => "Agent",
            Self::Git        => "Git",
            Self::Tools      => "Tools",
            Self::Skills     => "Skills",
            Self::Dynamic    => "Dynamic",
            Self::Other      => "Other",
        }
    }

    /// Palette display priority (lower = appears first, after Recent).
    pub fn sort_order(&self) -> u8 {
        match self {
            Self::Navigation => 0,
            Self::Agent      => 1,
            Self::Git        => 2,
            Self::Tools      => 3,
            Self::Skills     => 4,
            Self::Dynamic    => 5,
            Self::Other      => 6,
        }
    }
}
```

### A.3 `CommandSource`

Tracks where a command was registered from, used by `/help` output and audit logs.

```rust
/// Provenance of a `CommandEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    /// Hard-coded in `COMMAND_TABLE` / compiled into the binary.
    BuiltIn,
    /// Contributed by a loaded skill at session startup.
    Skill { skill_name: String },
    /// Registered at runtime (via `tool_factory` or direct API call).
    Dynamic { registered_at: Instant },
    /// Contributed by an MCP server.
    Mcp { server: String },
}
```

### A.4 `CommandCatalog`

Replaces `COMMAND_TABLE` as the single source of truth for slash command discovery.  It is built
once from the `SlashCommandRegistry` at session startup, then extended via `register_dynamic()`
as skills and dynamic tools contribute new commands during the session.

```rust
use std::collections::{HashMap, VecDeque};

/// Live registry of all slash commands known to the TUI.
///
/// Built from `SlashCommandRegistry` at startup, then extended dynamically.
/// Wrapped in `Arc<RwLock<CommandCatalog>>` everywhere it is shared.
pub struct CommandCatalog {
    /// All entries in insertion order (built-ins first, then skills, then dynamic).
    entries: Vec<CommandEntry>,
    /// Maps canonical name and every alias → index in `entries`.
    by_name: HashMap<String, usize>,
    /// Ring buffer of recently-used canonical command names (newest first).
    recently_used: VecDeque<String>,
    /// Maximum number of recently-used entries to keep.
    max_recently_used: usize,
}

impl CommandCatalog {
    /// Create an empty catalog.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_name: HashMap::new(),
            recently_used: VecDeque::new(),
            max_recently_used: 10,
        }
    }

    /// Build a catalog from the built-in `COMMAND_TABLE`, deduplicating aliases.
    ///
    /// Only canonical entries are added (entries where `meta.canonical == trigger`).
    /// Aliases are recorded in the entry's `aliases` vec and mapped in `by_name`.
    pub fn from_command_table() -> Self {
        use crate::slash::parser::COMMAND_TABLE;
        let mut catalog = Self::new();
        for (trigger, meta) in COMMAND_TABLE {
            // Skip alias rows — each command appears once under its canonical name.
            if *trigger != meta.canonical {
                continue;
            }
            let group = CommandGroup::from_canonical(meta.canonical);
            let entry = CommandEntry {
                name: meta.canonical.to_string(),
                aliases: meta.aliases.iter().map(|a| a.to_string()).collect(),
                description: meta.description.to_string(),
                args_hint: meta.args_hint.map(|s| s.to_string()),
                group,
                source: CommandSource::BuiltIn,
            };
            catalog.add_entry(entry);
        }
        catalog
    }

    /// Register a new dynamic entry.  If a command with the same canonical
    /// name already exists, it is replaced (last-writer-wins — skills may
    /// override built-in descriptions).
    pub fn register_dynamic(&mut self, entry: CommandEntry) {
        let name = entry.name.clone();
        let aliases = entry.aliases.clone();
        // Remove any existing mapping for this name to allow replacement.
        if let Some(&old_idx) = self.by_name.get(&name) {
            let old = &self.entries[old_idx];
            for alias in &old.aliases {
                self.by_name.remove(alias);
            }
            self.entries[old_idx] = entry;
            self.by_name.insert(name, old_idx);
            for alias in aliases {
                self.by_name.insert(alias, old_idx);
            }
        } else {
            let idx = self.entries.len();
            self.by_name.insert(name, idx);
            for alias in &aliases {
                self.by_name.insert(alias.clone(), idx);
            }
            self.entries.push(entry);
        }
    }

    /// Flat search: return entries whose canonical name or any alias starts
    /// with `partial` (case-insensitive prefix match).
    ///
    /// When `partial` is empty: returns all entries sorted by group order,
    /// then alphabetically within each group.
    /// When `partial` is non-empty: returns matches sorted alphabetically.
    pub fn search<'a>(&'a self, partial: &str) -> Vec<&'a CommandEntry> {
        let p = partial.to_ascii_lowercase();
        let mut result: Vec<&CommandEntry> = self
            .entries
            .iter()
            .filter(|e| {
                if p.is_empty() {
                    true
                } else {
                    e.name.starts_with(p.as_str())
                        || e.aliases.iter().any(|a| a.starts_with(p.as_str()))
                }
            })
            .collect();
        result.sort_by(|a, b| {
            if p.is_empty() {
                a.group.sort_order().cmp(&b.group.sort_order())
                    .then_with(|| a.name.cmp(&b.name))
            } else {
                a.name.cmp(&b.name)
            }
        });
        result
    }

    /// Grouped search: return `(group_label, entries)` pairs, in group
    /// display order.  Groups with no matches are omitted.
    ///
    /// The "Recent" pseudo-group is prepended when `partial` is empty and
    /// there are recently-used entries.
    pub fn grouped_search<'a>(
        &'a self,
        partial: &str,
    ) -> Vec<(&'static str, Vec<&'a CommandEntry>)> {
        let mut out: Vec<(&'static str, Vec<&'a CommandEntry>)> = Vec::new();

        // Prepend "Recent" pseudo-group when prefix is empty.
        if partial.is_empty() && !self.recently_used.is_empty() {
            let recent: Vec<&CommandEntry> = self
                .recently_used
                .iter()
                .filter_map(|name| self.by_name.get(name).and_then(|&i| self.entries.get(i)))
                .collect();
            if !recent.is_empty() {
                out.push(("Recent", recent));
            }
        }

        // Collect regular groups.
        let all_groups = [
            CommandGroup::Navigation,
            CommandGroup::Agent,
            CommandGroup::Git,
            CommandGroup::Tools,
            CommandGroup::Skills,
            CommandGroup::Dynamic,
            CommandGroup::Other,
        ];
        for group in &all_groups {
            let p = partial.to_ascii_lowercase();
            let mut entries: Vec<&CommandEntry> = self
                .entries
                .iter()
                .filter(|e| {
                    &e.group == group && {
                        if p.is_empty() {
                            true
                        } else {
                            e.name.starts_with(p.as_str())
                                || e.aliases.iter().any(|a| a.starts_with(p.as_str()))
                        }
                    }
                })
                .collect();
            if !entries.is_empty() {
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                out.push((group.label(), entries));
            }
        }
        out
    }

    /// Record a command as recently used.  Deduplicates and keeps most-recent first.
    pub fn record_used(&mut self, name: &str) {
        self.recently_used.retain(|n| n.as_str() != name);
        self.recently_used.push_front(name.to_string());
        while self.recently_used.len() > self.max_recently_used {
            self.recently_used.pop_back();
        }
    }

    /// Look up an entry by canonical name or alias.
    pub fn get(&self, name: &str) -> Option<&CommandEntry> {
        self.by_name.get(name).and_then(|&i| self.entries.get(i))
    }

    /// Total number of canonical entries (not counting aliases).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // Internal: add without dedup check (used during initial construction).
    fn add_entry(&mut self, entry: CommandEntry) {
        let idx = self.entries.len();
        self.by_name.insert(entry.name.clone(), idx);
        for alias in &entry.aliases {
            self.by_name.insert(alias.clone(), idx);
        }
        self.entries.push(entry);
    }
}
```

### A.5 `CommandGroup::from_canonical`

Maps a canonical command name to its `CommandGroup`.  Built-in classification is done here so
the `COMMAND_TABLE` parser does not need to change.

```rust
impl CommandGroup {
    /// Classify a built-in canonical command name.
    pub fn from_canonical(name: &str) -> Self {
        match name {
            "clear" | "compact" | "resume" | "rewind" | "quit" => Self::Navigation,
            "agents" | "model" | "cost" => Self::Agent,
            "diff" | "commit" | "pr" => Self::Git,
            "mcp" | "permissions" | "doctor" | "init" => Self::Tools,
            _ => Self::Other,
        }
    }
}
```

### A.6 `SlashCommandTriggerHandler`

Implements the `TriggerHandler` trait from PRD-59.  Wraps `Arc<RwLock<CommandCatalog>>` so the
catalog can be updated from signal handlers without rebuilding the handler.

```rust
use std::sync::{Arc, RwLock};
use crate::trigger::{TriggerHandler, TriggerContext, MatchItem};

/// Maximum number of entries to show per group in the palette.
pub const MAX_PER_GROUP: usize = 5;

/// `TriggerHandler` implementation for the `/` character.
///
/// Reads from a shared `CommandCatalog` so dynamically-registered commands
/// (skills, `tool_factory`, MCP) appear without restart.
pub struct SlashCommandTriggerHandler {
    /// Shared, lock-guarded catalog.
    pub catalog: Arc<RwLock<CommandCatalog>>,
    /// Maximum items per group shown in the palette (default: 5).
    pub max_per_group: usize,
    /// Whether to show the "Recent" pseudo-group (default: true).
    pub show_recently_used: bool,
    /// Whether to render group headers in the palette (default: true).
    pub show_groups: bool,
}

impl SlashCommandTriggerHandler {
    /// Create a handler backed by the given catalog.
    pub fn new(catalog: Arc<RwLock<CommandCatalog>>) -> Self {
        Self {
            catalog,
            max_per_group: MAX_PER_GROUP,
            show_recently_used: true,
            show_groups: true,
        }
    }
}

impl TriggerHandler for SlashCommandTriggerHandler {
    /// This handler activates on `/`.
    fn trigger_char(&self) -> char {
        '/'
    }

    /// Return `MatchItem`s for the current partial prefix.
    ///
    /// When `ctx.prefix` is empty: recently-used first, then groups in order.
    /// When `ctx.prefix` is non-empty: flat fuzzy-prefix search, up to
    /// `max_per_group * 3` total items (no group headers).
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let catalog = self.catalog.read().expect("catalog RwLock poisoned");
        let partial = ctx.prefix;

        if self.show_groups && partial.is_empty() {
            // Grouped mode: emit group-header sentinel items interleaved with command items.
            let groups = catalog.grouped_search(partial);
            let mut items: Vec<MatchItem> = Vec::new();
            for (label, entries) in groups {
                // Group-header sentinel: display = label, insert = empty, is_header = true.
                items.push(MatchItem {
                    display: label.to_string(),
                    insert: String::new(),
                    hint: None,
                    is_header: true,
                    kind: crate::trigger::MatchKind::GroupHeader,
                    metadata: None,
                });
                for entry in entries.iter().take(self.max_per_group) {
                    items.push(Self::entry_to_match_item(entry));
                }
            }
            items
        } else {
            // Flat mode: straight prefix search.
            let entries = catalog.search(partial);
            entries
                .iter()
                .take(self.max_per_group * 3)
                .map(|e| Self::entry_to_match_item(e))
                .collect()
        }
    }

    /// Called when the user selects an item with Enter or Tab.
    ///
    /// Returns `"/" + name + " "` (trailing space allows immediate arg entry).
    /// If the command takes no args (`args_hint` is `None`), the trailing space
    /// is still appended; the submit handler strips it before parsing.
    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        if item.is_header {
            // Group headers are not selectable; return empty to stay in palette.
            return String::new();
        }
        format!("/{} ", item.insert)
    }

    /// Free text is allowed: the user can type `/unknowncommand` and submit it.
    /// The parser will return an error that is shown in the transcript.
    fn allows_free_text(&self) -> bool {
        true
    }

    /// Called after `on_select` to record the command in the recently-used list.
    fn on_confirm(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) {
        if !item.is_header && !item.insert.is_empty() {
            if let Ok(mut catalog) = self.catalog.write() {
                catalog.record_used(&item.insert);
            }
        }
    }
}

impl SlashCommandTriggerHandler {
    fn entry_to_match_item(entry: &CommandEntry) -> MatchItem {
        // hint: "description  [args_hint]"
        let hint = match &entry.args_hint {
            Some(h) => Some(format!("{}  [{}]", entry.description, h)),
            None => Some(entry.description.clone()),
        };
        MatchItem {
            display: entry.name.clone(),
            insert: entry.name.clone(),
            hint,
            is_header: false,
            kind: crate::trigger::MatchKind::Command,
            metadata: None,
        }
    }
}
```

### A.7 Palette Column Layout

The renderer for `SlashCommandTriggerHandler` items uses a three-column layout within the
popup box drawn by the trigger overlay widget (defined in PRD-59):

```
  ┌──────────────────────────────────────────────────────────────────┐
  │ Recent                                                            │
  │▶ /compact    Manually trigger context compaction                 │
  │  /cost       Show per-agent token and cost table                 │
  │ Navigation                                            ───────── │
  │  /clear      Reset the current session transcript               │
  │  /resume     Resume a prior session                    [sess-id] │
  │  /rewind     Roll back transcript to message N              [N]  │
  │  /quit       Exit xaft                                           │
  │  ↑/↓ navigate  ·  Tab complete  ·  Enter execute  ·  Esc close  │
  └──────────────────────────────────────────────────────────────────┘
❯ /co_
```

Column widths (all relative to popup inner width `W`):
- Selection indicator: 2 cols (`▶ ` or `  `)
- `/name`: fixed 14 cols, bold when selected, dim for group-header rows
- Description: fills `W - 14 - right_col - 4`, truncated with `…`
- `[args_hint]`: right-aligned, up to 12 cols, dim style
- Group header rows: span full width, bold label, preceded by a dim `─` separator
  (the "Recent" group has no separator above it)

---

## Feature B — History Recall Trigger

### B.1 `HistoryEntry` and `HistoryKind`

```rust
// crates/xaft-tui/src/trigger/history.rs

use std::time::Instant;

/// A single entry in the input history store.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// The full text that was submitted.
    pub text: String,
    /// Wall-clock instant of submission.
    pub submitted_at: Instant,
    /// What kind of submission this was.
    pub kind: HistoryKind,
}

/// Classification of a history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryKind {
    /// A natural-language task sent to the agent.
    AgentTask,
    /// A `/command` that was executed.
    SlashCommand,
}

impl HistoryKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::AgentTask    => "task",
            Self::SlashCommand => "cmd",
        }
    }
}
```

### B.2 `InputHistoryStore`

Supersedes `SlashHistory` for general-purpose history.  `SlashHistory` (in `palette.rs`) is
retained as-is for the cursor-based Up/Down navigation that does not use the trigger system.
`InputHistoryStore` is the richer store that feeds the `#` trigger and can be searched.

```rust
use std::collections::VecDeque;

/// Ring buffer of all user input bar submissions.
///
/// Captures both agent tasks and slash commands.
/// Wrapped in `Arc<RwLock<InputHistoryStore>>` everywhere it is shared.
pub struct InputHistoryStore {
    entries: VecDeque<HistoryEntry>,
    /// Maximum number of entries retained (default: 50).
    pub max_entries: usize,
}

impl InputHistoryStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
        }
    }

    /// Push a new entry.  Adjacent identical texts are not deduplicated
    /// (unlike `SlashHistory`) — timing matters for the history view.
    pub fn push(&mut self, text: String, kind: HistoryKind) {
        if self.entries.len() >= self.max_entries {
            self.entries.pop_back();
        }
        self.entries.push_front(HistoryEntry {
            text,
            submitted_at: Instant::now(),
            kind,
        });
    }

    /// Return all entries whose `text` contains `query` (case-insensitive),
    /// newest first.  An empty query returns all entries.
    pub fn search(&self, query: &str) -> Vec<&HistoryEntry> {
        let q = query.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| q.is_empty() || e.text.to_ascii_lowercase().contains(q.as_str()))
            .collect()
    }

    /// Total number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
```

### B.3 Time-Ago Formatting

```rust
/// Format an `Instant` as a human-readable relative time label.
pub fn time_ago(submitted_at: Instant) -> String {
    let secs = submitted_at.elapsed().as_secs();
    match secs {
        0..=59       => format!("{}s ago",   secs),
        60..=3599    => format!("{}m ago",   secs / 60),
        3600..=86399 => format!("{}h ago",   secs / 3600),
        _            => format!("{}d ago",   secs / 86400),
    }
}
```

### B.4 `HistoryTriggerHandler`

```rust
use std::sync::{Arc, RwLock};
use crate::trigger::{TriggerHandler, TriggerContext, MatchItem, MatchKind};

/// `TriggerHandler` implementation for the `#` character.
///
/// Shows a searchable list of prior input bar submissions, newest first.
pub struct HistoryTriggerHandler {
    /// Shared, lock-guarded history store.
    pub history: Arc<RwLock<InputHistoryStore>>,
    /// If true, show the AgentTask/SlashCommand kind label in the hint.
    pub show_kind: bool,
    /// Maximum items to show (default: 20).
    pub max_items: usize,
}

impl HistoryTriggerHandler {
    pub fn new(history: Arc<RwLock<InputHistoryStore>>) -> Self {
        Self {
            history,
            show_kind: true,
            max_items: 20,
        }
    }
}

impl TriggerHandler for HistoryTriggerHandler {
    fn trigger_char(&self) -> char {
        '#'
    }

    /// Return `MatchItem`s for the current prefix (substring search on full text).
    ///
    /// `MatchItem.display` = first 60 chars of the entry text.
    /// `MatchItem.insert`  = full entry text (restored into input bar on select).
    /// `MatchItem.hint`    = "time_ago  ·  kind"
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let store = self.history.read().expect("history RwLock poisoned");
        store
            .search(ctx.prefix)
            .iter()
            .take(self.max_items)
            .map(|entry| {
                let display = if entry.text.chars().count() > 60 {
                    let truncated: String = entry.text.chars().take(57).collect();
                    format!("{}…", truncated)
                } else {
                    entry.text.clone()
                };
                let hint = if self.show_kind {
                    Some(format!(
                        "{}  ·  {}",
                        time_ago(entry.submitted_at),
                        entry.kind.label()
                    ))
                } else {
                    Some(time_ago(entry.submitted_at))
                };
                MatchItem {
                    display,
                    insert: entry.text.clone(),
                    hint,
                    is_header: false,
                    kind: MatchKind::History,
                    metadata: None,
                }
            })
            .collect()
    }

    /// Selecting a history item restores the full text into the input bar.
    /// The trigger prefix (`#query`) is replaced entirely by `item.insert`.
    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    /// History trigger does not allow free text: pressing Enter with no
    /// selection closes the dropdown without changing the buffer.
    fn allows_free_text(&self) -> bool {
        false
    }
}
```

### B.5 History Palette Display

```
  ┌──────────────────────────────────────────────────────────────────┐
  │▶ Fix the type error in src/auth.rs                  2m ago · task│
  │  Add unit tests for the payment module             1h ago · task │
  │  /compact                                         3h ago · cmd  │
  │  Refactor the database connection pool          yesterday · task │
  │  Implement the OAuth2 PKCE flow                  2d ago · task  │
  │  ↑/↓ navigate  ·  Enter restore  ·  Esc close                   │
  └──────────────────────────────────────────────────────────────────┘
❯ #fix_
```

Column widths:
- Selection indicator: 2 cols
- Text preview: fills `W - right_col - 4`, truncated with `…`
- Time + kind label: right-aligned, up to 18 cols, dim style

### B.6 Up-Arrow Integration

When the input buffer is completely empty and the user presses Up, the TUI should open the history
dropdown as if the user had typed `#`.  This is a special case in `AppState::handle_key()` that
runs **before** the normal trigger routing:

```rust
// In crates/xaft-tui/src/state.rs — inside handle_key() for KeyCode::Up:

if key.code == KeyCode::Up && self.input_bar.is_empty() && self.active_trigger.is_none() {
    // Activate the history trigger with an empty prefix.
    self.activate_trigger('#', "");
    return Ok(());
}
```

`activate_trigger(char, prefix)` is a helper method on `AppState` added by PRD-59 that:
1. Looks up the `TriggerHandler` for `char` in `self.trigger_registry`.
2. Calls `handler.matches(ctx)` with the given prefix.
3. Sets `self.active_trigger = Some(ActiveTrigger { ... })`.

The Up-arrow integration bypasses typing `#` entirely — the `#` is never inserted into the
buffer.  Esc closes the dropdown and leaves the buffer empty (unchanged).

---

## Feature C — Dynamic Command Registration

### C.1 `XaftCommandRegistered` Signal

Added to `crates/xaft-agent/src/signals.rs`:

```rust
/// Emitted when a new slash command is registered at runtime.
///
/// Sources:
/// - Skills registered at session startup (from PRD-52 `SkillLoader`).
/// - Dynamic tools created via `tool_factory` (PRD-57).
/// - MCP servers that expose slash-command schemas.
///
/// The TUI subscribes to this signal and calls
/// `AppState::register_command()` to update the `CommandCatalog`.
#[derive(Debug, Clone)]
pub struct XaftCommandRegistered {
    /// Canonical command name (no leading slash), e.g. `"deploy"`.
    pub name: String,
    /// Provenance string: `"skill:<name>"`, `"dynamic"`, or `"mcp:<server>"`.
    pub source: String,
    /// One-line description for the palette and `/help`.
    pub description: String,
    /// Optional argument syntax hint.
    pub args_hint: Option<String>,
}
```

### C.2 `TuiEvent::CommandRegistered`

Added to `crates/xaft-tui/src/bridge.rs`:

```rust
// In the TuiEvent enum:
/// A new slash command was registered at runtime.
CommandRegistered {
    name: String,
    source: String,
    description: String,
    args_hint: Option<String>,
},
```

And in `EventBridge::attach_xaft_signals()`:

```rust
let tx = self.tx.clone();
bus.on::<XaftCommandRegistered>(move |ev| {
    let _ = tx.send(TuiEvent::CommandRegistered {
        name: ev.name.clone(),
        source: ev.source.clone(),
        description: ev.description.clone(),
        args_hint: ev.args_hint.clone(),
    });
}).await;
```

### C.3 `AppState::register_command()`

```rust
// In crates/xaft-tui/src/state.rs:

impl AppState {
    /// Register a new slash command at runtime.
    ///
    /// Updates the shared `CommandCatalog` and refreshes the trigger palette
    /// if the `/` trigger is currently active.
    pub fn register_command(&mut self, entry: CommandEntry) {
        if let Ok(mut catalog) = self.command_catalog.write() {
            catalog.register_dynamic(entry);
        }
        // If the slash trigger is open, refresh the displayed items.
        if self.active_trigger_char() == Some('/') {
            self.refresh_trigger();
        }
    }

    /// Returns the trigger character of the currently active trigger, if any.
    fn active_trigger_char(&self) -> Option<char> {
        self.active_trigger.as_ref().map(|t| t.trigger_char)
    }
}
```

### C.4 Handling `TuiEvent::CommandRegistered` in `AppState::handle_event()`

```rust
TuiEvent::CommandRegistered { name, source, description, args_hint } => {
    let group = if source.starts_with("skill:") {
        CommandGroup::Skills
    } else if source.starts_with("mcp:") {
        CommandGroup::Dynamic
    } else {
        CommandGroup::Dynamic
    };
    let cmd_source = if let Some(skill_name) = source.strip_prefix("skill:") {
        CommandSource::Skill { skill_name: skill_name.to_string() }
    } else if let Some(server) = source.strip_prefix("mcp:") {
        CommandSource::Mcp { server: server.to_string() }
    } else {
        CommandSource::Dynamic { registered_at: std::time::Instant::now() }
    };
    let entry = CommandEntry {
        name,
        aliases: Vec::new(),
        description,
        args_hint,
        group,
        source: cmd_source,
    };
    self.register_command(entry);
}
```

### C.5 Skill Registration at Session Startup

In `crates/xaft-tui/src/app.rs`, after skills are loaded:

```rust
// After building the TriggerRegistry:
for (slug, skill) in &skills {
    let entry = CommandEntry {
        name: slug.clone(),
        aliases: Vec::new(),
        description: skill.description().unwrap_or(slug).to_string(),
        args_hint: skill.args_hint().map(|s| s.to_string()),
        group: CommandGroup::Skills,
        source: CommandSource::Skill { skill_name: slug.clone() },
    };
    if let Ok(mut catalog) = command_catalog.write() {
        catalog.register_dynamic(entry);
    }
}
```

---

## `AppState` Changes

The following fields are added to `AppState` in `crates/xaft-tui/src/state.rs`.  The existing
`slash_palette` and `slash_history` fields are **retained** during the migration period (the
old palette system is not removed until these features pass acceptance testing):

```rust
// New fields added to AppState:

/// Shared command catalog — single source of truth for all slash commands.
/// Replaces the static COMMAND_TABLE for palette/autocomplete purposes.
pub command_catalog: Arc<RwLock<CommandCatalog>>,

/// Shared input history store — all submissions (tasks + slash commands).
pub input_history: Arc<RwLock<InputHistoryStore>>,

/// Active trigger state (from PRD-59 TriggerRegistry).
/// `None` when no trigger is active.
pub active_trigger: Option<ActiveTrigger>,

/// Registered trigger handlers.
pub trigger_registry: TriggerRegistry,
```

In `AppState::new()`, after `slash_registry` is built:

```rust
let command_catalog = Arc::new(RwLock::new(CommandCatalog::from_command_table()));
let input_history = Arc::new(RwLock::new(InputHistoryStore::new(50)));

let slash_trigger = SlashCommandTriggerHandler::new(Arc::clone(&command_catalog));
let history_trigger = HistoryTriggerHandler::new(Arc::clone(&input_history));

let mut trigger_registry = TriggerRegistry::new();
trigger_registry.register(slash_trigger);
trigger_registry.register(history_trigger);
```

### `handle_submit()` update

When the user submits (Enter key with non-empty buffer):

```rust
fn handle_submit(&mut self, text: String) {
    let kind = if text.starts_with('/') {
        HistoryKind::SlashCommand
    } else {
        HistoryKind::AgentTask
    };
    if let Ok(mut hist) = self.input_history.write() {
        hist.push(text.clone(), kind);
    }
    // ... existing submit logic (slash parse vs. agent task dispatch) ...
}
```

---

## Files to Change

| File | Change |
|------|--------|
| `crates/xaft-tui/src/trigger/slash_command.rs` | **New** — `CommandEntry`, `CommandGroup`, `CommandSource`, `CommandCatalog`, `SlashCommandTriggerHandler` |
| `crates/xaft-tui/src/trigger/history.rs` | **New** — `HistoryEntry`, `HistoryKind`, `InputHistoryStore`, `HistoryTriggerHandler`, `time_ago` |
| `crates/xaft-tui/src/trigger/mod.rs` | Add `pub mod slash_command; pub mod history;` |
| `crates/xaft-tui/src/slash/palette.rs` | Keep `SlashHistory`; mark `SlashPalette` as `#[deprecated]` once trigger system is active |
| `crates/xaft-tui/src/slash/mod.rs` | Keep `CommandResult`, `ConfigSection`, `AgentStats`, etc.; no removals in this PRD |
| `crates/xaft-tui/src/state.rs` | Add `command_catalog`, `input_history`, `active_trigger`, `trigger_registry`; update `new()` and `handle_submit()` |
| `crates/xaft-tui/src/bridge.rs` | Add `TuiEvent::CommandRegistered`; wire `XaftCommandRegistered` in `attach_xaft_signals()` |
| `crates/xaft-agent/src/signals.rs` | Add `XaftCommandRegistered` |
| `crates/xaft-tui/src/app.rs` | Register skills into `CommandCatalog` at startup; wire Up-arrow shortcut in `handle_key()` |
| `crates/xaft-tui/tests/slash_trigger_tests.rs` | **New** — integration tests for Feature A |
| `crates/xaft-tui/tests/history_trigger_tests.rs` | **New** — integration tests for Feature B |

---

## Acceptance Criteria

### Feature A — Enhanced Slash Command Trigger

| # | Criterion |
|---|-----------|
| AC-A1 | Typing `/` in the input bar opens a grouped palette that reads from `CommandCatalog`, not `COMMAND_TABLE`. |
| AC-A2 | The palette shows groups in order: Recent, Navigation, Agent, Git, Tools, Skills, Dynamic, Other. Groups with zero matches are omitted. |
| AC-A3 | The "Recent" pseudo-group appears at the top when the prefix is empty and there are recently-used commands. |
| AC-A4 | After executing `/compact`, the next empty-prefix palette search shows `/compact` in the Recent section. |
| AC-A5 | Typing `/co` shows `/compact`, `/config`, `/commit`, `/cost` (all start with "co"); groups with no match are absent. |
| AC-A6 | Each row renders in three columns: `▶/  ` indicator (2), `/name` (14, bold if selected), description (variable), `[args_hint]` (right-aligned, dim). |
| AC-A7 | Group header rows are non-selectable: pressing Down skips over them. |
| AC-A8 | Pressing Tab on a selected command inserts `"/name "` (with trailing space) into the input bar. |
| AC-A9 | Pressing Esc restores `"/partial"` in the input bar (the typed prefix before the palette opened). |
| AC-A10 | A skill registered via `XaftCommandRegistered` with `source = "skill:deploy"` appears in the Skills group immediately, without restart. |
| AC-A11 | A dynamic tool registered via `XaftCommandRegistered` with `source = "dynamic"` appears in the Dynamic group and is visible in the open palette within one render tick. |
| AC-A12 | `CommandCatalog::from_command_table()` produces an entry for every canonical command in `COMMAND_TABLE` (i.e., 23 canonical entries: help, clear, compact, config, cost, init, agents, mcp, resume, rewind, permissions, model, vim, emacs, theme, login, logout, doctor, memory, diff, commit, pr, quit). |
| AC-A13 | `CommandCatalog::register_dynamic()` with a duplicate canonical name replaces the old entry (last-writer-wins). |
| AC-A14 | `CommandCatalog::search("")` returns all entries sorted by group order then alphabetically. |

### Feature B — History Recall Trigger

| # | Criterion |
|---|-----------|
| AC-B1 | Typing `#` in the input bar opens the history dropdown showing the last 20 submissions. |
| AC-B2 | Both agent-task submissions and slash-command submissions appear in the history. |
| AC-B3 | History entries are displayed newest-first. |
| AC-B4 | The time-ago label updates correctly: `"5s ago"`, `"2m ago"`, `"3h ago"`, `"1d ago"`. |
| AC-B5 | The kind label (`task` or `cmd`) is shown right-aligned next to the time-ago label. |
| AC-B6 | Typing `#fix` filters to entries whose text contains "fix" (case-insensitive). |
| AC-B7 | Selecting a history entry with Enter replaces the current buffer with the full text of the entry. |
| AC-B8 | Pressing Up arrow while the input bar is empty (and no trigger is active) opens the history dropdown with an empty prefix, without inserting `#`. |
| AC-B9 | Pressing Esc from the history dropdown closes it and restores the buffer to empty (or the `#prefix` that was typed). |
| AC-B10 | `InputHistoryStore` with `max_entries = 3` drops the oldest entry when a fourth is pushed. |
| AC-B11 | Adjacent identical submissions ARE recorded (unlike `SlashHistory` which deduplicates them) — the user may want to resubmit the same task from different points in time. |
| AC-B12 | Pressing Up arrow while the input bar is non-empty does NOT open the history dropdown; it moves the cursor within the input bar (existing multi-line navigation behaviour is unchanged). |

---

## Named Test Stubs

All tests live in `crates/xaft-tui/tests/slash_trigger_tests.rs` (Feature A) and
`crates/xaft-tui/tests/history_trigger_tests.rs` (Feature B), with unit tests co-located in the
source files following xaft conventions.

### Feature A: `SlashCommandTriggerHandler` / `CommandCatalog`

```rust
// --- crates/xaft-tui/tests/slash_trigger_tests.rs ---

/// A fresh catalog built from COMMAND_TABLE has 23 canonical entries.
#[test]
fn catalog_from_command_table_has_all_canonical_entries() { todo!() }

/// search("") returns entries in group order then alphabetically within group.
#[test]
fn catalog_search_empty_prefix_is_sorted_by_group_then_alpha() { todo!() }

/// search("co") returns compact, config, commit, cost — all starting with "co".
#[test]
fn catalog_search_prefix_filters_by_prefix() { todo!() }

/// Grouped search with empty prefix emits Recent pseudo-group first when
/// recently_used is non-empty.
#[test]
fn catalog_grouped_search_recent_appears_first_when_non_empty() { todo!() }

/// After record_used("compact") twice and record_used("cost") once,
/// recently_used order is ["cost", "compact"].
#[test]
fn catalog_record_used_maintains_newest_first_order() { todo!() }

/// register_dynamic() with duplicate name replaces old entry.
#[test]
fn catalog_register_dynamic_last_writer_wins() { todo!() }

/// SlashCommandTriggerHandler::matches() with empty prefix returns MatchItems
/// with is_header=true for group headers and is_header=false for commands.
#[test]
fn trigger_matches_empty_prefix_includes_group_headers() { todo!() }

/// SlashCommandTriggerHandler::matches() with prefix "gi" returns only entries
/// in the Git group (diff, commit, pr).
#[test]
fn trigger_matches_prefix_gi_returns_git_group_entries() { todo!() }

/// on_select() returns "/compact " (with trailing space).
#[test]
fn trigger_on_select_adds_trailing_space() { todo!() }

/// on_confirm() records the command in recently_used.
#[test]
fn trigger_on_confirm_records_recently_used() { todo!() }

/// A dynamic entry registered via register_dynamic() appears in
/// grouped_search("") results under the Dynamic group.
#[test]
fn catalog_dynamic_entry_appears_in_grouped_search() { todo!() }

/// Skills group entries sourced from CommandSource::Skill sort before
/// Dynamic group entries in the palette.
#[test]
fn catalog_skills_sort_before_dynamic_in_grouped_search() { todo!() }
```

### Feature B: `HistoryTriggerHandler` / `InputHistoryStore`

```rust
// --- crates/xaft-tui/tests/history_trigger_tests.rs ---

/// Pushing an AgentTask and a SlashCommand — both are recorded.
#[test]
fn history_store_records_both_kinds() { todo!() }

/// search("") returns all entries newest-first.
#[test]
fn history_store_search_empty_returns_all_newest_first() { todo!() }

/// search("auth") returns only entries whose text contains "auth".
#[test]
fn history_store_search_filters_by_substring() { todo!() }

/// max_entries enforced: oldest entry dropped when capacity is exceeded.
#[test]
fn history_store_max_entries_drops_oldest() { todo!() }

/// Adjacent identical pushes ARE recorded (no adjacent-dedup).
#[test]
fn history_store_does_not_dedup_adjacent_identical_entries() { todo!() }

/// HistoryTriggerHandler::matches() returns MatchItems with full text in insert.
#[test]
fn history_trigger_matches_insert_is_full_text() { todo!() }

/// HistoryTriggerHandler::matches() truncates display to 60 chars + ellipsis.
#[test]
fn history_trigger_matches_display_truncates_long_text() { todo!() }

/// on_select() returns the full text of the item (not the truncated display).
#[test]
fn history_trigger_on_select_returns_full_text() { todo!() }

/// time_ago formats seconds, minutes, hours, days correctly.
#[test]
fn time_ago_formats_all_ranges() { todo!() }

/// HistoryKind::label() returns "task" and "cmd".
#[test]
fn history_kind_label_is_correct() { todo!() }

/// trigger_char() returns '#'.
#[test]
fn history_trigger_char_is_hash() { todo!() }

/// allows_free_text() returns false.
#[test]
fn history_trigger_does_not_allow_free_text() { todo!() }
```

---

## Architecture Notes

### Thread Safety

Both `CommandCatalog` and `InputHistoryStore` are accessed from:

1. The TUI main event loop (single-threaded, holds `&mut AppState`).
2. The signal bridge background tasks (write from a tokio task via `EventBridge`).

For case 2, writes happen through `AppState::handle_event()` which is always called from the main
event loop.  Signal bridge tasks only write to `mpsc::UnboundedSender<TuiEvent>`; the actual
catalog/history mutation happens in `handle_event()`.  Therefore the `RwLock` is technically
never contended, but it is still required because `Arc<RwLock<…>>` is what allows the
`SlashCommandTriggerHandler` and `HistoryTriggerHandler` to hold shared references without
lifetime parameters.

The `RwLock` approach is intentional rather than `Mutex` because:
- Many palette render frames call `catalog.read()` concurrently.
- Writes (new dynamic registrations) are rare.

### Deprecation of `SlashPalette`

`SlashPalette` is not removed in this PRD.  The migration path is:

1. Implement `SlashCommandTriggerHandler` and wire it into `AppState`.
2. Gate the new behaviour behind a `CommandCatalog` being `Some` in `AppState`.
3. In the render layer, prefer the `active_trigger` overlay over `slash_palette` when the former
   is non-`None`.
4. Once acceptance tests pass, annotate `SlashPalette` with `#[deprecated]` and remove it in a
   follow-up cleanup PRD.

`SlashHistory` is similarly retained — it underpins the cursor-style Up/Down navigation that
exists independently of the `#` trigger dropdown.

### `CommandGroup::from_canonical` Completeness

Every canonical name in `COMMAND_TABLE` must be handled by `CommandGroup::from_canonical`.  The
`Other` arm is the catch-all and is correct for: `config`, `emacs`, `help`, `login`, `logout`,
`memory`, `theme`, `vim`.  Any future command that is not placed in a specific group will land in
`Other`, which is always visible in the palette.

### Avoiding Static Lifetime Coupling

`CommandEntry` uses `String` (not `&'static str`) throughout.  This is intentional: dynamic and
skill-sourced commands are allocated at runtime and cannot carry `'static` lifetimes.  The
slight memory overhead (String allocation per entry) is negligible for a catalog of 30–50 entries.
`COMMAND_TABLE`'s `&'static str` fields are converted to `String` at catalog build time.

### Interaction with PRD-59 `MatchItem`

The `MatchItem` type from PRD-59 is assumed to have these fields:

```rust
pub struct MatchItem {
    /// Short label shown in the palette row.
    pub display: String,
    /// Text inserted into the buffer on selection (may be longer than display).
    pub insert: String,
    /// Optional right-column hint (args_hint or time-ago).
    pub hint: Option<String>,
    /// True for non-selectable group-header rows.
    pub is_header: bool,
    /// Category for renderer styling.
    pub kind: MatchKind,
    /// Arbitrary metadata for custom handlers.
    pub metadata: Option<serde_json::Value>,
}

pub enum MatchKind {
    Command,
    History,
    Mention,
    GroupHeader,
}
```

If PRD-59 defines `MatchItem` differently, the `entry_to_match_item` helper and
`HistoryTriggerHandler::matches()` will need to adapt to the actual fields.  The semantics
described above (display vs. insert, is_header) must be preserved.

---

## Open Questions

| # | Question | Proposed Answer |
|---|----------|-----------------|
| OQ-1 | Should `CommandCatalog::grouped_search` cap each group at `max_per_group` by default, or leave capping to the renderer? | Leave capping to `SlashCommandTriggerHandler` — the catalog returns full results; the handler applies `max_per_group`. This makes the catalog independently testable. |
| OQ-2 | Should `InputHistoryStore` dedup adjacent identical entries? | No. Unlike `SlashHistory` (which is for cursor-nav and benefits from no-adjacent-dup), the history dropdown is for recall — users may want to see how often they repeated a task. |
| OQ-3 | Should `#` trigger also match slash commands (blending with the slash trigger)? | No. The `#` trigger shows ALL prior submissions including slash commands, but does not replace the `/` trigger. The two triggers are complementary. |
| OQ-4 | Where does `CommandCatalog` live — in `xaft-tui` or as a standalone crate? | In `xaft-tui` for now. If `xaft-cli` needs it (e.g. for shell completion), extract to `xaft-slash` in a future PRD. |
| OQ-5 | Should the history trigger support arrow-key navigation after `#` is typed, or only after the dropdown opens? | After the dropdown opens. Arrow keys before typing `#` use the existing `SlashHistory` cursor-nav path. |

---

## Verification

```bash
# Build
cargo build -p xaft-tui

# Run all TUI tests
cargo test -p xaft-tui

# Run feature-specific integration tests
cargo test -p xaft-tui --test slash_trigger_tests
cargo test -p xaft-tui --test history_trigger_tests

# Manual smoke test
RUST_LOG=xaft=debug cargo run -- run "hello"
# In the TUI:
# Type /              → grouped palette appears, "Recent" header absent on first open
# Type /compact Enter → palette closes; next / open shows "Recent: compact"
# Type /co            → shows compact, config, commit, cost (filtered)
# Type /co↓↓ Tab      → second match inserted with trailing space
# Type /unknownxyz Enter → error shown in transcript
# Submit "fix the bug in auth.rs" (agent task)
# Press Up (empty buffer) → history dropdown opens with that entry
# Type #fix           → history filters to entries containing "fix"
# Select entry Enter  → buffer restored to full task text
```
