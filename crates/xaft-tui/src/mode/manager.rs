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

    /// Create a manager pre-populated with every built-in mode, starting at Auto.
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
    /// starting from `ask`/`review` (or the internal `replay`) still advances
    /// predictably.
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
    ///
    /// Internal modes (`replay`) are refused as unknown, exactly as
    /// agenthicc's `set_by_name` refuses them: only trusted lifecycle code may
    /// enter one, through [`ModeManager::set_internal`].
    pub fn set(&mut self, name: &str) -> Result<&AgentMode, ModeError> {
        let canonical = canonical_mode_name(name);
        let idx = self
            .registry
            .all_modes()
            .iter()
            .position(|m| m.name == canonical && m.selectable)
            .ok_or_else(|| ModeError::UnknownMode(name.to_string()))?;
        self.active_idx = idx;
        Ok(self.active())
    }

    /// Enter a mode for a trusted lifecycle operation — internal modes
    /// included (agenthicc's `set_internal_by_name`). `/replay` uses this to
    /// run under `replay`.
    pub fn set_internal(&mut self, name: &str) -> Result<&AgentMode, ModeError> {
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

    /// Return to a mode captured before a lifecycle operation (agenthicc's
    /// `restore`), internal or not.
    pub fn restore(&mut self, name: &str) -> Result<&AgentMode, ModeError> {
        self.set_internal(name)
    }

    /// The names `/mode` accepts directly, in registration order: every
    /// selectable mode a user can reach by its own name. Internal modes and
    /// the two registered modes the alias table shadows (`ask`, `review`) are
    /// left out — agenthicc's `selectable_names()`.
    pub fn selectable_names(&self) -> Vec<String> {
        self.registry
            .selectable_modes()
            .filter(|m| canonical_mode_name(&m.name) == m.name)
            .map(|m| m.name.clone())
            .collect()
    }

    /// Reject a name that is not part of the interactive cycle.
    ///
    /// This is a *cycle* predicate, not an existence check: agenthicc's
    /// `ModeManager.cycle()` walks `registry.all()`, which contains only
    /// `SELECTABLE_MODE_NAMES` (`Safe`, `Plan`, `Yolo`). `Replay` is registered
    /// with `selectable=False` and is therefore outside the cycle too, so
    /// "not cyclable" never meant "not a real mode".
    ///
    /// agenthicc rejects `debug` because it registers no such mode at all
    /// (`UnknownModeError`), not because of this cycle rule. xaft now matches
    /// that directly: there is no built-in `debug` mode and
    /// `ModeRegistry::register` refuses the name, so `set("debug")` is an
    /// unknown mode. This predicate is only about the cycle.
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
    /// - Sets `req.mode_approval_filter` from `active().approval_filter` —
    ///   the tools Safe makes the user approve before each call.
    pub fn apply_to_run_request(&self, req: &mut RunRequest) {
        let mode = self.active();

        req.mode_system_patch = if mode.system_patch.is_empty() {
            None
        } else {
            Some(mode.system_patch.clone())
        };

        req.mode_tool_filter = mode.tool_filter.clone();
        req.mode_approval_filter = mode.approval_filter.clone();
    }

    /// Immutable reference to the registry.
    pub fn registry(&self) -> &ModeRegistry {
        &self.registry
    }

    /// Register a new mode (or replace an existing one by name). A reserved
    /// name (`debug`) is refused; returns whether the mode was registered.
    pub fn register_mode(&mut self, mode: AgentMode) -> bool {
        self.registry.register(mode)
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
            mode_approval_filter: None,
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
        assert!(!ModeManager::is_cyclable("debug")); // not a mode at all
        assert!(!ModeManager::is_cyclable("replay")); // internal
        assert!(!ModeManager::is_cyclable("nonexistent"));
    }

    /// agenthicc's `/mode debug` raises `UnknownModeError`; so does xaft's.
    #[test]
    fn debug_is_an_unknown_mode() {
        let mut mgr = ModeManager::default_builtin();
        assert!(matches!(mgr.set("debug"), Err(ModeError::UnknownMode(_))));
        assert!(mgr.set_internal("debug").is_err(), "not even internally");
        assert_eq!(
            mgr.active_name(),
            "auto",
            "a refused switch changes nothing"
        );
    }

    /// Row 7: Replay is internal — never selectable, never cycled, only
    /// entered (and left) by trusted lifecycle code.
    #[test]
    fn replay_is_entered_only_internally_and_restores() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set("plan").unwrap();
        assert!(matches!(mgr.set("replay"), Err(ModeError::UnknownMode(_))));
        assert_eq!(mgr.active_name(), "plan");

        mgr.set_internal("replay").unwrap();
        assert_eq!(mgr.active_name(), "replay");
        let mut req = make_req();
        mgr.apply_to_run_request(&mut req);
        let filter = req.mode_tool_filter.as_ref().expect("replay filters tools");
        assert!(
            !filter("read_file") && !filter("write_file"),
            "everything blocked"
        );

        mgr.restore("plan").unwrap();
        assert_eq!(mgr.active_name(), "plan");
        // The cycle never lands on replay.
        for _ in 0..6 {
            assert_ne!(mgr.cycle().name, "replay");
        }
    }

    /// Row 1: Safe carries agenthicc's approval requirement into the run.
    #[test]
    fn safe_sets_the_run_approval_filter_and_auto_clears_it() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set("safe").unwrap();
        let mut req = make_req();
        mgr.apply_to_run_request(&mut req);
        assert!(req.mode_tool_filter.is_none(), "Safe hard-blocks nothing");
        let needs = req.mode_approval_filter.as_ref().expect("approval filter");
        assert!(needs("write_file") && needs("bash_exec") && needs("read_url"));
        assert!(!needs("read_file") && !needs("git_status"));

        mgr.set("auto").unwrap();
        mgr.apply_to_run_request(&mut req);
        assert!(req.mode_approval_filter.is_none());
    }

    #[test]
    fn selectable_names_are_the_ones_mode_accepts_by_name() {
        let mgr = ModeManager::default_builtin();
        assert_eq!(mgr.selectable_names(), vec!["auto", "plan", "safe"]);
    }

    #[test]
    fn reject_if_not_cyclable() {
        let mgr = ModeManager::default_builtin();
        assert!(mgr.reject_if_not_cyclable("debug").is_err());
        assert!(mgr.reject_if_not_cyclable("replay").is_err());
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

    // ── Custom-registry construction and source bookkeeping ────────────────

    fn mode(name: &str, source: &str) -> AgentMode {
        AgentModeBuilder::new(name, name.to_uppercase())
            .source_id(source)
            .build()
    }

    #[test]
    fn new_accepts_a_non_empty_registry_and_starts_at_the_first_mode() {
        // The Ok arm of `ModeManager::new` — only its error arm had a test.
        let mut reg = ModeRegistry::new();
        reg.register(mode("alpha", "custom"));
        reg.register(mode("beta", "custom"));

        let mgr = ModeManager::new(reg).expect("a non-empty registry is accepted");
        assert_eq!(mgr.active_name(), "alpha");
        assert_eq!(mgr.registry().len(), 2);
    }

    #[test]
    fn set_cycle_names_replaces_the_cycle_and_none_restores_the_default() {
        let mut mgr = ModeManager::default_builtin();
        mgr.set_cycle_names(Some(vec!["plan".into(), "safe".into()]));

        // Only the two named modes participate now.
        mgr.set("plan").unwrap();
        mgr.cycle();
        assert_eq!(mgr.active_name(), "safe");
        mgr.cycle();
        assert_eq!(mgr.active_name(), "plan");

        // `None` puts the built-in Safe → Plan → Yolo cycle back.
        mgr.set_cycle_names(None);
        mgr.set("auto").unwrap();
        mgr.cycle();
        assert_eq!(mgr.active_name(), "safe");
    }

    #[test]
    fn cycle_falls_back_to_plain_advance_when_the_next_cycle_name_is_absent() {
        // A custom registry that registers only two of the three cycle names:
        // the cycle resolves `safe` next, finds no such mode, and must advance
        // by index rather than panic.
        let mut reg = ModeRegistry::new();
        reg.register(mode("auto", "custom"));
        reg.register(mode("plan", "custom"));
        let mut mgr = ModeManager::new(reg).unwrap();

        assert_eq!(mgr.active_name(), "auto");
        // canonical("auto") is "auto", which is last in the cycle, so the next
        // cycle name is "safe" — not registered here.
        let next = mgr.cycle().name.clone();
        assert_eq!(next, "plan");
    }

    #[test]
    fn register_mode_adds_new_modes_and_replaces_existing_ones() {
        let mut mgr = ModeManager::default_builtin();
        let before = mgr.registry().len();

        mgr.register_mode(mode("mcp-tool", "mcp"));
        assert_eq!(mgr.registry().len(), before + 1);
        assert_eq!(mgr.registry().get("mcp-tool").unwrap().source_id, "mcp");

        // Re-registering the same name replaces in place, keeping the count.
        mgr.register_mode(mode("mcp-tool", "mcp-v2"));
        assert_eq!(mgr.registry().len(), before + 1);
        assert_eq!(mgr.registry().get("mcp-tool").unwrap().source_id, "mcp-v2");
    }

    #[test]
    fn unregister_source_that_removed_the_active_mode_falls_back_to_the_first() {
        let mut reg = ModeRegistry::new();
        reg.register(mode("a", "src1"));
        reg.register(mode("b", "src2"));
        reg.register(mode("c", "src1"));
        let mut mgr = ModeManager::new(reg).unwrap();
        assert_eq!(mgr.active_name(), "a");

        mgr.unregister_source("src1");
        assert!(mgr.registry().get("a").is_none());
        assert_eq!(mgr.active_name(), "b", "index must reset to 0");
    }

    #[test]
    fn unregister_source_that_kept_the_active_mode_resyncs_its_index() {
        let mut reg = ModeRegistry::new();
        reg.register(mode("a", "src1"));
        reg.register(mode("b", "src2"));
        reg.register(mode("c", "src1"));
        let mut mgr = ModeManager::new(reg).unwrap();
        mgr.set("b").unwrap();
        assert_eq!(mgr.active_name(), "b");

        // Removing src1 drops `a` and `c`; `b` survives but shifts from index 1
        // to index 0, so the cached index has to be re-synced.
        mgr.unregister_source("src1");
        assert_eq!(mgr.registry().len(), 1);
        assert_eq!(mgr.active_name(), "b");
        assert_eq!(mgr.active().name, "b");
    }
}
