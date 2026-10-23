//! `/workflow` must change what the NEXT RUN actually does.
//!
//! Found during G10 verification of commit 56915c9: `AppState::new` built two
//! independent `Arc<WorkflowSelection>` values — one cloned into the slash
//! registry, a second assigned to the `workflow_selection` field. `/workflow`
//! writes through the registry and `resolved_workflow()` reads the field, so
//! the command printed
//!
//! ```text
//!   Workflow set to 'meta' for this session.
//! ```
//!
//! and the next run still used the configured default. Every pre-existing
//! test missed it because they each built their own `WorkflowSelection` and
//! handed it straight to the handler, so nothing ever went through
//! `AppState`'s construction.
//!
//! These tests go through the real objects — `AppState::new`,
//! `AppState::build_command_context`, `state.slash_registry` — because that
//! wiring IS the thing under test.

use xaft_runtime::WorkflowConfig;
use xaft_tui::AppState;
use xaft_tui::slash::{CommandResult, SlashCommand};

/// Run `/workflow <arg>` through the same three objects the TUI uses.
fn run_slash_workflow(state: &AppState, arg: &str) -> CommandResult {
    let command = SlashCommand::Workflow {
        name: (!arg.is_empty()).then(|| arg.to_string()),
    };
    let ctx = state.build_command_context(arg.to_string(), command.clone());
    state.slash_registry.get(&command).execute(ctx)
}

fn label(cfg: WorkflowConfig) -> &'static str {
    match cfg {
        WorkflowConfig::Standard => "standard",
        WorkflowConfig::Dynamic { .. } => "dynamic",
        WorkflowConfig::Meta { .. } => "meta",
        WorkflowConfig::Phased { .. } => "phased",
    }
}

fn rendered(result: CommandResult) -> String {
    match result {
        CommandResult::Lines(lines) => lines.join("\n"),
        other => panic!("expected Lines, got {other:?}"),
    }
}

#[test]
fn slash_workflow_selection_reaches_the_run_loop() {
    let state = AppState::new("do the thing");

    // Before: no override, so the configured `[workflow] mode` decides.
    assert_eq!(label(state.resolved_workflow()), "standard");

    let out = rendered(run_slash_workflow(&state, "meta"));
    assert!(out.contains("Workflow set to 'meta'"), "{out}");

    // THE ASSERTION THAT FAILS AGAINST 56915c9: the selection the run loop
    // reads is the same object `/workflow` just wrote to.
    assert_eq!(
        state.workflow_selection.current().as_deref(),
        Some("meta"),
        "/workflow confirmed a switch the run loop cannot see"
    );
    assert_eq!(
        label(state.resolved_workflow()),
        "meta",
        "the next run must actually use the selected workflow"
    );
}

#[test]
fn slash_workflow_reset_returns_the_run_to_the_configured_default() {
    let state = AppState::new("do the thing");
    run_slash_workflow(&state, "meta");
    assert_eq!(label(state.resolved_workflow()), "meta");

    run_slash_workflow(&state, "reset");

    assert_eq!(state.workflow_selection.current(), None);
    assert_eq!(
        label(state.resolved_workflow()),
        "standard",
        "reset must fall back to [workflow] mode, not keep the override"
    );
}

#[test]
fn switching_twice_keeps_working_through_the_same_registry() {
    let state = AppState::new("do the thing");

    run_slash_workflow(&state, "meta");
    assert_eq!(label(state.resolved_workflow()), "meta");

    run_slash_workflow(&state, "dynamic");
    assert_eq!(
        state.workflow_selection.current().as_deref(),
        Some("dynamic")
    );
    assert_eq!(
        label(state.resolved_workflow()),
        "dynamic",
        "a second switch must replace the first, not accumulate"
    );
}

#[test]
fn an_unknown_workflow_changes_neither_the_selection_nor_the_run() {
    let state = AppState::new("do the thing");

    let out = rendered(run_slash_workflow(&state, "nope"));

    assert!(out.contains("Unknown workflow: nope"), "{out}");
    assert_eq!(
        state.workflow_selection.current(),
        None,
        "a rejected switch must not stick"
    );
    assert_eq!(label(state.resolved_workflow()), "standard");
}

/// `AppState::new` must not construct a second, unreachable selection.
///
/// Guards the specific shape of the defect: even if a future refactor moves
/// the field out of the struct literal, a second `WorkflowSelection::new()`
/// next to the registry build reintroduces it.
#[test]
fn app_state_new_constructs_exactly_one_workflow_selection() {
    let source = include_str!("../src/state.rs");

    let constructions = source
        .lines()
        .filter(|line| line.contains("WorkflowSelection::new()"))
        .count();
    assert_eq!(
        constructions, 1,
        "state.rs must construct WorkflowSelection exactly once and share it \
         with the slash registry; found {constructions} construction sites"
    );
}

/// The helper that builds a registry with a hidden selection must not be the
/// one `AppState` uses — that is the trap that produced the defect.
#[test]
fn app_state_uses_the_session_aware_registry_builder() {
    let source = include_str!("../src/state.rs");
    assert!(
        source.contains("build_registry_with_session("),
        "AppState must build its registry with the shared session selection"
    );
}

/// Sanity: with no override the run follows `[workflow] mode`, which is what
/// makes the assertions above observable. Config-level resolution itself is
/// covered by the `resolve_for_run` unit tests in `src/workflow.rs`; this file
/// only pins that the SELECTION reaches the run.
#[test]
fn with_no_override_the_run_follows_the_configured_default() {
    let state = AppState::new("t");
    assert_eq!(state.workflow_selection.current(), None);
    assert_eq!(label(state.resolved_workflow()), "standard");
}
