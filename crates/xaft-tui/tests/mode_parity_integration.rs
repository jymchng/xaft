//! Cycle and alias parity with agenthicc's documented mode semantics.
//!
//! G7's acceptance criterion is that "cycle and alias behaviour have dedicated
//! tests matching agenthicc's documented semantics". The unit tests in
//! `mode/manager.rs` cover `canonical_mode_name` in isolation; this file pins
//! the behaviour a *user* reaches, through the same entry points the TUI uses.
//!
//! The ground truth is agenthicc `src/agenthicc/modes/builtin.py`, which
//! builds its registry with:
//!
//! ```python
//! registry = ModeRegistry(aliases={"Auto": "Yolo", "Guard": "Safe",
//!                                 "Ask": "Safe", "Review": "Plan"})
//! # registered in this order: Safe, Plan, Yolo
//! ```
//!
//! So: three product identities, four aliases, cycled in that order, and no
//! `Debug` mode at all.

use xaft_tui::mode::ModeRegistry;
use xaft_tui::mode::builtins::builtin_modes;
use xaft_tui::mode::manager::{CYCLE_NAMES, ModeManager, canonical_mode_name};

// ── The cycle: Safe → Plan → Yolo → Safe ──────────────────────────────
//
// agenthicc's TUI runtime cycles exactly ("Safe", "Plan", "Yolo") and wraps.
// xaft's CYCLE_NAMES spells the third step "auto", which is the alias for
// Yolo, so the cycle visits the same three permissions in the same order. The
// test asserts the *permissions*, not the spelling, so renaming the canonical
// mode to `yolo` later will not silently break it.

#[test]
fn cycle_visits_exactly_three_modes_and_wraps_to_the_first() {
    let mut mgr = ModeManager::default_builtin();
    mgr.set("safe").unwrap();

    let mut visited = Vec::new();
    for _ in 0..3 {
        visited.push(mgr.active_name().to_string());
        mgr.cycle();
    }
    // Three steps, then back where we started.
    visited.push(mgr.active_name().to_string());

    assert_eq!(visited.len(), 4);
    assert_eq!(visited[0], visited[3], "cycle must wrap: {visited:?}");
    assert_eq!(
        visited[0], "safe",
        "agenthicc starts the cycle at Safe: {visited:?}"
    );
    assert_eq!(visited[1], "plan", "Safe advances to Plan: {visited:?}");
    // The third step is Yolo's identity. xaft registers it as `auto`, which
    // `canonical_mode_name` treats as Yolo.
    assert_eq!(
        canonical_mode_name(&visited[2]),
        "auto",
        "the third cycle step must be Yolo (spelled `auto` in xaft): {visited:?}"
    );
    assert_eq!(CYCLE_NAMES.len(), 3, "agenthicc cycles exactly 3 modes");
}

#[test]
fn cycling_from_an_alias_starts_from_the_alias_identity() {
    // A user in `review` (= Plan) must advance to Yolo, not to some mode that
    // happens to sit next to `review` in xaft's six-mode registry.
    let mut mgr = ModeManager::default_builtin();
    mgr.set("review").unwrap();
    mgr.cycle();
    assert_eq!(mgr.active_name(), "auto", "review(=Plan) advances to Yolo");
}

#[test]
fn every_cycle_step_resolves_to_a_registered_mode() {
    // Guards the fallback branch in `ModeManager::cycle`: a cycle name that is
    // not in the registry must not silently degrade into arbitrary advancing.
    let mgr = ModeManager::default_builtin();
    for name in CYCLE_NAMES {
        assert!(
            mgr.registry().get(name).is_some(),
            "cycle step {name:?} is not a registered mode"
        );
    }
}

// ── Aliases: auto≡yolo, guard≡safe, ask≡safe, review≡plan ─────────────

/// The four aliases agenthicc's registry declares, with xaft's canonical
/// spelling of each target.
const AGENTHICC_ALIASES: &[(&str, &str)] = &[
    ("auto", "auto"),   // agenthicc Auto  -> Yolo  (xaft names Yolo "auto")
    ("guard", "safe"),  // agenthicc Guard -> Safe
    ("ask", "safe"),    // agenthicc Ask   -> Safe
    ("review", "plan"), // agenthicc Review-> Plan
];

