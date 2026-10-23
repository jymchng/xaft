//! `ask_user` through the TUI state machine: a `QuestionsAsked` event opens
//! the questions overlay, keys answer it, and the answers are queued for the
//! gate — agenthicc's `QuestionsOverlay` (PRD-100).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use xaft_runtime::phased::question::UserQuestion;
use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::AppState;

fn key(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ask(state: &mut AppState, id: &str, questions: Vec<UserQuestion>) {
    state.handle_event(TuiEvent::QuestionsAsked {
        id: id.to_string(),
        questions,
        timeout_s: 300.0,
    });
}

fn db_and_cache() -> Vec<UserQuestion> {
    vec![
        UserQuestion::new(
            "db",
            "Which database?",
            vec!["postgres".into(), "sqlite".into()],
        ),
        UserQuestion::new("cache", "Which cache?", vec!["redis".into()]),
    ]
}

#[test]
fn a_round_opens_the_overlay_and_its_answers_reach_the_gate_queue() {
    let mut state = AppState::new(String::new());
    ask(&mut state, "q-1", db_and_cache());

    let rows = state.overlay_snapshot().expect("the overlay is drawn");
    assert!(
        rows[0].text.contains("Questions (0 of 2 answered)"),
        "{}",
        rows[0].text
    );
    assert!(rows.iter().any(|r| r.text.contains("Which database?")));
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Other — type your answer"))
    );

    state.handle_event(key(KeyCode::Enter)); // postgres
    state.handle_event(key(KeyCode::Enter)); // redis → submit
    assert!(state.questions.is_none(), "submitting closes the overlay");
    assert_eq!(
        state.pending_question_answers,
        vec![(
            "q-1".to_string(),
            Some(vec!["postgres".to_string(), "redis".to_string()])
        )]
    );
}

#[test]
fn the_overlay_is_modal_and_typed_keys_do_not_reach_the_input_bar() {
    let mut state = AppState::new(String::new());
    ask(
        &mut state,
        "q-2",
        vec![UserQuestion::free_text("why", "Why?")],
    );
    state.handle_event(key(KeyCode::Enter)); // open the free-text editor
    for c in "speed".chars() {
        state.handle_event(key(KeyCode::Char(c)));
    }
    state.handle_event(TuiEvent::Paste(" matters".into()));
    assert!(
        state.input_bar.text().is_empty(),
        "keys and pastes belong to the open round"
    );
    state.handle_event(key(KeyCode::Enter));
    assert_eq!(
        state.pending_question_answers,
        vec![("q-2".to_string(), Some(vec!["speed matters".to_string()]))]
    );
}

#[test]
fn rounds_queue_behind_each_other_and_esc_cancels_one() {
    let mut state = AppState::new(String::new());
    ask(&mut state, "q-a", db_and_cache());
    ask(&mut state, "q-b", vec![UserQuestion::free_text("x", "X?")]);
    assert_eq!(state.queued_questions.len(), 1);

    state.handle_event(key(KeyCode::Esc));
    assert_eq!(
        state.pending_question_answers,
        vec![("q-a".to_string(), None)],
        "Esc reports a cancelled round"
    );
    assert_eq!(
        state.questions.as_ref().map(|q| q.id.as_str()),
        Some("q-b"),
        "the queued round opens next"
    );
}

#[test]
fn an_expired_round_closes_without_an_answer() {
    let mut state = AppState::new(String::new());
    ask(&mut state, "q-a", db_and_cache());
    ask(&mut state, "q-b", db_and_cache());
    state.handle_event(TuiEvent::QuestionsExpired { id: "q-b".into() });
    assert!(
        state.queued_questions.is_empty(),
        "a queued round can expire too"
    );
    state.handle_event(TuiEvent::QuestionsExpired { id: "q-a".into() });
    assert!(state.questions.is_none());
    assert!(
        state.pending_question_answers.is_empty(),
        "the gate already reported the timeout; nothing is sent back"
    );
}

#[test]
fn ctrl_c_still_reaches_the_cancel_path() {
    let mut state = AppState::new(String::new());
    ask(&mut state, "q-1", db_and_cache());
    state.handle_event(TuiEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert!(
        state.questions.is_some(),
        "Ctrl+C is not an answer: the round stays open"
    );
    assert!(state.pending_question_answers.is_empty());

    state.close_questions();
    assert!(state.questions.is_none() && state.queued_questions.is_empty());
}
