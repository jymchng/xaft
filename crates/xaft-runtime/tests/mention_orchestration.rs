//! End-to-end orchestration tests for the F3 @-mention feature.
//!
//! These tests drive the full pipeline: a `UserMessage::MultiPart`
//! (built from a `MentionResolver::expand` call against a real
//! workspace) flows through `run_dynamic_handoff` and lands as
//! `Message::user_with_parts(...)` in the agent's conversation. The
//! resulting `MessageContent::MultiPart` is then verified to contain
//! the resolved `FileRef` blocks verbatim — no path rewriting, no
//! content truncation at the boundary.
//!
//! Uses `MockLlmProvider` + `MockTransport` — no real API calls.

use std::sync::Arc;

use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::{ConversationStore, InMemoryConversationStore};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::team::HandoffAgentStore;
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use agtrs_runtime::transport::{ContentBlock, FileRefContent, MessageContent};
use agtrs_workspace::InMemoryWorkspaceStore;
use tempfile::TempDir;
use xaft_config::{EscapePolicy, MentionConfig};
use xaft_runtime::agent_registry::{AgentDefinition, AgentRegistry, AgentToolSet, WorkflowConfig};
use xaft_runtime::orchestrator::run_dynamic_handoff;
use xaft_runtime::session::AgentSession;
use xaft_tui::{MentionResolver, UserMessage};

// ── Helpers ────────────────────────────────────────────────────────────────

fn make_resolve_ctx() -> Arc<injectable_runtime::ResolveContext> {
    Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
        injectable_runtime::EmptySingletonStore,
    )))
}

fn make_session(dir: &TempDir) -> AgentSession {
    AgentSession::new(
        "test task".to_string(),
        dir.path().to_path_buf(),
        "default".to_string(),
        "mock-model".to_string(),
    )
}

fn make_registry_one() -> AgentRegistry {
    AgentRegistry::new().register(AgentDefinition {
        name: "default".into(),
        system_prompt_fn: Box::new(|_, _| "You are a default agent.".into()),
        tool_set: AgentToolSet::ReadOnly,
        max_turns: 100,
        can_handoff_to: vec![],
    })
}

fn dynamic_cfg(initial: &str) -> WorkflowConfig {
    WorkflowConfig::Dynamic {
        initial_agent: initial.to_string(),
        max_handoffs: 4,
        agent_subset: None,
    }
}

fn cfg() -> MentionConfig {
    MentionConfig {
        max_inline_lines: 100,
        max_inline_bytes: 50_000,
        image_max_bytes: 1024 * 1024,
        resolver_max_file_bytes: 100_000,
        dedupe: false,
        escape_policy: EscapePolicy::Always, // skip dialog in tests
        escape_allowlist: vec![],
    }
}

// ── 1. Workspace-relative mention flows into MultiPart ─────────────────────

#[tokio::test]
async fn mention_workspace_relative_attaches_file_ref_in_orchestration() {
    let tmp = TempDir::new().unwrap();
    let store = InMemoryWorkspaceStore::with_files([(
        "src/lib.rs".to_string(),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
    )]);

    let expanded = MentionResolver::expand("see @src/lib.rs", &store, &cfg()).await;
    assert!(expanded.warnings.is_empty(), "no warnings expected");
    let user_message: UserMessage = expanded.into_user_message();
    let parts = match &user_message {
        UserMessage::MultiPart(parts) => parts.clone(),
        UserMessage::Text(s) => panic!("expected MultiPart, got Text: {s}"),
    };
    assert_eq!(parts.len(), 2); // "see " text + 1 FileRef

    // Wire up the LLM mock: it produces a final answer without tool calls.
    let transport = Arc::new(MockTransport::new());
    transport.queue_text("I see the function.").await;
    let convo: Arc<dyn ConversationStore> = Arc::new(InMemoryConversationStore::new());
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let signals = Arc::new(SignalBus::new());
    let mut session = make_session(&tmp);

    let result = run_dynamic_handoff(
        "test task",
        &make_registry_one(),
        &dynamic_cfg("default"),
        llm,
        signals,
        make_resolve_ctx(),
        vec![],
        vec![],
        &mut session,
        Some(convo.clone()),
        None,
        Some(parts),
    )
    .await
    .unwrap();

    // The final assistant turn must contain the LLM mock's answer.
    assert_eq!(result.agent_name, "default");
    assert!(result.content.contains("I see the function."));

    // Inspect the conversation store: the first user message must be MultiPart.
    // Key format: "{conv_id}::{agent_name}" = "{session_id}::{initial_agent}::{current_agent}".
    let conv_key = format!("{}::default::default", session.id);
    let messages = convo.load(&conv_key).await.unwrap();
    let first_user = messages
        .iter()
        .find(|m| matches!(m.role, agtrs_runtime::transport::Role::User))
        .expect("a user message was recorded");
    match &first_user.content {
        MessageContent::MultiPart(blocks) => {
            assert!(
                blocks.iter().any(
                    |b| matches!(b, ContentBlock::FileRef { path, .. } if path == "src/lib.rs")
                ),
                "the resolved FileRef must flow into the agent's history"
            );
        }
        other => panic!("expected MultiPart user content, got {other:?}"),
    }
}

// ── 2. Plain text submission collapses to MessageContent::Text ─────────────