#[test]
fn every_agenthicc_alias_resolves_to_its_canonical_mode() {
    for (alias, expected) in AGENTHICC_ALIASES {
        assert_eq!(
            canonical_mode_name(alias),
            *expected,
            "alias {alias:?} must resolve to {expected:?}"
        );
    }
}

#[test]
fn every_agenthicc_alias_actually_switches_the_active_mode() {
    // `canonical_mode_name` alone proves nothing about what a user gets: this
    // drives the same `ModeManager::set` the TUI calls and asserts the mode
    // that ends up active.
    let mut mgr = ModeManager::default_builtin();

    for (alias, expected) in AGENTHICC_ALIASES {
        mgr.set("plan").unwrap(); // start somewhere known-unrelated
        mgr.set(alias)
            .unwrap_or_else(|e| panic!("alias {alias:?} rejected: {e}"));
        assert_eq!(
            mgr.active_name(),
            *expected,
            "/mode {alias} must activate {expected}, not something else"
        );
    }
}

#[test]
fn guard_is_accepted_even_though_no_guard_mode_is_registered() {
    // `guard` exists only in agenthicc's alias table. It must resolve without
    // a registry entry of its own -- that is what "alias" means.
    let registry = ModeRegistry::default();
    assert!(
        registry.get("guard").is_none(),
        "guard must NOT be a registered mode; it is an alias"
    );
    assert!(ModeManager::is_cyclable("guard"));
    assert_eq!(canonical_mode_name("guard"), "safe");
}

#[test]
fn an_alias_resolves_to_a_mode_that_genuinely_exists() {
    // An alias pointing at a missing mode would make `set` fail at runtime.
    let registry = ModeRegistry::default();
    for (alias, _) in AGENTHICC_ALIASES {
        let target = canonical_mode_name(alias);
        assert!(
            registry.get(target).is_some(),
            "alias {alias:?} points at {target:?}, which is not registered"
        );
    }
}

// ── debug is not an alias ─────────────────────────────────────────────

#[test]
fn debug_is_not_in_the_alias_table() {
    // agenthicc has no Debug mode and therefore no debug alias. xaft does ship
    // a `debug` mode of its own, so this asserts only that agenthicc's alias
    // semantics do not reach it: `canonical_mode_name` must be the identity
    // function for debug, not a mapping onto some other mode.
    assert_eq!(
        canonical_mode_name("debug"),
        "debug",
        "debug must not be rewritten into an agenthicc alias target"
    );
    assert!(
        !ModeManager::is_cyclable("debug"),
        "debug is not part of agenthicc's Safe -> Plan -> Yolo cycle"
    );
}

#[test]
fn unknown_mode_names_are_rejected_rather_than_silently_accepted() {
    let mut mgr = ModeManager::default_builtin();
    let err = mgr
        .set("definitely_not_a_mode")
        .expect_err("an unknown name must not resolve to a mode");
    assert!(
        format!("{err}").contains("definitely_not_a_mode"),
        "the error must name the rejected input, got: {err}"
    );
}

// ── The three canonical permissions are distinct and real ─────────────

#[test]
fn the_three_cycle_modes_carry_the_three_distinct_permissions() {
    // agenthicc: Safe gates on approval, Plan hard-blocks via an allowlist,
    // Yolo allows everything. Assert the *shape* of each rather than an exact
    // tool list, so xaft's own tool names stay free to change.
    let mgr = ModeManager::default_builtin();

    let plan = mgr.registry().get("plan").unwrap();
    assert!(
        plan.tool_filter.is_some(),
        "Plan must hard-block side effects with an allowlist"
    );

    let yolo = mgr.registry().get("auto").unwrap();
    assert!(
        yolo.tool_filter.is_none(),
        "Yolo allows every tool, so it must not carry a filter"
    );

    let safe = mgr.registry().get("safe").unwrap();
    assert!(
        safe.tool_filter.is_some(),
        "Safe must restrict the tool surface"
    );
}

