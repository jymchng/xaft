//! Memory configuration for xaft.

use serde::{Deserialize, Serialize};

/// Fields that exist on this runtime type but **not** on
/// `xaft_config::MemoryConfig`.
///
/// WHY THIS LIST EXISTS. `MemoryConfig` is declared twice — once as the
/// user-facing schema (`xaft_config::MemoryConfig`, held by
/// `XaftConfig.memory` and read from `xaft.toml`) and once here as the
/// runtime copy the memory system actually consumes. `src/main.rs` maps
/// between them field by field, so a field added to one side is **not**
/// automatically a setting a user can write: the mapping is the only thing
/// connecting them, and nothing forced the two to stay in step.
///
/// That is not hypothetical. `auto_remember_ttl_secs` was added here first and
/// `src/main.rs` hardcoded it to `None`, so it was unreachable AND unread —
/// a knob that looked implemented and did nothing. Every entry below must
/// therefore justify why it is legitimately runtime-only; an entry with no
/// real reason is the bug, not the exception.
///
/// KEEP THIS IN STEP with `xaft-config`'s struct. `config_fields_are_not_silently_unreachable`
/// in `tests/config_parity.rs` fails if a field appears on either side
/// without being accounted for here.
pub const RUNTIME_ONLY_FIELDS: &[(&str, &str)] = &[(
    "backend",
    "typed as `MemoryBackend` here and `String` on the config side, \
         because the TUI config menu (crates/xaft-tui/src/menu/config_menu.rs) \
         reads and writes `config.memory.backend` as a `ConfigValue::Str`. The \
         conversion happens once, in `src/main.rs`. The VALUE is covered: \
         `config_backend_round_trips_into_the_runtime_enum` asserts the \
         string->enum mapping.",
)];

/// Configuration for the xaft memory system.
///
/// The user-facing schema lives in `xaft_config::MemoryConfig`; this is the
/// runtime view of it. See [`RUNTIME_ONLY_FIELDS`] for the field list that
/// deliberately differs and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Whether the memory system is enabled.
    pub enabled: bool,

    /// Storage backend: `"sqlite"` or `"in_memory"`.
    pub backend: MemoryBackend,

    /// Auto-remember facts extracted from agent turns.
    pub auto_remember: bool,

    /// Auto-summarize old memories when the store grows large.
    pub auto_summarize: bool,

    /// Default to project-scoped memory (workspace scope).
    pub project_scope_default: bool,

    /// Maximum number of memories before auto-summarization triggers.
    pub max_entries: Option<usize>,

    /// TTL in seconds for auto-remembered entries. `None` = no expiry.
    ///
    /// READ by `XaftMemoryManager::remember_with_agent`, which stamps
    /// `MemoryEntry::with_expiry(...)` on each newly stored entry. Before
    /// that it was read by nothing at all, and `src/main.rs` hardcoded the
    /// runtime value to `None`, so no user could set it — see
    /// `xaft_config::MemoryConfig::auto_remember_ttl_secs`, which is now the
    /// key that feeds this field.
    pub auto_remember_ttl_secs: Option<u64>,

    /// Maximum search results returned by recall.
    pub max_search_results: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: MemoryBackend::Sqlite,
            auto_remember: true,
            auto_summarize: true,
            project_scope_default: true,
            max_entries: Some(10_000),
            auto_remember_ttl_secs: None,
            max_search_results: 10,
        }
    }
}

/// Storage backend selection.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBackend {
    /// SQLite-backed persistent storage.
    #[default]
    Sqlite,
    /// Ephemeral in-memory storage (for tests or ephemeral sessions).
    InMemory,
}
