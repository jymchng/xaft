//! `CommandCatalog` — live registry of all slash commands known to the TUI.
//!
//! Replaces the static `COMMAND_TABLE` as the single source of truth for slash
//! command discovery in the autocomplete palette. Built once from
//! `COMMAND_TABLE` at session startup, then extended at runtime as skills and
//! dynamic tools register new commands via `XaftCommandRegistered` signals.
//!
//! # Thread safety
//!
//! `CommandCatalog` is intended to be wrapped in `Arc<RwLock<CommandCatalog>>`
//! so it can be shared between the TUI main loop and
//! `SlashCommandTriggerHandler`. Reads (search / grouped_search) acquire a
//! read lock; writes (register_dynamic / record_used) acquire a write lock.
//!
//! # Example
//!
//! ```rust
//! use xaft_tui::trigger::catalog::{CommandCatalog, CommandGroup};
//!
//! let catalog = CommandCatalog::from_command_table();
//! assert!(!catalog.is_empty());
//!
//! // Prefix search
//! let hits = catalog.search("co");
//! assert!(hits.iter().any(|e| e.name == "compact"));
//!
//! // Grouped search for the palette
//! let groups = catalog.grouped_search("");
//! assert!(groups.iter().any(|(label, _)| *label == "Navigation"));
//! ```

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

// ── CommandGroup ──────────────────────────────────────────────────────────────

/// Display group for a slash command in the autocomplete palette.
///
/// Groups appear in the order defined by [`CommandGroup::sort_order`]:
/// Navigation → Agent → Git → Tools → Skills → Dynamic → Other.
///
/// The "Recent" pseudo-group is not a variant; it is computed at search time
/// by [`CommandCatalog::grouped_search`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandGroup {
    /// Navigation commands: `/clear`, `/compact`, `/resume`, `/rewind`, `/quit`.
    Navigation,
    /// Agent-interaction commands: `/agents`, `/model`, `/cost`.
    Agent,
    /// Git commands: `/diff`, `/commit`, `/pr`.
    Git,
    /// Tool-management commands: `/mcp`, `/permissions`, `/doctor`, `/init`.
    Tools,
    /// Commands contributed by loaded skills at session startup.
    Skills,
    /// Commands registered at runtime via `tool_factory` or an MCP server.
    Dynamic,
    /// Everything else: `/help`, `/config`, `/theme`, `/vim`, `/emacs`,
    /// `/login`, `/logout`, `/memory`.
    Other,
}

impl CommandGroup {
    /// Display label shown as a group header in the palette.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Agent => "Agent",
            Self::Git => "Git",
            Self::Tools => "Tools",
            Self::Skills => "Skills",
            Self::Dynamic => "Dynamic",
            Self::Other => "Other",
        }
    }

    /// Palette display priority (lower = appears first, after the Recent pseudo-group).
    pub fn sort_order(&self) -> u8 {
        match self {
            Self::Navigation => 0,
            Self::Agent => 1,
            Self::Git => 2,
            Self::Tools => 3,
            Self::Skills => 4,
            Self::Dynamic => 5,
            Self::Other => 6,
        }
    }

    /// Classify a built-in canonical command name into its display group.
    ///
    /// The `Other` arm is the catch-all for commands that do not fit a
    /// specific group. Any future command not listed here lands in `Other`.
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

// ── CommandSource ─────────────────────────────────────────────────────────────

/// Provenance of a `CommandEntry` — where the command was registered from.
///
/// Used by `/help` output and audit logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    /// Hard-coded in `COMMAND_TABLE` / compiled into the binary.
    BuiltIn,
    /// Contributed by a loaded skill at session startup.
    Skill {
        /// The slug / identifier of the skill.
        skill_name: String,
    },
    /// Registered at runtime (via `tool_factory` or a direct API call).
    Dynamic {
        /// Wall-clock instant at which the command was registered.
        registered_at: Instant,
    },
    /// Contributed by an MCP server.
    Mcp {
        /// The MCP server name.
        server: String,
    },
}

// ── CommandEntry ──────────────────────────────────────────────────────────────