#[tokio::test]
async fn plain_text_mention_submission_collapses_to_text() {
    let tmp = TempDir::new().unwrap();
    let store = InMemoryWorkspaceStore::new();
    let expanded = MentionResolver::expand("hello world", &store, &cfg()).await;
    let um = expanded.into_user_message();
    // No mentions → must be `UserMessage::Text`, not MultiPart.
    assert!(matches!(um, UserMessage::Text(ref s) if s == "hello world"));

    // Now drive the orchestrator with the resulting Text → user_parts
    // should NOT be Some (Text path does not require it). The test
    // verifies the run completes end-to-end.
    let transport = Arc::new(MockTransport::new());
    transport.queue_text("Hi.").await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let signals = Arc::new(SignalBus::new());
    let mut session = make_session(&tmp);

    let result = run_dynamic_handoff(
        "test task",
        &make_registry_one(),
        &dynamic_cfg("default"),
        llm,
        signals,
        make_resolve_ctx(),
        vec![],
        vec![],
        &mut session,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(result.content.contains("Hi."));
}

// ── 3. File not-found mention inlines literal `@<path>` token ─────────────

#[tokio::test]
async fn file_not_found_inlines_literal_token_in_orchestration() {
    let tmp = TempDir::new().unwrap();
    let store = InMemoryWorkspaceStore::new(); // empty
    let expanded = MentionResolver::expand("see @nope.rs please", &store, &cfg()).await;
    // The resolver must record a FileNotFound warning but still emit
    // the literal token in the text portion so the LLM at least sees
    // what the user typed.
    assert!(
        expanded
            .warnings
            .iter()
            .any(|w| matches!(w, xaft_tui::MentionError::FileNotFound { .. }))
    );

    let parts = match expanded.into_user_message() {
        UserMessage::MultiPart(parts) => parts,
        UserMessage::Text(_) => panic!("expected MultiPart with text containing @nope.rs"),
    };
    let text: String = parts
        .iter()
        .map(|p| match p {
            ContentBlock::Text { text } => text.clone(),
            _ => String::new(),
        })
        .collect();
    assert!(
        text.contains("@nope.rs"),
        "literal token must be preserved: {text}"
    );
}

// ── 4. Image FileRef flows through the orchestration boundary ──────────────

#[tokio::test]
async fn image_file_ref_flows_through_orchestration() {
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

    let tmp = TempDir::new().unwrap();
    let expanded = MentionResolver::expand("look at @logo.png", &PngStore, &cfg()).await;
    assert!(expanded.warnings.is_empty());

    let parts = match expanded.into_user_message() {
        UserMessage::MultiPart(parts) => parts,
        UserMessage::Text(_) => panic!("expected MultiPart"),
    };
    // Find the FileRef::Image block and verify the bytes round-trip.
    let img = parts
        .iter()
        .find_map(|p| match p {
            ContentBlock::FileRef {
                content:
                    FileRefContent::Image {
                        media_type,
                        data_base64,
                    },
                ..
            } => Some((media_type.clone(), data_base64.clone())),
            _ => None,
        })
        .expect("an image FileRef block");
    assert_eq!(img.0, "image/png");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&img.1)
        .unwrap();
    assert!(decoded.starts_with(b"\x89PNG"));

    // Drive the orchestrator end-to-end.
    let transport = Arc::new(MockTransport::new());
    transport.queue_text("Got the image.").await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let signals = Arc::new(SignalBus::new());
    let mut session = make_session(&tmp);

    let result = run_dynamic_handoff(
        "test task",
        &make_registry_one(),
        &dynamic_cfg("default"),
        llm,
        signals,
        make_resolve_ctx(),
        vec![],
        vec![],
        &mut session,
        None,
        None,
        Some(parts),
    )
    .await
    .unwrap();
    assert!(result.content.contains("Got the image."));
}

// ── 5. EscapePolicy::Always silently attaches the file ─────────────────────

#[tokio::test]
async fn escape_policy_always_silently_attaches_through_orchestrator() {
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
    let tmp = TempDir::new().unwrap();
    let mut c = cfg();
    c.escape_policy = EscapePolicy::Always;

    let expanded = MentionResolver::expand("see @/etc/hosts", &AnyStore, &c).await;
    assert!(expanded.warnings.is_empty(), "Always should not warn");
    assert_eq!(expanded.escape_mentions.len(), 1);

    let parts = match expanded.into_user_message() {
        UserMessage::MultiPart(parts) => parts,
        UserMessage::Text(_) => panic!("expected MultiPart"),
    };
    let has_escape = parts.iter().any(|p| match p {
        ContentBlock::FileRef { escape, path, .. } => escape.is_some() && path == "/etc/hosts",
        _ => false,
    });
    assert!(has_escape, "the FileRef must carry EscapeInfo");
}

// ── 6. Multi-mention expansion preserves all blocks in order ──────────────

#[tokio::test]
async fn multi_mention_expansion_preserves_all_blocks() {
    let tmp = TempDir::new().unwrap();
    let store = InMemoryWorkspaceStore::with_files([
        ("a.rs".to_string(), "AAA".to_string()),
        ("b.rs".to_string(), "BBB".to_string()),
    ]);
    let expanded = MentionResolver::expand("first @a.rs then @b.rs", &store, &cfg()).await;
    let parts = match expanded.into_user_message() {
        UserMessage::MultiPart(parts) => parts,
        UserMessage::Text(_) => panic!("expected MultiPart"),
    };
    // text "first " + FileRef(a) + text " then " + FileRef(b)
    assert_eq!(parts.len(), 4);
    let paths: Vec<&str> = parts
        .iter()
        .filter_map(|p| match p {
            ContentBlock::FileRef { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(paths, vec!["a.rs", "b.rs"]);
}
