//! F3 @-mention integration tests for the TUI.
//!
//! Tests the submit-time resolution flow: a user message with
//! `@<path>` mentions is expanded against a workspace, the resulting
//! `UserMessage::MultiPart` is verified to contain resolved `FileRef`
//! blocks, and warnings appear in the right place when resolution
//! fails.
//!
//! The escape-confirmation flow (dialog open/close, signal emission,
//! session-wide approval) is tested in
//! `crates/xaft-tui/tests/escape_confirmation.rs`.
//!
//! These tests are integration-level in that they exercise the full
//! `MentionResolver` + `UserMessage` pipeline through the public API
//! of `xaft_tui`. They do **not** require a real terminal — the
//! resolution is async and the test drives it directly.

use agtrs_runtime::transport::{ContentBlock, FileRefContent};
use agtrs_workspace::InMemoryWorkspaceStore;
use xaft_config::{EscapePolicy, MentionConfig};
use xaft_tui::{EscapeReason, MentionError, MentionResolver, UserMessage};

// ── find_mentions coverage ──────────────────────────────────────────────────

#[test]
fn find_mentions_finds_simple_mention() {
    let toks = MentionResolver::find_mentions("see @src/lib.rs");
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].path, "src/lib.rs");
}

#[test]
fn find_mentions_classifies_workspace_relative() {
    let toks = MentionResolver::find_mentions("@src/lib.rs");
    assert!(!toks[0].is_escape);
    assert!(toks[0].escape_reason.is_none());
}

#[test]
fn find_mentions_classifies_parent_traversal() {
    let toks = MentionResolver::find_mentions("@../sibling/foo.rs");
    assert!(toks[0].is_escape);
    assert_eq!(toks[0].escape_reason, Some(EscapeReason::ParentTraversal));
    assert_eq!(toks[0].escape_depth, 1);
}

#[test]
fn find_mentions_classifies_absolute() {
    let toks = MentionResolver::find_mentions("@/etc/hosts");
    assert!(toks[0].is_escape);
    assert_eq!(toks[0].escape_reason, Some(EscapeReason::Absolute));
    assert_eq!(toks[0].escape_depth, 0);
}

#[test]
fn find_mentions_classifies_home_expansion() {
    let toks = MentionResolver::find_mentions("@~/notes.md");
    assert!(toks[0].is_escape);
    assert_eq!(toks[0].escape_reason, Some(EscapeReason::HomeExpansion));
}

// ── expand() coverage ───────────────────────────────────────────────────────

fn cfg() -> MentionConfig {
    MentionConfig {
        max_inline_lines: 100,
        max_inline_bytes: 50_000,
        image_max_bytes: 1024 * 1024,
        resolver_max_file_bytes: 100_000,
        dedupe: false,
        escape_policy: EscapePolicy::Confirm,
        escape_allowlist: vec![],
    }
}

#[tokio::test]
async fn expand_workspace_relative_resolves_to_file_ref() {
    let store = InMemoryWorkspaceStore::with_files([(
        "src/lib.rs".to_string(),
        "pub fn add(a:i32,b:i32)->i32{a+b}".to_string(),
    )]);
    let expanded = MentionResolver::expand("see @src/lib.rs", &store, &cfg()).await;
    assert!(expanded.warnings.is_empty());
    assert!(expanded.tokens.len() == 1);
    assert_eq!(expanded.parts.len(), 2); // text + FileRef

    let um = expanded.into_user_message();
    assert!(matches!(um, UserMessage::MultiPart(_)));
    let msg = um.into_message();
    match msg.content {
        agtrs_runtime::transport::MessageContent::MultiPart(blocks) => {
            assert_eq!(blocks.len(), 2);
            match &blocks[1] {
                ContentBlock::FileRef { path, escape, .. } => {
                    assert_eq!(path, "src/lib.rs");
                    assert!(escape.is_none());
                }
                _ => panic!("expected FileRef"),
            }
        }
        other => panic!("expected MultiPart, got {other:?}"),
    }
}

