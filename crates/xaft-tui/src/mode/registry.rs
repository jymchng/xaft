//! `ModeRegistry` — ordered list of `AgentMode` with fast name lookup.

use std::collections::HashMap;

use super::AgentMode;
use super::builtins::builtin_modes;

/// An ordered registry of `AgentMode` values with O(1) name lookup.
pub struct ModeRegistry {
    modes: Vec<AgentMode>,
    index: HashMap<String, usize>,
}

impl ModeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            modes: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Register a mode.
    ///
    /// If a mode with the same name already exists it is replaced in-place
    /// (preserving insertion order). Otherwise the mode is appended.
    ///
    /// A mode named after a [`RESERVED_MODE_NAMES`] entry is refused (logged
    /// and dropped), as agenthicc drops a mode plugin named `debug` with
    /// "Debug is a rejected mode name". Returns whether the mode was
    /// registered.
    pub fn register(&mut self, mode: AgentMode) -> bool {
        if is_reserved_mode_name(&mode.name) {
            tracing::warn!(mode = %mode.name, "ignoring mode: {} is a rejected mode name", mode.name);
            return false;
        }
        if let Some(&idx) = self.index.get(&mode.name) {
            self.modes[idx] = mode;
        } else {
            let idx = self.modes.len();
            self.index.insert(mode.name.clone(), idx);
            self.modes.push(mode);
        }
        true
    }

    /// Remove all modes whose `source_id` matches `source_id` and rebuild
    /// the index.
    pub fn unregister_source(&mut self, source_id: &str) {
        self.modes.retain(|m| m.source_id != source_id);
        self.rebuild_index();
    }

    /// Look up a mode by name.
    pub fn get(&self, name: &str) -> Option<&AgentMode> {
        self.index.get(name).map(|&idx| &self.modes[idx])
    }

    /// Return the mode that follows `name` in registration order, wrapping
    /// to the first mode when `name` is the last entry.
    ///
    /// If the registry is empty or `name` is not found, returns the first mode
    /// (panics if the registry is truly empty — callers must ensure it is not).
    pub fn next_after(&self, name: &str) -> &AgentMode {
        assert!(!self.modes.is_empty(), "ModeRegistry is empty");
        match self.index.get(name) {
            Some(&idx) => {
                let next = (idx + 1) % self.modes.len();
                &self.modes[next]
            }
            None => &self.modes[0],
        }
    }

    /// All registered modes in insertion order, internal ones included.
    pub fn all_modes(&self) -> &[AgentMode] {
        &self.modes
    }

    /// The user-selectable modes (agenthicc's `registry.all()`): internal
    /// modes such as `replay` are excluded.
    pub fn selectable_modes(&self) -> impl Iterator<Item = &AgentMode> {
        self.modes.iter().filter(|m| m.selectable)
    }

    /// Number of registered modes.
    pub fn len(&self) -> usize {
        self.modes.len()
    }

    /// `true` when the registry contains no modes.
    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    // ── Private helpers ────────────────────────────────────────────────────────

    fn rebuild_index(&mut self) {
        self.index.clear();
        for (idx, mode) in self.modes.iter().enumerate() {
            self.index.insert(mode.name.clone(), idx);
        }
    }
}

impl Default for ModeRegistry {
    /// Pre-populated with all 6 built-in modes.
    fn default() -> Self {
        let mut reg = Self::new();
        for mode in builtin_modes() {
            reg.register(mode);
        }
        reg
    }
}

/// Mode names no source may register. agenthicc rejects `debug` outright —
/// there is no Debug identity and the name is reserved.
pub const RESERVED_MODE_NAMES: &[&str] = &["debug"];

/// Whether `name` is reserved (case-insensitive, as agenthicc normalises).
pub fn is_reserved_mode_name(name: &str) -> bool {
    let name = name.trim();
    RESERVED_MODE_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::AgentModeBuilder;

    fn make_mode(name: &str, source: &str) -> AgentMode {
        AgentModeBuilder::new(name, name.to_uppercase())
            .source_id(source)
            .build()
    }

    #[test]
    fn register_and_get() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("alpha", "src"));
        assert!(reg.get("alpha").is_some());
        assert!(reg.get("beta").is_none());
    }

    #[test]
    fn register_replaces_existing() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("alpha", "src1"));
        reg.register(make_mode("alpha", "src2")); // replace
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("alpha").unwrap().source_id, "src2");
    }

    #[test]
    fn next_after_wraps() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("a", "s"));
        reg.register(make_mode("b", "s"));
        reg.register(make_mode("c", "s"));
        assert_eq!(reg.next_after("c").name, "a");
        assert_eq!(reg.next_after("a").name, "b");
    }

    #[test]
    fn unregister_source_removes_and_rebuilds_index() {
        let mut reg = ModeRegistry::new();
        reg.register(make_mode("a", "builtin"));
        reg.register(make_mode("b", "mcp"));
        reg.register(make_mode("c", "builtin"));
        reg.unregister_source("mcp");
        assert_eq!(reg.len(), 2);
        assert!(reg.get("b").is_none());
        assert!(reg.get("a").is_some());
        assert!(reg.get("c").is_some());
    }

    #[test]
    fn default_has_the_builtins_and_no_debug() {
        let reg = ModeRegistry::default();
        assert_eq!(reg.len(), 6);
        assert!(reg.get("auto").is_some());
        assert!(reg.get("plan").is_some());
        assert!(reg.get("ask").is_some());
        assert!(reg.get("review").is_some());
        assert!(reg.get("safe").is_some());
        assert!(reg.get("replay").is_some());
        assert!(reg.get("debug").is_none());
        let selectable: Vec<&str> = reg.selectable_modes().map(|m| m.name.as_str()).collect();
        assert!(!selectable.contains(&"replay"));
    }

    #[test]
    fn a_debug_mode_is_refused_whatever_its_source() {
        let mut reg = ModeRegistry::new();
        assert!(!reg.register(make_mode("debug", "plugin")));
        assert!(!reg.register(make_mode("DEBUG", "mcp")));
        assert!(reg.is_empty());
        assert!(
            reg.register(make_mode("debugger", "plugin")),
            "only the exact name"
        );
    }
}
