//! Shared transcript-replay rendering.
//!
//! Two callers project stored `Message`s into the transcript, and both now
//! read the **session store's own conversation** — the key
//! `xaft_session::conversation_key_for` derives from the session id, which is
//! the same store and key `xaft session inspect` reads:
//!
//! * `app.rs` (`--resume`) replays a **bounded tail** of that store: the newest
//!   `resume_transcript_turns` turns, chunked, behind a `Loading transcript…`
//!   status. That is Gap 4 in `knowledge/tui-parity-gap-analysis.md`, closed.
//! * `slash/commands/replay.rs` (`/replay`) replays a whole named session.
//!
//! This module owns the *selection* as well as the rendering, because the two
//! had drifted: `--resume` used to probe the orchestrator's in-memory
//! `{session}::workflow::planner` keys and render whatever it happened to find,
//! so the transcript a user saw after `--resume` depended on which memory keys
//! the orchestrator wrote rather than on the session log.

use agtrs_runtime::transport::{ContentBlock, Message, MessageContent, Role};

use crate::ephemeral::EphemeralState;
use crate::transcript::{LineKind, StyledLine};

/// How many characters of the session id a replay banner shows.
const BANNER_ID_LEN: usize = 8;

/// The status label shown while a resumed transcript is being appended.
///
/// agenthicc drives this from a `transcript_loading` signal that the status bar
/// checks *before* its running/waiting branches
/// (`agenthicc/tui/workspace/components.py`: `if transcript_loading:
/// state_text = "Loading transcript…"`), so it outranks every other status.
/// [`crate::ephemeral::build_ephemeral`] reproduces that precedence.
pub const LOADING_LABEL: &str = "Loading transcript…";

/// agenthicc's `BehaviourSettings.resume_transcript_turns` default (`config.py`).
///
/// The fallback used when no `TuiConfig::resume_transcript_turns` is supplied;
/// the config field is the real source.
pub const DEFAULT_RESUME_TRANSCRIPT_TURNS: usize = 20;

/// How many rendered lines are appended per chunk.
///
/// agenthicc replays in chunks of 64 with a loop yield between them, "so the
/// loading state is observable and the first Live redraw is not delayed"
/// (`Workspace.replay_transcript`). Same bound, same reason.
pub const REPLAY_CHUNK_LINES: usize = 64;

/// A resumed transcript, split so the status label stays observable while it
/// is appended.
#[derive(Debug, Clone)]
pub struct ResumeTranscript {
    /// The `Loading transcript…` status to show while `chunks` are appended.
    pub loading: EphemeralState,
    /// The rendered lines, in bounded chunks. Empty when there is nothing to
    /// replay, in which case `loading` should never be shown.
    pub chunks: Vec<Vec<StyledLine>>,
}

impl ResumeTranscript {
    /// Every replayed line, in order — the un-chunked view used by tests and
    /// by callers that are not driving an incremental renderer.
    pub fn lines(&self) -> Vec<StyledLine> {
        self.chunks.iter().flatten().cloned().collect()
    }

    /// True when there is nothing to replay, so the loading status should be
    /// skipped entirely.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// Build the `Loading transcript…` ephemeral status.
///
/// Kept next to the projection so the label and the chunking it is observed
/// during cannot drift apart.
pub fn loading_state() -> EphemeralState {
    EphemeralState {
        spinner_line: LOADING_LABEL.to_string(),
        status_line: None,
    }
}

/// The newest `max_turns` turns of `messages`.
///
/// A **turn** is a user message plus everything after it up to the next user
/// message. agenthicc counts `turn_start` markers while scanning its JSONL
/// session log backwards (`_load_recent_turns`); xaft's conversation store holds
/// a flat `Vec<Message>` with no turn markers, so `Role::User` is the
/// equivalent boundary and reproduces the same grouping.
///
/// * `max_turns == 0` keeps the complete history — agenthicc passes
///   `replay_turns or None`, and `SessionEventLog.load` bounds only when
///   `last_turns is not None and last_turns > 0`.
/// * Fewer than `max_turns` user turns means the whole history fits, so all of
///   it is returned.
/// * With no user message at all there are no turns to count. agenthicc falls
///   back to "their newest bounded event suffix" for logs predating turn
///   markers; the equivalent here is the newest `max_turns` *messages*.
pub fn tail_turns<'a>(messages: &'a [Message], max_turns: usize) -> &'a [Message] {
    if max_turns == 0 {
        return messages;
    }
    let user_starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();

    if user_starts.is_empty() {
        // No user turn to anchor on: bound by messages instead.
        let start = messages.len().saturating_sub(max_turns);
        return &messages[start..];
    }
    if user_starts.len() < max_turns {
        return messages;
    }
    // Index of the max_turns-th user message counted from the end.
    let start = user_starts[user_starts.len() - max_turns];
    &messages[start..]
}

