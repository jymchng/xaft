//! `ModeManager` — active mode selection and RunRequest patching.

use thiserror::Error;
use xaft_runtime::RunRequest;

use super::AgentMode;
use super::registry::ModeRegistry;

/// Errors from `ModeManager` operations.
#[derive(Debug, Error)]
pub enum ModeError {
    /// A mode name was looked up but not found in the registry.
    #[error("unknown mode: '{0}'")]
    UnknownMode(String),
    /// An operation requires at least one mode but the registry is empty.
    #[error("mode registry is empty")]
    EmptyRegistry,
}

/// Manages the active mode and applies it to `RunRequest` instances.
pub struct ModeManager {
    registry: ModeRegistry,
    active_idx: usize,
}

impl ModeManager {
    /// Create a manager from a pre-built registry.
    ///
    /// Returns `Err(ModeError::EmptyRegistry)` when the registry has no modes.
    pub fn new(registry: ModeRegistry) -> Result<Self, ModeError> {
        if registry.is_empty() {
            return Err(ModeError::EmptyRegistry);
        }
        Ok(Self {
            registry,
            active_idx: 0,
        })
    }

    /// Create a manager pre-populated with all 6 built-in modes, starting at Auto.
    pub fn default_builtin() -> Self {
        let registry = ModeRegistry::default();
        Self {
            registry,
            active_idx: 0,
        }
    }

    /// The currently active mode.
    pub fn active(&self) -> &AgentMode {
        &self.registry.all_modes()[self.active_idx]
    }

    /// Name of the currently active mode.
    pub fn active_name(&self) -> &str {
        &self.registry.all_modes()[self.active_idx].name
    }

    /// Advance to the next mode (wrapping). Returns the new active mode.
    pub fn cycle(&mut self) -> &AgentMode {
        let total = self.registry.len();
        self.active_idx = (self.active_idx + 1) % total;
        self.active()
    }

    /// Switch to the named mode. Returns `Err` when the name is unknown.
    pub fn set(&mut self, name: &str) -> Result<&AgentMode, ModeError> {
        let idx = self
            .registry
            .all_modes()
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| ModeError::UnknownMode(name.to_string()))?;
        self.active_idx = idx;
        Ok(self.active())
    }

    /// Apply the active mode to a `RunRequest`:
    ///
    /// - Sets `req.mode_system_patch` from `active().system_patch` (or `None`
    ///   when the patch is empty).
    /// - Sets `req.mode_tool_filter` from `active().tool_filter`.
    pub fn apply_to_run_request(&self, req: &mut RunRequest) {
        let mode = self.active();

        req.mode_system_patch = if mode.system_patch.is_empty() {
            None
        } else {
            Some(mode.system_patch.clone())
        };

        req.mode_tool_filter = mode.tool_filter.clone();
    }

    /// Immutable reference to the registry.
    pub fn registry(&self) -> &ModeRegistry {
        &self.registry
    }

    /// Register a new mode (or replace an existing one by name).
    pub fn register_mode(&mut self, mode: AgentMode) {
        self.registry.register(mode);
    }

    /// Remove all modes from a source. Resets `active_idx` to 0 if the active
    /// mode was removed.
    pub fn unregister_source(&mut self, source_id: &str) {
        let active_name = self.active_name().to_string();
        self.registry.unregister_source(source_id);

        // If the active mode was removed, fall back to index 0 (first mode).
        if self.registry.get(&active_name).is_none() {
            self.active_idx = 0;
        } else {
            // Re-sync the index since positions may have shifted.
            self.active_idx = self
                .registry
                .all_modes()
                .iter()
                .position(|m| m.name == active_name)
                .unwrap_or(0);
        }
    }
}

impl Default for ModeManager {
    fn default() -> Self {
        Self::default_builtin()
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::AgentModeBuilder;

    fn make_req() -> RunRequest {
        use std::path::PathBuf;
        RunRequest {
            task: "test".into(),
            config: xaft_config::XaftConfig::default(),
            working_dir: PathBuf::from("."),
            headless: true,
            dry_run: true,
            auto_approve: false,
            dangerously_skip_permissions: false,
            resume_session_id: None,
            workflow: xaft_runtime::WorkflowConfig::default(),
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
        }
    }

    #[test]
    fn default_is_auto() {
        let mgr = ModeManager::default_builtin();
        assert_eq!(mgr.active_name(), "auto");
    }

    #[test]
    fn cycle_advances() {
        let mut mgr = ModeManager::default_builtin();
        let name = mgr.cycle().name.clone();
        assert_ne!(name, "auto");
    }

    #[test]
    fn set_by_name() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set("plan").unwrap();
        assert_eq!(mgr.active_name(), "plan");
    }

    #[test]
    fn set_unknown_returns_err() {
        let mut mgr = ModeManager::default_builtin();
        let r = mgr.set("nonexistent");
        assert!(r.is_err());
    }

    #[test]
    fn apply_system_patch() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set("plan").unwrap();
        let mut req = make_req();
        mgr.apply_to_run_request(&mut req);
        assert!(req.mode_system_patch.is_some());
        assert!(req.mode_system_patch.as_ref().unwrap().contains("PLAN"));
    }

    #[test]
    fn apply_tool_filter() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set("plan").unwrap();
        let mut req = make_req();
        mgr.apply_to_run_request(&mut req);
        let filter = req.mode_tool_filter.as_ref().unwrap();
        assert!(filter("read_file"));
        assert!(!filter("write_file"));
    }

    #[test]
    fn auto_mode_no_patch() {
        let mgr = ModeManager::default_builtin();
        let mut req = make_req();
        mgr.apply_to_run_request(&mut req);
        assert!(req.mode_system_patch.is_none());
        assert!(req.mode_tool_filter.is_none());
    }

    #[test]
    fn new_empty_registry_returns_err() {
        let reg = ModeRegistry::new();
        assert!(ModeManager::new(reg).is_err());
    }
}
