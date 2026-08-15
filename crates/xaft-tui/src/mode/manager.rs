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
    /// A mode name is not part of the interactive cycle
    /// (agenthicc parity: only Safe → Plan → Yolo are cyclable).
    #[error("mode '{0}' is not part of the interactive cycle")]
    NotInCycle(String),
}

/// Interactive cycle names, mirroring agenthicc's Safe → Plan → Yolo cycle.
///
/// agenthicc (`src/agenthicc/tui/runtime/mode_manager.py`) selects exactly
/// `("Safe", "Plan", "Yolo")` and treats `auto` as an alias for Yolo, `ask`/
/// `guard` as aliases for Safe, and `review` as an alias for Plan. xaft keeps
/// its full six-mode registry for direct `/mode <name>` selection, but the
/// Shift+Tab cycle follows the same three-mode surface.
pub const CYCLE_NAMES: &[&str] = &["safe", "plan", "auto"];

/// Map an agenthicc-compatible alias onto a canonical xaft mode name.
/// Unknown aliases fall back to the name itself (direct selection still works).
pub fn canonical_mode_name(name: &str) -> &str {
    match name {
        "guard" => "safe",
        "ask" => "safe",
        "review" => "plan",
        "yolo" => "auto",
        other => other,
    }
}

/// Manages the active mode and applies it to `RunRequest` instances.
pub struct ModeManager {
    registry: ModeRegistry,
    active_idx: usize,
    /// Names (in cycle order) that Shift+Tab walks. Defaults to
    /// `CYCLE_NAMES` when `None`.
    cycle_names: Option<Vec<String>>,
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
            cycle_names: None,
        })
    }

    /// Create a manager pre-populated with all 6 built-in modes, starting at Auto.
    pub fn default_builtin() -> Self {
        let registry = ModeRegistry::default();
        Self {
            registry,
            active_idx: 0,
            cycle_names: None,
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

    /// Advance to the next *cycle* mode (wrapping).
    ///
    /// Only the modes named in [`CYCLE_NAMES`] participate; the active mode is
    /// first mapped onto the cycle (via [`canonical_mode_name`]) so that
    /// starting from `ask`/`review`/`debug` still advances predictably.
    /// Returns the new active mode.
    pub fn cycle(&mut self) -> &AgentMode {
        let cycle = self
            .cycle_names
            .clone()
            .unwrap_or_else(|| CYCLE_NAMES.iter().map(|s| s.to_string()).collect());
        let active = self.active_name();
        let canonical = canonical_mode_name(active);
        // Position of the canonical name in the cycle (default to last so a
        // non-cycle mode wraps to the first cycle mode).
        let pos = cycle
            .iter()
            .position(|n| n == canonical)
            .unwrap_or(cycle.len().saturating_sub(1));
        let next = &cycle[(pos + 1) % cycle.len()];
        // If a cycle name isn't in the registry (custom registry), fall back to
        // plain advancing so we never panic.
        let idx = self
            .registry
            .all_modes()
            .iter()
            .position(|m| m.name == *next);
        match idx {
            Some(i) => {
                self.active_idx = i;
                self.active()
            }
            None => {
                let total = self.registry.len();
                self.active_idx = (self.active_idx + 1) % total;
                self.active()
            }
        }
    }

    /// Whether `name` is part of the interactive Shift+Tab cycle.
    pub fn is_cyclable(name: &str) -> bool {
        let canonical = canonical_mode_name(name);
        CYCLE_NAMES.contains(&canonical)
    }

    /// Set the cycle names (for custom registries). `None` restores the default.
    pub fn set_cycle_names(&mut self, names: Option<Vec<String>>) {
        self.cycle_names = names;
    }

    /// Switch to the named mode. Returns `Err` when the name is unknown.
    pub fn set(&mut self, name: &str) -> Result<&AgentMode, ModeError> {
        let canonical = canonical_mode_name(name);
        let idx = self
            .registry
            .all_modes()
            .iter()
            .position(|m| m.name == canonical)
            .ok_or_else(|| ModeError::UnknownMode(name.to_string()))?;
        self.active_idx = idx;
        Ok(self.active())
    }

    /// Reject a mode that is not part of the interactive cycle, mirroring
    /// agenthicc's `/mode` guard (Debug is not an alias and is rejected).
    /// Direct selection of non-cycle modes is still allowed via [`set`].
    pub fn reject_if_not_cyclable(&self, name: &str) -> Result<(), ModeError> {
        if Self::is_cyclable(name) {
            Ok(())
        } else {
            Err(ModeError::NotInCycle(name.to_string()))
        }
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
    fn cycle_follows_safe_plan_yolo() {
        let mut mgr = ModeManager::default_builtin();
        // Start at auto (default). Cycle should go auto → safe → plan → auto.
        assert_eq!(mgr.active_name(), "auto");
        mgr.cycle();
        assert_eq!(mgr.active_name(), "safe");
        mgr.cycle();
        assert_eq!(mgr.active_name(), "plan");
        mgr.cycle();
        assert_eq!(mgr.active_name(), "auto");
    }

    #[test]
    fn cycle_from_alias_maps_canonical() {
        let mut mgr = ModeManager::default_builtin();
        // ask ≡ safe → next cycle should be plan.
        mgr.set("ask").unwrap();
        assert_eq!(mgr.active_name(), "safe");
        mgr.cycle();
        assert_eq!(mgr.active_name(), "plan");
    }

    #[test]
    fn set_accepts_alias() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set("yolo").unwrap();
        assert_eq!(mgr.active_name(), "auto");
        mgr.set("guard").unwrap();
        assert_eq!(mgr.active_name(), "safe");
        mgr.set("review").unwrap();
        assert_eq!(mgr.active_name(), "plan");
    }

    #[test]
    fn is_cyclable_classifies() {
        assert!(ModeManager::is_cyclable("safe"));
        assert!(ModeManager::is_cyclable("plan"));
        assert!(ModeManager::is_cyclable("auto"));
        assert!(ModeManager::is_cyclable("yolo")); // alias → auto
        assert!(ModeManager::is_cyclable("review")); // alias → plan
        assert!(ModeManager::is_cyclable("ask")); // alias → safe
        assert!(ModeManager::is_cyclable("guard")); // alias → safe
        assert!(!ModeManager::is_cyclable("debug")); // rejected (agenthicc parity)
        assert!(!ModeManager::is_cyclable("nonexistent"));
    }

    #[test]
    fn reject_if_not_cyclable() {
        let mgr = ModeManager::default_builtin();
        assert!(mgr.reject_if_not_cyclable("debug").is_err());
        assert!(mgr.reject_if_not_cyclable("safe").is_ok());
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
