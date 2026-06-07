//! F3 `UserMessage` — typed envelope for messages sent from the TUI to the
//! runtime.
//!
//! Replaces the previous `UnboundedSender<String>` channel on
//! `AppState::user_message_tx`. The text-only path is preserved as
//! `UserMessage::Text(String)`; messages that contain resolved
//! `@<path>` mentions use `UserMessage::MultiPart(Vec<ContentBlock>)`.
//!
//! The type itself lives in `xaft_runtime::UserMessage` so the runtime
//! boundary type `RunRequest::user_message` can carry it without a
//! `xaft-tui` → `xaft-runtime` dependency. This module re-exports the
//! canonical definition.

pub use xaft_runtime::UserMessage;

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::transport::{ContentBlock, FileRefContent, MessageContent};

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
                content: FileRefContent::Text("…".into()),
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
                content: FileRefContent::Text("y".into()),
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
                content: FileRefContent::Text("b".into()),
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
                content: FileRefContent::Text(String::new()),
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