/// Project `messages` into a chunked [`ResumeTranscript`] under a `resumed`
/// banner, keeping only the newest `max_turns` turns.
pub fn project_resume_transcript(
    messages: &[Message],
    session_id: &str,
    max_turns: usize,
) -> ResumeTranscript {
    let tail = tail_turns(messages, max_turns);
    if tail.is_empty() {
        return ResumeTranscript {
            loading: loading_state(),
            chunks: Vec::new(),
        };
    }
    let lines = render_replay_lines(tail, session_id, "resumed", false);
    ResumeTranscript {
        loading: loading_state(),
        chunks: lines
            .chunks(REPLAY_CHUNK_LINES)
            .map(<[StyledLine]>::to_vec)
            .collect(),
    }
}

/// Project the newest `max_turns` turns of `session_id`'s stored conversation.
///
/// This is the Gap 4 fix: `--resume` reads the **session log**, so it shows the
/// same turns `xaft session inspect` and `/replay` see. It no longer depends on
/// the orchestrator having written a `{session}::workflow::planner` memory key.
#[cfg(feature = "session")]
pub async fn load_resume_transcript(
    store: &dyn agtrs_runtime::memory::ConversationStore,
    session_id: &str,
    max_turns: usize,
) -> ResumeTranscript {
    use agtrs_runtime::memory::ConversationStore as _;
    let messages = store.load(session_id).await.unwrap_or_default();
    project_resume_transcript(&messages, session_id, max_turns)
}

/// The banner that introduces a replayed transcript.
///
/// `verb` is `"resumed"` for `--resume` and `"replayed"` for `/replay`, so a
/// reader can tell a transcript that was reloaded at startup from one that a
/// `/replay` pushed mid-session.
pub(crate) fn banner(verb: &str, session_id: &str) -> StyledLine {
    let short_id: String = session_id.chars().take(BANNER_ID_LEN).collect();
    let mut rule = String::from("─── ");
    rule.push_str(verb);
    rule.push_str(": ");
    rule.push_str(&short_id);
    rule.push(' ');
    let pad = 62usize.saturating_sub(rule.chars().count());
    rule.push_str(&"─".repeat(pad));
    StyledLine::new(format!("  {rule}"), LineKind::Separator)
}

/// Render `messages` as transcript lines, under a banner naming `session_id`.
///
/// User turns render as `❯ text` (matching live rendering), assistant text as
/// indented `AgentText` lines, and tool-use calls as compact `ToolCall`
/// markers. `Role::System` and `Role::Tool` messages are skipped as too noisy
/// for display.
///
/// `skip_user` omits `Role::User` turns — used for delegated-agent sections,
/// whose opening "user" turn is plan text piped in by the orchestrator rather
/// than the user's own words.
pub(crate) fn render_replay_lines(
    messages: &[Message],
    session_id: &str,
    verb: &str,
    skip_user: bool,
) -> Vec<StyledLine> {
    let mut out = vec![banner(verb, session_id)];
    for msg in messages {
        render_message_into(msg, &mut out, skip_user);
    }
    out
}

