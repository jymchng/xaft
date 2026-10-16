//! `/skills` end-to-end: parser -> registry -> handler -> transcript.
//!
//! The unit tests in `slash/commands/skills.rs` prove the rendering given a
//! hand-built list. These prove the seams, each of which is a defect shape
//! recorded somewhere in this repo:
//!
//! * the trigger parses AND resolves to a registered handler (the `/usage`
//!   defect: listed in the table, unreachable by the parser);
//! * `/skills` reads the list `AppState::init_skills` published, rather than
//!   a private copy — a handler wired to its own empty `Arc` would answer
//!   "no skills found" forever while `$` offered the loaded ones;
//! * `/skills reload` is refused rather than accepted and ignored, because
//!   xaft has no reload hook (agenthicc's third branch, declined and recorded
//!   in the parity row);
//! * the result lands in the transcript, so the listing is visible rather
//!   than computed and dropped.

use xaft_skills::Skill;
use xaft_tui::slash::parser::SlashCommandParser;
use xaft_tui::slash::{CommandResult, SlashCommand};
use xaft_tui::state::AppState;

fn skill(name: &str, description: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        tags: vec![],
        body: format!("body of {name}"),
        source_path: format!("/proj/.xaft/skills/{name}.md"),
        priority: 0,
    }
}

/// Dispatch a slash command through `AppState`'s registry and context, the
/// way `execute_slash_command` does.
fn run(state: &mut AppState, input: &str) -> CommandResult {
    let command = match SlashCommandParser::parse(input) {
        Some(Ok(c)) => c,
        Some(Err(e)) => panic!("{input:?} failed to parse: {e}"),
        None => panic!("{input:?} did not parse as a slash command"),
    };
    let args = match input.find(char::is_whitespace) {
        Some(i) => input[i + 1..].trim().to_string(),
        None => String::new(),
    };
    let handler = state.slash_registry.get(&command);
    handler.execute(state.build_command_context(args, command))
}

fn texts(result: CommandResult) -> String {
    match result {
        CommandResult::StyledLines(lines) => lines
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        CommandResult::Lines(v) => v.join("\n"),
        CommandResult::Error(e) => e,
        other => panic!("unexpected result: {other:?}"),
    }
}

fn committed(state: &AppState) -> String {
    xaft_tui::state::commit_line_texts(&state.mutations)
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

// ── the trigger is reachable ────────────────────────────────────────────────

#[test]
fn skills_parses_and_has_a_registered_handler() {
    assert_eq!(
        SlashCommandParser::parse("/skills"),
        Some(Ok(SlashCommand::Skills))
    );

    let state = AppState::new("");
    let handler = state.slash_registry.get(&SlashCommand::Skills);
    assert!(
        !handler.description().is_empty(),
        "a handler with no description is invisible in /help"
    );
    assert!(
        !handler.is_async(),
        "/skills reads a list already in memory; it should not need a runtime"
    );
}

// ── the listing reads what init_skills loaded ───────────────────────────────

#[test]
fn a_loaded_skill_appears_in_the_listing() {
    let mut state = AppState::new("");
    state.init_skills(vec![
        skill("rust-review", "Rust code review checklist"),
        skill("commit-msg", "Conventional commit message format"),
    ]);

    let text = texts(run(&mut state, "/skills"));
    assert!(text.contains("$rust-review"), "{text}");
    assert!(text.contains("$commit-msg"), "{text}");
    assert!(text.contains("Rust code review checklist"), "{text}");
    assert!(
        text.contains("/proj/.xaft/skills/rust-review.md"),
        "the source path must be reported: {text}"
    );
}

/// The specific false-parity shape: `AppState::new` builds the registry before
/// any skill is loaded, so a handler that captured the list at construction
/// time lists nothing. This drives the same order production does.
#[test]
fn the_registry_was_built_before_the_skills_were_loaded() {
    let mut state = AppState::new("");
    assert!(
        texts(run(&mut state, "/skills")).contains("(no skills found)"),
        "a fresh session has loaded no skills"
    );

    state.init_skills(vec![skill("late", "loaded after the registry was built")]);

    let text = texts(run(&mut state, "/skills"));
    assert!(
        text.contains("$late"),
        "the handler must see skills published after it was constructed: {text}"
    );
    assert!(!text.contains("(no skills found)"), "{text}");
}

#[test]
fn a_second_init_skills_replaces_the_listing_rather_than_appending() {
    let mut state = AppState::new("");
    state.init_skills(vec![skill("review", "first load")]);
    state.init_skills(vec![skill("deploy", "second load")]);

    let text = texts(run(&mut state, "/skills"));
    assert!(text.contains("$deploy"), "{text}");
    assert!(
        !text.contains("$review"),
        "the listing must mirror the latest load, not accumulate: {text}"
    );
}

/// `/skills` and the `$` picker must answer from one list. If they can diverge,
/// the listing is a claim about the picker that nothing checks.
#[test]
fn the_listing_and_the_dollar_picker_agree() {
    let mut state = AppState::new("");
    state.init_skills(vec![skill("review", "Rust code review checklist")]);

    let listed = texts(run(&mut state, "/skills"));
    assert!(listed.contains("$review"), "{listed}");

    state.input_bar.set_text("$rev");
    state.refresh_trigger();
    let picker = state
        .active_trigger
        .as_ref()
        .expect("typing '$rev' must open the skill picker");
    assert!(
        picker.items.iter().any(|i| i.display.contains("review")),
        "the picker that /skills claims to describe offered nothing"
    );
}

// ── the declined reload branch is refused, not faked ────────────────────────

#[test]
fn skills_reload_is_refused_with_its_reason() {
    let Err(message) = SlashCommandParser::parse("/skills reload").expect("recognised trigger")
    else {
        panic!("/skills reload must not parse into a command that quietly does nothing");
    };
    assert!(
        message.contains("reload") && message.contains("/skills"),
        "the refusal must name the argument and the usage: {message}"
    );
}

#[test]
fn any_other_argument_is_refused_too() {
    for input in ["/skills all", "/skills list", "/skills reload extra"] {
        assert!(
            matches!(SlashCommandParser::parse(input), Some(Err(_))),
            "{input} must be refused: /skills takes no arguments"
        );
    }
}

// ── the listing reaches the transcript ─────────────────────────────────────

#[test]
fn the_listing_is_committed_to_the_transcript() {
    let mut state = AppState::new("");
    state.init_skills(vec![skill("review", "Rust code review checklist")]);

    let result = run(&mut state, "/skills");
    state.apply_command_result(result);

    let transcript = committed(&state);
    assert!(
        transcript.contains("$review"),
        "the listing was computed and dropped instead of shown: {transcript}"
    );
}
