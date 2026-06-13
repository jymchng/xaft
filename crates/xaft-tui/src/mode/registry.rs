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
    pub fn register(&mut self, mode: AgentMode) {
        if let Some(&idx) = self.index.get(&mode.name) {
            self.modes[idx] = mode;
        } else {
            let idx = self.modes.len();
            self.index.insert(mode.name.clone(), idx);
            self.modes.push(mode);
        }
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

    /// All registered modes in insertion order.
    pub fn all_modes(&self) -> &[AgentMode] {
        &self.modes
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
    fn default_has_six_builtins() {
        let reg = ModeRegistry::default();
        assert_eq!(reg.len(), 6);
        assert!(reg.get("auto").is_some());
        assert!(reg.get("plan").is_some());
        assert!(reg.get("ask").is_some());
        assert!(reg.get("review").is_some());
        assert!(reg.get("safe").is_some());
        assert!(reg.get("debug").is_some());
    }
}