/// Render a single message into `out`.
pub(crate) fn render_message_into(msg: &Message, out: &mut Vec<StyledLine>, skip_user: bool) {
    match msg.role {
        Role::User => {
            if skip_user {
                return;
            }
            let text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::MultiPart(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            let file_refs: Vec<&str> = match &msg.content {
                MessageContent::MultiPart(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::FileRef { path, .. } = b {
                            Some(path.as_str())
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => vec![],
            };
            let text = text.trim().to_string();
            if !text.is_empty() {
                out.push(StyledLine::new(format!("❯ {text}"), LineKind::UserMessage));
            }
            for path in file_refs {
                out.push(StyledLine::new(format!("  @{path}"), LineKind::System));
            }
        }

        Role::Assistant => {
            let blocks: Vec<&ContentBlock> = match &msg.content {
                MessageContent::Text(t) => {
                    for line in t.lines() {
                        if !line.trim().is_empty() {
                            out.push(StyledLine::new(format!("  {line}"), LineKind::AgentText));
                        }
                    }
                    return;
                }
                MessageContent::MultiPart(b) => b.iter().collect(),
            };
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        for line in text.lines() {
                            if !line.trim().is_empty() {
                                out.push(StyledLine::new(format!("  {line}"), LineKind::AgentText));
                            }
                        }
                    }
                    ContentBlock::ToolUse { name, input, .. } => {
                        if name != "handoff_to_agent" {
                            let preview =
                                crate::transcript::format_tool_call_inline(name, input, 80);
                            out.push(StyledLine::new(format!("  {preview}"), LineKind::ToolCall));
                        }
                    }
                    _ => {}
                }
            }
        }

        Role::System | Role::Tool => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[StyledLine]) -> String {
        lines
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn banner_names_the_session_and_the_verb() {
        let line = banner("replayed", "abcdef0123456789");
        assert!(line.text.contains("replayed: abcdef01"), "{}", line.text);
        assert_eq!(line.kind, LineKind::Separator);
    }

    #[test]
    fn banner_tolerates_an_id_shorter_than_the_display_width() {
        let line = banner("resumed", "abc");
        assert!(line.text.contains("resumed: abc"), "{}", line.text);
        // The rule is padded, not truncated: nothing is lost.
        assert!(line.text.trim_end().ends_with('─'), "{}", line.text);
    }

    #[test]
    fn banner_does_not_split_a_multibyte_session_id() {
        // A byte-slice would panic here; the renderer counts chars.
        let line = banner("replayed", "αααααααααββββββββββββββββββββ");
        assert!(line.text.contains('α'));
    }

    #[test]
    fn user_and_assistant_turns_both_render() {
        let msgs = vec![
            Message::user("what did you do?"),
            Message::assistant("I fixed the parser"),
        ];
        let out = texts(&render_replay_lines(&msgs, "sess", "replayed", false));
        assert!(out.contains("❯ what did you do?"), "{out}");
        assert!(out.contains("I fixed the parser"), "{out}");
    }

    #[test]
    fn skip_user_drops_user_turns_but_keeps_assistant_ones() {
        let msgs = vec![
            Message::user("orchestrator-injected plan"),
            Message::assistant("agent reply"),
        ];
        let out = texts(&render_replay_lines(&msgs, "sess", "replayed", true));
        assert!(!out.contains("orchestrator-injected plan"), "{out}");
        assert!(out.contains("agent reply"), "{out}");
    }

    #[test]
    fn system_and_tool_turns_are_skipped() {
        let msgs = vec![Message::system("you are a helpful agent")];
        let lines = render_replay_lines(&msgs, "sess", "replayed", false);
        assert_eq!(lines.len(), 1, "only the banner survives: {lines:?}");
    }

    // ── Gap 4: bounded tail projection of the SESSION LOG ──────────────────

    /// A conversation of `n` user/assistant exchanges.
    fn exchange(n: usize) -> Vec<Message> {
        let mut out = Vec::new();
        for i in 0..n {
            out.push(Message::user(format!("question {i}")));
            out.push(Message::assistant(format!("answer {i}")));
        }
        out
    }

    fn tail_text(messages: &[Message], max_turns: usize) -> String {
        tail_turns(messages, max_turns)
            .iter()
            .flat_map(|m| match &m.content {
                MessageContent::Text(t) => vec![t.clone()],
                _ => vec![],
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn tail_keeps_the_newest_turns_only() {
        let msgs = exchange(5);
        assert_eq!(
            tail_text(&msgs, 2),
            "question 3|answer 3|question 4|answer 4"
        );
        assert_eq!(tail_text(&msgs, 1), "question 4|answer 4");
    }

    #[test]
    fn tail_keeps_everything_when_the_bound_exceeds_the_history() {
        let msgs = exchange(3);
        assert_eq!(tail_text(&msgs, 20), tail_text(&msgs, 0));
    }

    #[test]
    fn a_zero_bound_means_complete_history() {
        // agenthicc: `SessionEventLog.load(..., last_turns=replay_turns or None)`
        // and the bound applies only when `last_turns > 0`.
        let msgs = exchange(7);
        assert_eq!(tail_turns(&msgs, 0).len(), msgs.len());
    }

    #[test]
    fn a_turn_includes_the_trailing_assistant_reply() {
        // The bound counts USER turns, so a turn is never truncated mid-reply.
        let msgs = vec![
            Message::user("q0"),
            Message::assistant("a0"),
            Message::user("q1"),
            Message::assistant("a1"),
        ];
        assert_eq!(tail_text(&msgs, 1), "q1|a1");
    }

    #[test]
    fn a_history_with_no_user_turn_falls_back_to_the_newest_messages() {
        // Mirrors agenthicc's "newest bounded event suffix" fallback for logs
        // predating turn markers.
        let msgs = vec![
            Message::assistant("orphan-1"),
            Message::assistant("orphan-2"),
            Message::assistant("orphan-3"),
        ];
        assert_eq!(tail_text(&msgs, 2), "orphan-2|orphan-3");
    }

    #[test]
    fn projection_carries_the_loading_label_and_is_chunked() {
        let msgs = exchange(200);
        let t = project_resume_transcript(&msgs, "sess-abc", 200);
        assert_eq!(t.loading.spinner_line, LOADING_LABEL);
        assert!(t.loading.status_line.is_none());
        assert!(t.chunks.len() > 1, "200 turns must span several chunks");
        for c in &t.chunks {
            assert!(
                c.len() <= REPLAY_CHUNK_LINES,
                "chunk not bounded: {}",
                c.len()
            );
        }
        // Chunking must not drop or reorder a single line.
        let joined: Vec<String> = t.lines().iter().map(|l| l.text.clone()).collect();
        let expected = render_replay_lines(&msgs, "sess-abc", "resumed", false);
        assert_eq!(joined.len(), expected.len());
        for (a, b) in joined.iter().zip(expected.iter()) {
            assert_eq!(a, &b.text);
        }
    }

    #[test]
    fn an_empty_history_replays_nothing() {
        let t = project_resume_transcript(&[], "sess", 20);
        assert!(t.is_empty());
        assert!(t.lines().is_empty());
    }

    #[test]
    fn projection_honours_the_bound_and_uses_the_resumed_banner() {
        let msgs = exchange(4);
        let t = project_resume_transcript(&msgs, "abcdef0123456789", 1);
        let out = texts(&t.lines());
        assert!(out.contains("resumed: abcdef01"), "{out}");
        assert!(out.contains("question 3"), "{out}");
        assert!(!out.contains("question 0"), "{out}");
    }

    /// THE REGRESSION TEST for Gap 4.
    ///
    /// It seeds a session through the real `SessionManager::save_with_history`
    /// and asserts the projection shows those turns.
    ///
    /// Deliberately seeded with a `{session}::workflow::planner` key holding
    /// DIFFERENT text, and with more turns than the bound. Both are needed for
    /// the test to have teeth:
    ///
    /// * Seeding only the bare conversation key is NOT enough. The pre-fix path
    ///   tried `{session}::workflow::planner`, then `{session}::workflow::{agent}`,
    ///   then `{session}::workflow`, and finally the bare id — so a store with
    ///   nothing but the bare key was still found by that last fallback, and a
    ///   naive test would have been green against the broken code.
    /// * With a planner key present, the pre-fix path stops at the planner and
    ///   renders ITS text, so asserting the marker is absent genuinely fails.
    /// * The pre-fix path had no bound, so seeding more turns than `max_turns`
    ///   and asserting the oldest is absent genuinely fails there too.
    #[cfg(feature = "session")]
    #[tokio::test]
    async fn resume_projection_reads_the_session_log_not_memory_keys() {
        use agtrs_runtime::memory::ConversationStore as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = xaft_session::SessionManager::new(dir.path())
            .await
            .expect("session manager");
        let store = mgr.conversation_store();

        let session = xaft_runtime::session::AgentSession::new(
            "a task".to_string(),
            dir.path().to_path_buf(),
            "default".to_string(),
            "model".to_string(),
        );
        let msgs = exchange(30);
        mgr.save_with_history(&session, &msgs).await.expect("seed");

        // The in-memory key the orchestrator used to write. It must NOT win.
        let planner_key = format!("{}::workflow::planner", session.id.as_str());
        store
            .save(
                &planner_key,
                &[
                    Message::user("PLANNER-ONLY-MARKER"),
                    Message::assistant("plan text"),
                ],
            )
            .await
            .expect("seed planner key");

        let t =
            crate::replay::load_resume_transcript(store.as_ref(), session.id.as_str(), 20).await;
        let out = texts(&t.lines());

        // (1) It came from the SESSION LOG, not the planner memory key.
        assert!(
            !out.contains("PLANNER-ONLY-MARKER"),
            "--resume replayed the in-memory planner key instead of the session log: {out}"
        );
        assert!(
            out.contains("question 10"),
            "missing oldest kept turn: {out}"
        );
        assert!(out.contains("answer 29"), "missing newest turn: {out}");

        // (2) The bound holds: newest 20 turns only (turn 9 is the 21st from
        //     the end and must NOT appear).
        assert!(!out.contains("question 9"), "bound not applied: {out}");
        assert!(!out.contains("question 0"), "bound not applied: {out}");

        // (3) A bound of 1 keeps only the final turn.
        let one =
            crate::replay::load_resume_transcript(store.as_ref(), session.id.as_str(), 1).await;
        let one_out = texts(&one.lines());
        assert!(one_out.contains("question 29"), "{one_out}");
        assert!(!one_out.contains("question 28"), "{one_out}");
        assert!(!one_out.contains("PLANNER-ONLY-MARKER"), "{one_out}");
    }
}