/// Owned descriptor for a single slash command.
///
/// Unlike `CommandMeta` (which uses `&'static str` slices and is tied to
/// compile-time data), `CommandEntry` uses owned `String`s so dynamic and
/// skill-sourced commands can be represented equally alongside built-ins.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    /// Canonical name without leading slash, e.g. `"compact"`.
    pub name: String,
    /// Additional trigger names that resolve to this command, e.g. `["ctx"]`.
    pub aliases: Vec<String>,
    /// One-line description shown in the palette and `/help` output.
    pub description: String,
    /// Optional argument syntax hint shown right-aligned in the palette,
    /// e.g. `"[session-id]"` or `"<name>"`.
    pub args_hint: Option<String>,
    /// Display group for the palette.
    pub group: CommandGroup,
    /// Where this command was registered from.
    pub source: CommandSource,
}

// ── CommandCatalog ────────────────────────────────────────────────────────────

/// Live registry of all slash commands known to the TUI.
///
/// Built from `COMMAND_TABLE` at session startup, then extended dynamically
/// as skills and dynamic tools register new commands. Wrap in
/// `Arc<RwLock<CommandCatalog>>` everywhere it is shared.
///
/// # Deduplication semantics
///
/// - Initial construction via [`from_command_table`](CommandCatalog::from_command_table)
///   skips alias rows (rows where `meta.canonical != trigger`). Each canonical
///   command appears exactly once; its aliases are stored in the entry's
///   `aliases` field.
/// - [`register_dynamic`](CommandCatalog::register_dynamic) is last-writer-wins:
///   if a command with the same canonical name already exists, the old entry is
///   replaced and all alias mappings are updated.
pub struct CommandCatalog {
    /// All entries in insertion order (built-ins first, then skills, then dynamic).
    entries: Vec<CommandEntry>,
    /// Maps canonical name and every alias → index into `entries`.
    by_name: HashMap<String, usize>,
    /// Ring buffer of recently-used canonical command names (newest first).
    recently_used: VecDeque<String>,
    /// Maximum number of recently-used entries to keep.
    max_recently_used: usize,
}

impl Default for CommandCatalog {
    fn default() -> Self {
        Self::new()
    }
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
    ///
    /// This is the primary constructor for production use; tests may call
    /// [`new`](CommandCatalog::new) and [`register_dynamic`](CommandCatalog::register_dynamic)
    /// to build a custom catalog.
    pub fn from_command_table() -> Self {
        use crate::slash::parser::COMMAND_TABLE;
        let mut catalog = Self::new();
        for (trigger, meta) in COMMAND_TABLE {
            // Skip alias rows — each command must appear once under its canonical name.
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

    /// Register a new dynamic entry.
    ///
    /// If a command with the same canonical name already exists it is replaced
    /// (last-writer-wins — skills may override built-in descriptions). All alias
    /// mappings are updated atomically.
    pub fn register_dynamic(&mut self, entry: CommandEntry) {
        let name = entry.name.clone();
        let aliases = entry.aliases.clone();

        if let Some(&old_idx) = self.by_name.get(&name) {
            // Remove old alias mappings before replacing.
            let old_aliases: Vec<String> = self.entries[old_idx].aliases.clone();
            for alias in &old_aliases {
                self.by_name.remove(alias);
            }
            // Replace in-place to keep insertion order stable.
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
    /// - When `partial` is empty: returns all entries sorted by group order,
    ///   then alphabetically within each group.
    /// - When `partial` is non-empty: returns matches sorted alphabetically.
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
                a.group
                    .sort_order()
                    .cmp(&b.group.sort_order())
                    .then_with(|| a.name.cmp(&b.name))
            } else {
                a.name.cmp(&b.name)
            }
        });
        result
    }

    /// Grouped search: return `(group_label, entries)` pairs in palette display order.
    ///
    /// Groups with no matching entries are omitted. The "Recent" pseudo-group is
    /// prepended when `partial` is empty and there are recently-used entries.
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