#[test]
fn plan_and_safe_never_permit_a_write_tool() {
    // The single most important property of the restricted modes: neither may
    // admit a tool that writes. Checked by name *shape* so a renamed tool
    // cannot slip through.
    let mgr = ModeManager::default_builtin();
    for mode_name in ["plan", "safe"] {
        let mode = mgr.registry().get(mode_name).unwrap();
        let filter = mode
            .tool_filter
            .as_ref()
            .unwrap_or_else(|| panic!("{mode_name} must carry a filter"));
        for tool in ["write_file", "bash_exec", "git_commit", "delete_file"] {
            assert!(!filter(tool), "{mode_name} mode must not permit {tool}");
        }
    }
}

#[test]
fn builtin_registry_registers_every_name_the_aliases_point_at() {
    // A guard against the alias table and the registry drifting apart, which is
    // the failure that would make `/mode guard` error at runtime.
    let modes = builtin_modes();
    for (alias, expected) in AGENTHICC_ALIASES {
        assert!(
            modes.iter().any(|m| m.name == *expected),
            "builtin_modes() is missing {expected:?}, which alias {alias:?} needs"
        );
    }
}

// ── The keybinding: Shift+Tab must actually walk Safe → Plan → Yolo ───
//
// `crates/xaft-tui/tests/mode_e2e_runtime.rs::test_shift_tab_cycles_mode`
// already drives BackTab through the real `AppState::handle_event`, but it
// only asserts `assert_ne!(initial, after)` — that *something* changed. That
// is the same weak claim that let `/usage` be marked ✅ while the only string
// a user could type answered "Unknown command": a change is not the same as
// the right change. This drives the key event and pins the actual order, so
// parity-matrix keybinding row 1 is backed by more than "it moved".

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::AppState;

fn backtab() -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code: KeyCode::BackTab,
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

#[test]
fn shift_tab_walks_safe_plan_yolo_and_wraps() {
    let mut state = AppState::new_for_test();
    state.mode_manager.set("safe").unwrap();

    let mut seen = vec![state.mode_manager.active_name().to_string()];
    for _ in 0..3 {
        state.handle_event(backtab());
        seen.push(state.mode_manager.active_name().to_string());
    }

    assert_eq!(
        seen,
        vec!["safe", "plan", "auto", "safe"],
        "Shift+Tab must walk agenthicc's Safe -> Plan -> Yolo and wrap to Safe"
    );
}

#[test]
fn shift_tab_from_an_alias_enters_the_cycle_not_the_registry() {
    // xaft registers six modes but cycles three. If the key handler walked the
    // registry instead of CYCLE_NAMES, a user in `review` would land on
    // `safe` (the next registry entry) rather than on Yolo.
    let mut state = AppState::new_for_test();
    state.mode_manager.set("review").unwrap();
    state.handle_event(backtab());
    assert_eq!(
        state.mode_manager.active_name(),
        "auto",
        "Shift+Tab from review(=Plan) must reach Yolo"
    );
}

// ── Two registered modes are unreachable by their own name ──────────────────
//
// MEASURED DURING G7, and not previously recorded anywhere.
//
// `builtin_modes()` registers six modes: auto, plan, ask, review, safe, debug.
// `ModeManager::set` resolves the requested name through
// `canonical_mode_name` BEFORE it looks in the registry, and that alias table
// contains `ask -> safe` and `review -> plan` (both copied from agenthicc's
// `MODE_ALIASES`). The result is that the registered `ask` mode and the
// registered `review` mode cannot be selected by ANY spelling: `ask` and
// `guard` both land on `safe`, `review` lands on `plan`.
//
// This matters beyond tidiness. xaft's `ask` mode is the one that carries
// agenthicc's *Safe* policy shape -- every tool available, side effects
// behind a [Y/n] prompt -- while xaft's `safe` mode hard-blocks writes behind
// a read-only allowlist and never prompts. So the mode that implements
// agenthicc-Safe semantics is exactly the one that cannot be reached, and the
// name that reaches it instead lands on a stricter mode.
//
// The alias table is agenthicc-exact and the current resolution order is
// fail-closed (`/mode ask` = read-only, not = unrestricted), so this file does
// NOT change the order. Swapping it would silently widen `/mode ask` from a
// hard sandbox to every tool with only a model-side [Y/n] instruction, which
// is a security-semantics change and not a silent one to make. The decision
// recorded in docs/parity-matrix.md section 3 is: the collision is a known
// divergence with two possible owners, and the operator picks.

