//! `UserMessage` — typed envelope for messages sent from the TUI to the runtime.
//!
//! Replaces the previous `UnboundedSender<String>` channel on
//! `AppState::user_message_tx`. The text-only path is preserved as
//! `UserMessage::Text(String)`; messages that contain resolved
//! `@<path>` mentions use `UserMessage::MultiPart(Vec<ContentBlock>)`.
//!
//! The runtime receives this envelope and decides whether to construct
//! a plain `Message::user(task)` or a `Message::user_with_parts(parts)`
//! first user message.

use agtrs_runtime::transport::{ContentBlock, Message, MessageContent};

/// A message sent from the TUI input bar to the runtime.
#[derive(Debug, Clone)]
pub enum UserMessage {
    /// A plain text message (no mention resolution needed). Equivalent to
    /// the old `String` channel payload.
    Text(String),
    /// A structured message carrying one or more resolved `@<path>`
    /// `FileRef` blocks (and any interleaving text).
    MultiPart(Vec<ContentBlock>),
}

// We don't derive PartialEq because ContentBlock::ToolUse holds
// serde_json::Value (no Eq). Tests in this module compare by
// destructuring instead.

impl UserMessage {
    /// Build a `UserMessage` from a list of content blocks. Collapses to
    /// `Text` if `parts` is empty or a single `Text` block.
    pub fn from_parts(parts: Vec<ContentBlock>) -> Self {
        match MessageContent::from_parts(parts) {
            MessageContent::Text(s) => UserMessage::Text(s),
            MessageContent::MultiPart(blocks) => UserMessage::MultiPart(blocks),
        }
    }

    /// Lossy string view of the message. Used for transcript rendering and
    /// for the literal-text fallback when a mention is unresolved.
    pub fn as_text_lossy(&self) -> String {
        match self {
            UserMessage::Text(s) => s.clone(),
            UserMessage::MultiPart(blocks) => blocks
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.clone(),
                    ContentBlock::FileRef { path, .. } => format!("@{path}"),
                    ContentBlock::Image { .. } => "[image]".to_string(),
                    ContentBlock::ToolUse { name, .. } => format!("[tool_use:{name}]"),
                    ContentBlock::ToolResult { content, .. } => content.clone(),
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// Convert this `UserMessage` into an `agtrs_runtime::transport::Message`
    /// suitable for sending to the LLM. For `Text`, produces a
    /// `MessageContent::Text`; for `MultiPart`, produces a
    /// `MessageContent::MultiPart`.
    pub fn into_message(self) -> Message {
        match self {
            UserMessage::Text(s) => Message::user(s),
            UserMessage::MultiPart(parts) => Message::user_with_parts(parts),
        }
    }

    /// Returns `true` when the message contains at least one `FileRef`
    /// block (i.e. at least one mention was successfully resolved).
    pub fn has_file_refs(&self) -> bool {
        matches!(self, UserMessage::MultiPart(parts) if parts.iter().any(|b| matches!(b, ContentBlock::FileRef { .. })))
    }
}

impl From<String> for UserMessage {
    fn from(s: String) -> Self {
        UserMessage::Text(s)
    }
}

impl From<&str> for UserMessage {
    fn from(s: &str) -> Self {
        UserMessage::Text(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_variant_lossy() {
        let m = UserMessage::Text("hello".into());
        assert_eq!(m.as_text_lossy(), "hello");
    }

    #[test]
    fn multipart_lossy_interleaves_text() {
        let m = UserMessage::MultiPart(vec![
            ContentBlock::Text {
                text: "see ".into(),
            },
            ContentBlock::FileRef {
                path: "src/lib.rs".into(),
                content: agtrs_runtime::transport::FileRefContent::Text("…".into()),
                truncation: None,
                byte_size: 100,
                line_count: 5,
                sha256: "deadbeef".into(),
                escape: None,
            },
            ContentBlock::Text {
                text: " please".into(),
            },
        ]);
        assert_eq!(m.as_text_lossy(), "see @src/lib.rs please");
    }

    #[test]
    fn from_parts_collapses_single_text() {
        let m = UserMessage::from_parts(vec![ContentBlock::Text { text: "hi".into() }]);
        assert!(matches!(m, UserMessage::Text(ref s) if s == "hi"));
    }

    #[test]
    fn from_parts_collapses_empty() {
        let m = UserMessage::from_parts(vec![]);
        assert!(matches!(m, UserMessage::Text(ref s) if s.is_empty()));
    }

    #[test]
    fn from_parts_keeps_multipart() {
        let m = UserMessage::from_parts(vec![
            ContentBlock::Text { text: "a".into() },
            ContentBlock::FileRef {
                path: "x.rs".into(),
                content: agtrs_runtime::transport::FileRefContent::Text("y".into()),
                truncation: None,
                byte_size: 1,
                line_count: 1,
                sha256: String::new(),
                escape: None,
            },
        ]);
        assert!(matches!(m, UserMessage::MultiPart(_)));
    }

    #[test]
    fn into_message_text() {
        let msg = UserMessage::Text("hello".into()).into_message();
        assert!(matches!(msg.content, MessageContent::Text(ref s) if s == "hello"));
    }

    #[test]
    fn into_message_multipart() {
        let msg = UserMessage::MultiPart(vec![
            ContentBlock::Text { text: "a".into() },
            ContentBlock::FileRef {
                path: "x".into(),
                content: agtrs_runtime::transport::FileRefContent::Text("b".into()),
                truncation: None,
                byte_size: 1,
                line_count: 1,
                sha256: String::new(),
                escape: None,
            },
        ])
        .into_message();
        match msg.content {
            MessageContent::MultiPart(parts) => assert_eq!(parts.len(), 2),
            other => panic!("expected MultiPart, got {other:?}"),
        }
    }

    #[test]
    fn has_file_refs_true() {
        let m = UserMessage::MultiPart(vec![
            ContentBlock::Text {
                text: "see ".into(),
            },
            ContentBlock::FileRef {
                path: "x.rs".into(),
                content: agtrs_runtime::transport::FileRefContent::Text(String::new()),
                truncation: None,
                byte_size: 0,
                line_count: 0,
                sha256: String::new(),
                escape: None,
            },
        ]);
        assert!(m.has_file_refs());
    }

    #[test]
    fn has_file_refs_false_for_text_only() {
        assert!(!UserMessage::Text("plain".into()).has_file_refs());
    }

    #[test]
    fn from_string_and_str() {
        let _: UserMessage = String::from("x").into();
        let _: UserMessage = "y".into();
    }
}
