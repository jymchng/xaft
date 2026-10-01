//! Shared transcript-replay rendering.
//!
//! Two callers project stored `Message`s into the transcript, and they read
//! from **different** sources on purpose:
//!
//! * `app.rs` (`--resume`) replays the agent's **memory keys** —
//!   `{session}::workflow::planner`, `{session}::workflow::{agent}`. That is
//!   what Gap 4 in `knowledge/tui-parity-gap-analysis.md` is about, and it is
//!   deliberately left alone: fixing it is a separate item.
//! * `slash/commands/replay.rs` (`/replay`) replays the **session store's own
//!   history** — the conversation key `xaft_session::conversation_key_for`
//!   derives from the session id, which is the same store `xaft session
//!   inspect` reads.
//!
//! The sources differ; the rendering does not. Keeping it here stops the two
//! paths from drifting apart over time without touching either path's
//! *selection* of messages.

use agtrs_runtime::transport::{ContentBlock, Message, MessageContent, Role};

use crate::transcript::{LineKind, StyledLine};

/// How many characters of the session id a replay banner shows.
const BANNER_ID_LEN: usize = 8;

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
}