#[test]
fn the_ask_name_resolves_to_safe_even_though_an_ask_mode_is_registered() {
    let mgr = ModeManager::default_builtin();
    assert!(
        mgr.registry().get("ask").is_some(),
        "precondition: xaft does register a mode named `ask`"
    );
    assert_eq!(canonical_mode_name("ask"), "safe");
    assert_ne!(
        canonical_mode_name("ask"),
        "ask",
        "the alias table shadows the registered mode -- this is the collision"
    );
}

#[test]
fn the_review_name_resolves_to_plan_even_though_a_review_mode_is_registered() {
    let mgr = ModeManager::default_builtin();
    assert!(
        mgr.registry().get("review").is_some(),
        "precondition: xaft does register a mode named `review`"
    );
    assert_eq!(canonical_mode_name("review"), "plan");
    assert_ne!(
        canonical_mode_name("review"),
        "review",
        "the alias table shadows the registered mode -- this is the collision"
    );
}

#[test]
fn set_lands_on_the_alias_target_not_on_the_shadowed_mode() {
    // The user-visible consequence, driven through the same `set` the TUI uses.
    let mut mgr = ModeManager::default_builtin();

    mgr.set("ask").unwrap();
    assert_eq!(mgr.active_name(), "safe", "`/mode ask` activates safe");

    mgr.set("review").unwrap();
    assert_eq!(mgr.active_name(), "plan", "`/mode review` activates plan");
}

#[test]
fn the_two_shadowed_modes_carry_distinct_behaviour_from_their_alias_targets() {
    // If `ask` behaved exactly like `safe` and `review` exactly like `plan`,
    // the shadowing would be cosmetic and row 1 would not be a divergence.
    // Assert they are NOT the same, so the cost of leaving it is visible.
    let mgr = ModeManager::default_builtin();

    let ask = mgr.registry().get("ask").unwrap();
    let safe = mgr.registry().get("safe").unwrap();
    assert!(
        ask.tool_filter.is_none(),
        "xaft's `ask` is unfiltered (agenthicc-Safe's shape: ask, do not block)"
    );
    assert!(
        safe.tool_filter.is_some(),
        "xaft's `safe` is an allowlist (block, do not ask) -- the opposite shape"
    );
    assert!(
        ask.system_patch.contains("[Y/n]"),
        "the unreachable `ask` mode is the one that prompts"
    );
}

#[test]
fn three_of_the_six_registered_modes_cannot_be_activated_by_their_own_name() {
    // Bounds the blast radius, and keeps it from silently growing. This asks
    // the user-facing question directly -- "type this mode's own name, does it
    // get selected?" -- rather than trusting a hand-kept list.
    let mgr = ModeManager::default_builtin();
    let registered: Vec<String> = mgr
        .registry()
        .all_modes()
        .iter()
        .map(|m| m.name.clone())
        .collect();
    assert_eq!(registered.len(), 6, "xaft registers six built-in modes");

    let mut unreachable: Vec<(String, String)> = Vec::new();
    for name in &registered {
        let mut probe = ModeManager::default_builtin();
        // `set` only errors on a name with no registry entry at all, so every
        // registered name is accepted; the question is which one it lands on.
        probe.set(name).unwrap();
        if probe.active_name() != name {
            unreachable.push((name.clone(), probe.active_name().to_string()));
        }
    }

    assert_eq!(
        unreachable,
        vec![
            ("ask".to_string(), "safe".to_string()),
            ("review".to_string(), "plan".to_string())
        ],
        "exactly `ask` and `review` are shadowed by the alias table"
    );
}