        // Collect regular groups in sort_order.
        let all_groups = [
            CommandGroup::Navigation,
            CommandGroup::Agent,
            CommandGroup::Git,
            CommandGroup::Tools,
            CommandGroup::Skills,
            CommandGroup::Dynamic,
            CommandGroup::Other,
        ];
        let p = partial.to_ascii_lowercase();
        for group in &all_groups {
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

    /// Record a command as recently used.
    ///
    /// Deduplicates: if the name is already in the recently-used ring it is
    /// moved to the front (most-recent). The ring is capped at
    /// `max_recently_used` entries; the oldest entry is evicted when full.
    pub fn record_used(&mut self, name: &str) {
        self.recently_used.retain(|n| n.as_str() != name);
        self.recently_used.push_front(name.to_string());
        while self.recently_used.len() > self.max_recently_used {
            self.recently_used.pop_back();
        }
    }

    /// Look up an entry by canonical name or alias.
    ///
    /// Returns `None` when the name is not registered.
    pub fn get(&self, name: &str) -> Option<&CommandEntry> {
        self.by_name.get(name).and_then(|&i| self.entries.get(i))
    }

    /// Total number of canonical entries (aliases are not counted separately).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the catalog contains no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Add an entry without a dedup check (used during initial construction).
    fn add_entry(&mut self, entry: CommandEntry) {
        let idx = self.entries.len();
        self.by_name.insert(entry.name.clone(), idx);
        for alias in &entry.aliases {
            self.by_name.insert(alias.clone(), idx);
        }
        self.entries.push(entry);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dynamic_entry(name: &str, group: CommandGroup) -> CommandEntry {
        CommandEntry {
            name: name.to_string(),
            aliases: Vec::new(),
            description: format!("Dynamic command: {name}"),
            args_hint: None,
            group,
            source: CommandSource::Dynamic {
                registered_at: Instant::now(),
            },
        }
    }

    #[test]
    fn from_command_table_has_builtin_commands() {
        let catalog = CommandCatalog::from_command_table();
        assert!(!catalog.is_empty(), "catalog must not be empty");
        // Check canonical commands expected by PRD-60 AC-A12.
        for name in &[
            "help",
            "clear",
            "compact",
            "config",
            "cost",
            "init",
            "agents",
            "mcp",
            "resume",
            "rewind",
            "permissions",
            "model",
            "vim",
            "emacs",
            "theme",
            "login",
            "logout",
            "doctor",
            "memory",
            "diff",
            "commit",
            "pr",
            "quit",
        ] {
            assert!(
                catalog.get(name).is_some(),
                "catalog must contain '/{name}'"
            );
        }
    }

    #[test]
    fn search_empty_prefix_returns_all_sorted_by_group() {
        let catalog = CommandCatalog::from_command_table();
        let results = catalog.search("");
        assert!(!results.is_empty());

        // Verify ordering: Navigation before Git before Other (by sort_order).
        let positions: HashMap<&str, usize> = results
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.as_str(), i))
            .collect();

        // "clear" (Navigation, order=0) should appear before "diff" (Git, order=2).
        let clear_pos = positions["clear"];
        let diff_pos = positions["diff"];
        assert!(
            clear_pos < diff_pos,
            "Navigation must sort before Git: clear={clear_pos}, diff={diff_pos}"
        );
    }

    #[test]
    fn search_partial_returns_matches() {
        let catalog = CommandCatalog::from_command_table();
        let results = catalog.search("co");
        let names: Vec<&str> = results.iter().map(|e| e.name.as_str()).collect();
        // All starting with "co": compact, config, commit, cost
        for expected in &["compact", "config", "commit", "cost"] {
            assert!(
                names.contains(expected),
                "search('co') must contain '{expected}', got: {names:?}"
            );
        }
        // Must not contain unrelated commands.
        assert!(
            !names.contains(&"help"),
            "search('co') must not contain 'help'"
        );
    }