#[tokio::test]
async fn expand_text_only_collapses_to_user_message_text() {
    let store = InMemoryWorkspaceStore::new();
    let expanded = MentionResolver::expand("hello world", &store, &cfg()).await;
    let um = expanded.into_user_message();
    assert!(matches!(um, UserMessage::Text(ref s) if s == "hello world"));
}

#[tokio::test]
async fn expand_file_not_found_inlines_literal_token() {
    let store = InMemoryWorkspaceStore::new();
    let expanded = MentionResolver::expand("see @nope.rs", &store, &cfg()).await;
    assert!(
        expanded
            .warnings
            .iter()
            .any(|w| matches!(w, MentionError::FileNotFound { .. }))
    );
    let um = expanded.into_user_message();
    match um {
        UserMessage::MultiPart(parts) => {
            let text: String = parts
                .iter()
                .map(|p| match p {
                    ContentBlock::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect();
            assert!(text.contains("@nope.rs"), "literal token preserved: {text}");
        }
        other => panic!("expected MultiPart, got {other:?}"),
    }
}

#[tokio::test]
async fn expand_escape_records_escape_info() {
    // Use a custom store that returns content for any path.
    struct AnyStore;
    #[async_trait::async_trait]
    impl agtrs_workspace::WorkspaceStore for AnyStore {
        async fn write(&self, _: &str, _: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            Ok(())
        }
        async fn read(&self, _: &str) -> Result<String, agtrs_runtime::error::AgtrsError> {
            Ok("hi".into())
        }
        async fn exists(&self, _: &str) -> bool {
            true
        }
        async fn list(&self) -> Vec<String> {
            vec![]
        }
        async fn delete(&self, _: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            Ok(())
        }
        async fn read_all(&self) -> std::collections::HashMap<String, String> {
            Default::default()
        }
        fn root_display(&self) -> String {
            "<any>".into()
        }
        async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, agtrs_runtime::error::AgtrsError> {
            Ok(b"hi".to_vec())
        }
        async fn head_bytes(
            &self,
            _: &str,
            _: usize,
        ) -> Result<(Vec<u8>, bool), agtrs_runtime::error::AgtrsError> {
            Ok((b"hi".to_vec(), false))
        }
    }
    let store = AnyStore;
    let expanded = MentionResolver::expand("see @/etc/hosts", &store, &cfg()).await;
    assert!(expanded.warnings.is_empty());
    assert_eq!(expanded.escape_mentions.len(), 1);
    assert_eq!(expanded.escape_mentions[0].reason, EscapeReason::Absolute);
}

#[tokio::test]
async fn expand_escape_policy_never_rejects() {
    let store = InMemoryWorkspaceStore::new();
    let mut c = cfg();
    c.escape_policy = EscapePolicy::Never;
    let expanded = MentionResolver::expand("see @../foo.rs", &store, &c).await;
    assert!(
        expanded
            .warnings
            .iter()
            .any(|w| matches!(w, MentionError::EscapeRejected { .. }))
    );
    assert!(expanded.escape_mentions.is_empty());
}

#[tokio::test]
async fn expand_escape_policy_always_silently_attaches() {
    struct AnyStore;
    #[async_trait::async_trait]
    impl agtrs_workspace::WorkspaceStore for AnyStore {
        async fn write(&self, _: &str, _: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            Ok(())
        }
        async fn read(&self, _: &str) -> Result<String, agtrs_runtime::error::AgtrsError> {
            Ok("x".into())
        }
        async fn exists(&self, _: &str) -> bool {
            true
        }
        async fn list(&self) -> Vec<String> {
            vec![]
        }
        async fn delete(&self, _: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            Ok(())
        }
        async fn read_all(&self) -> std::collections::HashMap<String, String> {
            Default::default()
        }
        fn root_display(&self) -> String {
            "<any>".into()
        }
        async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, agtrs_runtime::error::AgtrsError> {
            Ok(b"x".to_vec())
        }
        async fn head_bytes(
            &self,
            _: &str,
            _: usize,
        ) -> Result<(Vec<u8>, bool), agtrs_runtime::error::AgtrsError> {
            Ok((b"x".to_vec(), false))
        }
    }
    let store = AnyStore;
    let mut c = cfg();
    c.escape_policy = EscapePolicy::Always;
    let expanded = MentionResolver::expand("@/etc/hosts", &store, &c).await;
    assert!(expanded.warnings.is_empty());
    assert_eq!(expanded.escape_mentions.len(), 1);
}

#[tokio::test]
async fn expand_image_detection_routes_to_image_block() {
    use base64::Engine;
    struct PngStore;
    #[async_trait::async_trait]
    impl agtrs_workspace::WorkspaceStore for PngStore {
        async fn write(&self, _: &str, _: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            Ok(())
        }
        async fn read(&self, _: &str) -> Result<String, agtrs_runtime::error::AgtrsError> {
            Ok(String::new())
        }
        async fn exists(&self, _: &str) -> bool {
            true
        }
        async fn list(&self) -> Vec<String> {
            vec![]
        }
        async fn delete(&self, _: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            Ok(())
        }
        async fn read_all(&self) -> std::collections::HashMap<String, String> {
            Default::default()
        }
        fn root_display(&self) -> String {
            "<png>".into()
        }
        async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, agtrs_runtime::error::AgtrsError> {
            Ok(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec())
        }
        async fn head_bytes(
            &self,
            _: &str,
            _: usize,
        ) -> Result<(Vec<u8>, bool), agtrs_runtime::error::AgtrsError> {
            Ok((b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec(), false))
        }
    }
    let expanded = MentionResolver::expand("@logo.png", &PngStore, &cfg()).await;
    if let ContentBlock::FileRef {
        content: FileRefContent::Image {
            media_type,
            data_base64,
        },
        ..
    } = &expanded.parts[0]
    {
        assert_eq!(media_type, "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .unwrap();
        assert!(decoded.starts_with(b"\x89PNG"));
    } else {
        panic!("expected FileRef::Image");
    }
}

// ── UserMessage round-trips ────────────────────────────────────────────────

#[test]
fn user_message_text_into_message_round_trip() {
    let m = UserMessage::Text("hello".into()).into_message();
    assert!(matches!(
        m.content,
        agtrs_runtime::transport::MessageContent::Text(ref s) if s == "hello"
    ));
}

#[test]
fn user_message_multipart_into_message_round_trip() {
    // Single Text part → collapses to MessageContent::Text
    let m = UserMessage::MultiPart(vec![ContentBlock::Text { text: "x".into() }]).into_message();
    assert!(matches!(
        m.content,
        agtrs_runtime::transport::MessageContent::Text(ref s) if s == "x"
    ));
    // Multi-part (text + FileRef) → stays MultiPart
    let m = UserMessage::MultiPart(vec![
        ContentBlock::Text {
            text: "see ".into(),
        },
        ContentBlock::FileRef {
            path: "src/lib.rs".into(),
            content: FileRefContent::Text("...".into()),
            truncation: None,
            byte_size: 3,
            line_count: 1,
            sha256: "abc".into(),
            escape: None,
        },
    ])
    .into_message();
    assert!(matches!(
        m.content,
        agtrs_runtime::transport::MessageContent::MultiPart(_)
    ));
}

#[test]
fn user_message_as_text_lossy_substitutes_at_path() {
    let m = UserMessage::MultiPart(vec![
        ContentBlock::Text {
            text: "see ".into(),
        },
        ContentBlock::FileRef {
            path: "src/lib.rs".into(),
            content: FileRefContent::Text("…".into()),
            truncation: None,
            byte_size: 1,
            line_count: 1,
            sha256: String::new(),
            escape: None,
        },
    ]);
    assert_eq!(m.as_text_lossy(), "see @src/lib.rs");
}
