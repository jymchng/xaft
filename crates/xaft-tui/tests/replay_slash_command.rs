//! `/replay` end-to-end: parser -> registry -> async handler -> transcript.
//!
//! The unit tests in `slash/commands/replay.rs` prove the handler's output
//! given a hand-built context. These prove the three things that only fail at
//! the seams, each of which is the shape of a real defect recorded in this
//! repo:
//!
//! * the trigger parses AND resolves to a registered handler (the `/usage`
//!   defect: present in `aliases`, unreachable by the parser);
//! * the command reaches the session store `AppState` was given, rather than
//!   reporting "no store available" in the live TUI;
//! * the result actually lands in the transcript, so the replay is visible
//!   rather than computed and dropped.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::transport::Message;
use xaft_runtime::session::AgentSession;
use xaft_runtime::session_store::{InMemorySessionStore, SessionStore};
use xaft_tui::slash::parser::SlashCommandParser;
use xaft_tui::slash::{CommandResult, SlashCommand};
use xaft_tui::state::AppState;

// ── An in-memory ConversationStore standing in for the SQLite one ─────────

#[derive(Default)]
struct MemConv {
    inner: tokio::sync::RwLock<HashMap<String, Vec<Message>>>,
}

#[async_trait::async_trait]
impl ConversationStore for MemConv {
    async fn load(&self, id: &str) -> Result<Vec<Message>, AgtrsError> {
        Ok(self.inner.read().await.get(id).cloned().unwrap_or_default())
    }
    async fn save(&self, id: &str, messages: &[Message]) -> Result<(), AgtrsError> {
        self.inner
            .write()
            .await
            .insert(id.to_string(), messages.to_vec());
        Ok(())
    }
    async fn delete(&self, id: &str) -> Result<(), AgtrsError> {
        self.inner.write().await.remove(id);
        Ok(())
    }
}

const WORKDIR: &str = "/proj/replay-e2e";

/// An `AppState` wired to real stores, as `app.rs` wires it.
async fn wired_state() -> (AppState, Arc<InMemorySessionStore>, Arc<MemConv>) {
    let sessions = Arc::new(InMemorySessionStore::new());
    let conversations = Arc::new(MemConv::default());
    let mut state = AppState::new("");
    state.working_dir = PathBuf::from(WORKDIR);
    state.attach_session_stores(
        Some(Arc::clone(&sessions) as Arc<dyn SessionStore>),
        Some(Arc::clone(&conversations) as Arc<dyn ConversationStore>),
    );
    (state, sessions, conversations)
}

/// Save one session with a distinctive exchange. Returns its id.
async fn seed(sessions: &InMemorySessionStore, conversations: &MemConv, n: usize) -> String {
    let mut s = AgentSession::new(
        format!("task {n}"),
        PathBuf::from(WORKDIR),
        "default".into(),
        "model".into(),
    );
    s.updated_at = chrono::Utc::now() + chrono::Duration::seconds(n as i64);
    let id = s.id.to_string();
    sessions.save(&s).await.unwrap();
    conversations
        .save(
            &id,
            &[
                Message::user(&format!("ALPHA-QUESTION-{n}")),
                Message::assistant(&format!("ALPHA-ANSWER-{n}")),
            ],
        )
        .await
        .unwrap();
    id
}

/// Dispatch `/replay <args>` the way the input loop does: parse, look the
/// handler up in the real registry, build the real context, run async.
async fn dispatch(state: &mut AppState, args: &str) -> CommandResult {
    let input = if args.is_empty() {
        "/replay".to_string()
    } else {
        format!("/replay {args}")
    };
    let command = match SlashCommandParser::parse(&input) {
        Some(Ok(c)) => c,
        Some(Err(e)) => panic!("{input:?} failed to parse: {e}"),
        None => panic!("{input:?} is not recognised as a slash command"),
    };
    let handler = state.slash_registry.get(&command);
    handler
        .execute_boxed_async(state.build_command_context(args.to_string(), command))
        .await
}