    #[test]
    fn search_alias_match() {
        let catalog = CommandCatalog::from_command_table();
        // "ctx" is an alias for "compact"; searching "ct" should find compact.
        let results = catalog.search("ct");
        let names: Vec<&str> = results.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"compact"),
            "search('ct') via alias 'ctx' must return 'compact', got: {names:?}"
        );
    }

    #[test]
    fn grouped_search_recent_section_prepended() {
        let mut catalog = CommandCatalog::from_command_table();
        catalog.record_used("compact");
        let groups = catalog.grouped_search("");
        assert_eq!(
            groups[0].0, "Recent",
            "first group must be 'Recent' when recently_used is non-empty"
        );
        let recent_names: Vec<&str> = groups[0].1.iter().map(|e| e.name.as_str()).collect();
        assert!(
            recent_names.contains(&"compact"),
            "Recent group must contain 'compact'"
        );
    }

    #[test]
    fn record_used_deduplicates() {
        let mut catalog = CommandCatalog::from_command_table();
        catalog.record_used("compact");
        catalog.record_used("cost");
        catalog.record_used("compact"); // move to front
        let groups = catalog.grouped_search("");
        let recent = &groups[0];
        assert_eq!(recent.0, "Recent");
        // compact must appear only once and must be first.
        let recent_names: Vec<&str> = recent.1.iter().map(|e| e.name.as_str()).collect();
        let compact_count = recent_names.iter().filter(|&&n| n == "compact").count();
        assert_eq!(
            compact_count, 1,
            "compact must appear exactly once in Recent"
        );
        assert_eq!(
            recent_names[0], "compact",
            "compact must be first (most recently used)"
        );
    }

    #[test]
    fn register_dynamic_adds_to_dynamic_group() {
        let mut catalog = CommandCatalog::new();
        let entry = make_dynamic_entry("deploy", CommandGroup::Dynamic);
        catalog.register_dynamic(entry);
        assert_eq!(catalog.len(), 1);
        let e = catalog.get("deploy").expect("deploy must be registered");
        assert_eq!(e.group, CommandGroup::Dynamic);
    }

    #[test]
    fn register_dynamic_replaces_existing() {
        let mut catalog = CommandCatalog::from_command_table();
        let original = catalog.get("compact").expect("compact must exist").clone();
        assert_eq!(original.source, CommandSource::BuiltIn);

        // Replace with a skill-sourced entry.
        let new_entry = CommandEntry {
            name: "compact".to_string(),
            aliases: Vec::new(),
            description: "Skill-overridden compact".to_string(),
            args_hint: None,
            group: CommandGroup::Skills,
            source: CommandSource::Skill {
                skill_name: "my-skill".to_string(),
            },
        };
        let count_before = catalog.len();
        catalog.register_dynamic(new_entry);
        // Count must not grow — it's a replacement.
        assert_eq!(
            catalog.len(),
            count_before,
            "register_dynamic must replace in-place, not add"
        );
        let replaced = catalog.get("compact").expect("compact must still exist");
        assert_eq!(replaced.description, "Skill-overridden compact");
        assert_eq!(replaced.group, CommandGroup::Skills);
    }

    #[test]
    fn from_canonical_maps_clear_to_navigation() {
        assert_eq!(
            CommandGroup::from_canonical("clear"),
            CommandGroup::Navigation
        );
        assert_eq!(
            CommandGroup::from_canonical("compact"),
            CommandGroup::Navigation
        );
        assert_eq!(
            CommandGroup::from_canonical("resume"),
            CommandGroup::Navigation
        );
        assert_eq!(
            CommandGroup::from_canonical("rewind"),
            CommandGroup::Navigation
        );
        assert_eq!(
            CommandGroup::from_canonical("quit"),
            CommandGroup::Navigation
        );
    }

    #[test]
    fn from_canonical_maps_diff_to_git() {
        assert_eq!(CommandGroup::from_canonical("diff"), CommandGroup::Git);
        assert_eq!(CommandGroup::from_canonical("commit"), CommandGroup::Git);
        assert_eq!(CommandGroup::from_canonical("pr"), CommandGroup::Git);
    }

    #[test]
    fn from_canonical_maps_agents_to_agent_group() {
        assert_eq!(CommandGroup::from_canonical("agents"), CommandGroup::Agent);
        assert_eq!(CommandGroup::from_canonical("model"), CommandGroup::Agent);
        assert_eq!(CommandGroup::from_canonical("cost"), CommandGroup::Agent);
    }

    #[test]
    fn from_canonical_maps_help_to_other() {
        assert_eq!(CommandGroup::from_canonical("help"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("config"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("theme"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("vim"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("emacs"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("login"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("logout"), CommandGroup::Other);
        assert_eq!(CommandGroup::from_canonical("memory"), CommandGroup::Other);
    }

    #[test]
    fn grouped_search_with_prefix_returns_only_matches() {
        let catalog = CommandCatalog::from_command_table();
        let groups = catalog.grouped_search("di");
        // "di" matches "diff" (Git group)
        let found = groups
            .iter()
            .any(|(label, entries)| *label == "Git" && entries.iter().any(|e| e.name == "diff"));
        assert!(found, "grouped_search('di') must find 'diff' in Git group");
        // Must not contain Navigation commands.
        let has_nav = groups.iter().any(|(label, _)| *label == "Navigation");
        assert!(!has_nav, "Navigation group must be absent for prefix 'di'");
    }

    #[test]
    fn len_and_is_empty() {
        let empty = CommandCatalog::new();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let catalog = CommandCatalog::from_command_table();
        assert!(!catalog.is_empty());
        assert!(
            catalog.len() >= 23,
            "expected at least 23 canonical entries"
        );
    }
}