fn texts(result: CommandResult) -> String {
    match result {
        CommandResult::Lines(v) => v.join("\n"),
        CommandResult::StyledLines(v) => v
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
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

/// The parser resolves `/replay` to its own variant, not to a neighbour, and
/// the registry has a handler for it.
#[test]
fn replay_parses_and_has_a_registered_handler() {
    assert_eq!(
        SlashCommandParser::parse("/replay"),
        Some(Ok(SlashCommand::Replay { id: None }))
    );
    assert_eq!(
        SlashCommandParser::parse("/replay abc123"),
        Some(Ok(SlashCommand::Replay {
            id: Some("abc123".into())
        }))
    );
    let state = AppState::new("");
    let handler = state.slash_registry.get(&SlashCommand::Replay { id: None });
    assert!(
        handler.is_async(),
        "/replay reads the store, so it is async"
    );
    assert!(!handler.description().is_empty());
}

#[tokio::test]
async fn no_argument_with_no_prior_session_prints_usage_and_survives_dispatch() {
    let (mut state, _s, _c) = wired_state().await;
    let out = texts(dispatch(&mut state, "").await);
    assert_eq!(
        out,
        "Usage: /replay <session-id> — no previous session found for this directory."
    );
}

#[tokio::test]
async fn unknown_id_truncates_to_16_characters() {
    let (mut state, _s, _c) = wired_state().await;
    let out = texts(dispatch(&mut state, "0123456789abcdef-and-then-some").await);
    assert!(
        out.starts_with("Session 0123456789abcdef not found."),
        "id must be truncated to 16 chars: {out}"
    );
    assert!(!out.contains("and-then-some"), "full id leaked: {out}");
}

/// The goal's explicit test: two sessions, one of them active, no argument.
#[tokio::test]
async fn no_argument_never_replays_the_current_session() {
    let (mut state, sessions, conversations) = wired_state().await;
    let older = seed(&sessions, &conversations, 0).await;
    let newer = seed(&sessions, &conversations, 1).await;
    // The active session is the newer one, read back through the store.
    state.session = sessions
        .load(&xaft_runtime::session::SessionId::from_string(
            newer.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(state.session.as_ref().unwrap().id.as_str(), newer);

    let out = texts(dispatch(&mut state, "").await);
    assert!(
        out.contains("ALPHA-QUESTION-0"),
        "must replay the earlier session: {out}"
    );
    assert!(
        !out.contains("ALPHA-QUESTION-1"),
        "must never replay the session in progress: {out}"
    );
    assert!(
        out.contains(&older[..8]),
        "banner names the older session: {out}"
    );
}

#[tokio::test]
async fn a_named_session_replays_and_its_turns_reach_the_transcript() {
    let (mut state, sessions, conversations) = wired_state().await;
    let id = seed(&sessions, &conversations, 7).await;

    let result = dispatch(&mut state, &id).await;
    assert!(
        matches!(result, CommandResult::StyledLines(_)),
        "a successful replay must produce styled transcript lines: {result:?}"
    );
    state.apply_command_result(result);

    let shown = committed(&state);
    assert!(shown.contains("replayed: "), "expected a banner: {shown}");
    assert!(
        shown.contains("❯ ALPHA-QUESTION-7"),
        "user turn missing: {shown}"
    );
    assert!(
        shown.contains("ALPHA-ANSWER-7"),
        "assistant turn missing: {shown}"
    );
}

#[tokio::test]
async fn a_session_without_history_does_not_replay_another_sessions_turns() {
    let (mut state, sessions, conversations) = wired_state().await;
    let with_history = seed(&sessions, &conversations, 3).await;
    // A metadata-only session: exists in the store, has no conversation.
    let empty = AgentSession::new(
        "empty",
        PathBuf::from(WORKDIR),
        "default".into(),
        "model".into(),
    );
    sessions.save(&empty).await.unwrap();

    let out = texts(dispatch(&mut state, empty.id.as_str()).await);
    assert!(
        out.contains("no recorded conversation to replay"),
        "an empty session must not borrow another session's transcript: {out}"
    );
    assert!(!out.contains("ALPHA-QUESTION-3"), "{out}");
    assert!(with_history.len() > 0);
}

/// Without the wiring `app.rs` now does, a live `/replay` could not read
/// anything. This pins the wiring the handler depends on.
#[tokio::test]
async fn an_unwired_state_reports_no_store_rather_than_silently_succeeding() {
    let mut state = AppState::new("");
    let out = texts(dispatch(&mut state, "").await);
    assert!(
        out.contains("No session store"),
        "an unwired state must say so instead of pretending: {out}"
    );
}
/// `attach_session_stores` must reach `build_command_context`. This is the seam
/// `app.rs` got wrong once already: an inner `let` shadowed the outer binding,
/// so the runtime held real stores while the state held `None`, and `/replay`
/// reported "no session store" for a feature that was plainly shipped. The
/// symptom is only visible if the state is inspected AFTER attaching.
#[tokio::test]
async fn attached_stores_reach_the_command_context() {
    let (state, _s, _c) = wired_state().await;
    assert!(
        state.session_store.is_some(),
        "session store was not attached"
    );
    assert!(
        state.conversation_store.is_some(),
        "conversation store was not attached"
    );
    let ctx = state.build_command_context(String::new(), SlashCommand::Replay { id: None });
    assert!(
        ctx.session_store.is_some(),
        "context lost the session store"
    );
    assert!(
        ctx.conversation_store.is_some(),
        "context lost the conversation store"
    );
}
