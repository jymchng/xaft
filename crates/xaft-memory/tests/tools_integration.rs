//! Integration tests for xaft-memory tools.
//!
//! Tests the full flow: memory manager → tools → agent execution.

use std::sync::Arc;

use agtrs_runtime::tool::{Tool, ToolContext};
use xaft_memory::config::{MemoryBackend, MemoryConfig};
use xaft_memory::manager::XaftMemoryManager;
use xaft_memory::tools::{ForgetTool, RecallTool, RememberTool, SummarizeMemoryTool};

fn default_config() -> MemoryConfig {
    MemoryConfig {
        enabled: true,
        backend: MemoryBackend::InMemory,
        ..Default::default()
    }
}

fn make_tool_ctx() -> ToolContext {
    ToolContext::new("test-tool-use-id")
}

// ── RememberTool ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn remember_tool_stores_fact() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tool = RememberTool::new(Arc::clone(&mgr));

    let input = serde_json::json!({
        "content": "The auth service uses JWT tokens",
        "tags": ["architecture", "auth"]
    });

    let result = tool.call(input, &make_tool_ctx()).await.unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("Remembered"));
    assert!(result.content.contains("JWT"));
}

#[tokio::test]
async fn remember_tool_rejects_empty_content() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tool = RememberTool::new(Arc::clone(&mgr));

    let input = serde_json::json!({
        "content": ""
    });

    let result = tool.call(input, &make_tool_ctx()).await.unwrap();
    assert!(result.is_error);
}

#[tokio::test]
async fn remember_tool_rejects_missing_content() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tool = RememberTool::new(Arc::clone(&mgr));

    let input = serde_json::json!({});
    let result = tool.call(input, &make_tool_ctx()).await;
    assert!(result.is_err());
}

// ── RecallTool ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn recall_tool_finds_stored_fact() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    mgr.remember("The auth service uses JWT tokens", &["auth"])
        .await
        .unwrap();

    let tool = RecallTool::new(Arc::clone(&mgr));
    let input = serde_json::json!({"query": "auth service"});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("JWT"));
}

#[tokio::test]
async fn recall_tool_returns_no_results_for_missing() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tool = RecallTool::new(Arc::clone(&mgr));

    let input = serde_json::json!({"query": "nonexistent topic"});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("No matching"));
}

// ── ForgetTool ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_tool_deletes_stored_fact() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let id = mgr.remember("temporary fact", &[]).await.unwrap();

    let tool = ForgetTool::new(Arc::clone(&mgr));
    let input = serde_json::json!({"id": id.to_string()});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("Forgot"));

    // Verify it's gone
    let results = mgr.recall("temporary fact").await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn forget_tool_reports_missing() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tool = ForgetTool::new(Arc::clone(&mgr));

    let input = serde_json::json!({"id": "nonexistent-id"});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("not found"));
}

// ── SummarizeMemoryTool ──────────────────────────────────────────────────────

#[tokio::test]
async fn summarize_tool_lists_all_memories() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    mgr.remember("Fact A", &["arch"]).await.unwrap();
    mgr.remember("Fact B", &["testing"]).await.unwrap();
    mgr.remember("Fact C", &["arch"]).await.unwrap();

    let tool = SummarizeMemoryTool::new(Arc::clone(&mgr));
    let input = serde_json::json!({});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("3 entries"));
    assert!(result.content.contains("Fact A"));
    assert!(result.content.contains("Fact B"));
    assert!(result.content.contains("Fact C"));
}

#[tokio::test]
async fn summarize_tool_filters_by_tags() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    mgr.remember("Fact A", &["arch"]).await.unwrap();
    mgr.remember("Fact B", &["testing"]).await.unwrap();

    let tool = SummarizeMemoryTool::new(Arc::clone(&mgr));
    let input = serde_json::json!({"tags": ["arch"]});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("1 entries"));
    assert!(result.content.contains("Fact A"));
    assert!(!result.content.contains("Fact B"));
}

#[tokio::test]
async fn summarize_tool_empty_store() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tool = SummarizeMemoryTool::new(Arc::clone(&mgr));

    let input = serde_json::json!({});
    let result = tool.call(input, &make_tool_ctx()).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("No memories"));
}

// ── Tool name/description ─────────────────────────────────────────────────────

#[tokio::test]
async fn tool_names_are_correct() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    assert_eq!(RememberTool::new(Arc::clone(&mgr)).name(), "remember");
    assert_eq!(RecallTool::new(Arc::clone(&mgr)).name(), "recall");
    assert_eq!(ForgetTool::new(Arc::clone(&mgr)).name(), "forget");
    assert_eq!(
        SummarizeMemoryTool::new(Arc::clone(&mgr)).name(),
        "summarize_memory"
    );
}

#[tokio::test]
async fn tool_schemas_are_valid_json() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let tools: Vec<
        Box<dyn Tool<Inputs = serde_json::Value, Output = agtrs_runtime::tool::ToolResult>>,
    > = vec![
        Box::new(RememberTool::new(Arc::clone(&mgr))),
        Box::new(RecallTool::new(Arc::clone(&mgr))),
        Box::new(ForgetTool::new(Arc::clone(&mgr))),
        Box::new(SummarizeMemoryTool::new(Arc::clone(&mgr))),
    ];

    for tool in tools {
        let schema = tool.schema();
        assert!(schema.is_object(), "schema must be a JSON object");
        assert!(
            schema.get("type").is_some(),
            "schema must have a 'type' field"
        );
    }
}

// ── MemoryToolset ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn memory_toolset_all_has_four_tools() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let toolset = xaft_memory::tools::memory_toolset(mgr);
    assert_eq!(toolset.all().len(), 4);
}

#[tokio::test]
async fn memory_toolset_read_only_has_one_tool() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let toolset = xaft_memory::tools::memory_toolset(mgr);
    assert_eq!(toolset.read_only().len(), 1);
    assert_eq!(toolset.read_only()[0].name(), "recall");
}

// ── Full workflow ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn full_workflow_remember_recall_forget() {
    let mgr = Arc::new(
        XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap(),
    );
    let ctx = make_tool_ctx();

    // Remember
    let remember = RememberTool::new(Arc::clone(&mgr));
    let result = remember
        .call(
            serde_json::json!({"content": "Database uses connection pooling", "tags": ["db"]}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!result.is_error);

    // Recall
    let recall = RecallTool::new(Arc::clone(&mgr));
    let result = recall
        .call(serde_json::json!({"query": "database connection"}), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("connection pooling"));

    // Summarize
    let summarize = SummarizeMemoryTool::new(Arc::clone(&mgr));
    let result = summarize.call(serde_json::json!({}), &ctx).await.unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("1 entries"));

    // Forget - use manager directly for the ID
    let results = mgr.recall("database connection").await.unwrap();
    let id = &results[0].entry.id;

    let forget = ForgetTool::new(Arc::clone(&mgr));
    let result = forget
        .call(serde_json::json!({"id": id.to_string()}), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);

    // Verify gone
    let results = mgr.recall("database").await.unwrap();
    assert!(results.is_empty());
}

// ── Disabled memory ───────────────────────────────────────────────────────────

#[tokio::test]
async fn tools_fail_when_memory_disabled() {
    let config = MemoryConfig {
        enabled: false,
        ..default_config()
    };
    let mgr = Arc::new(XaftMemoryManager::in_memory(config).await.unwrap());
    let ctx = make_tool_ctx();

    let remember = RememberTool::new(Arc::clone(&mgr));
    let result = remember
        .call(serde_json::json!({"content": "test"}), &ctx)
        .await;
    assert!(result.is_err());

    let recall = RecallTool::new(Arc::clone(&mgr));
    let result = recall
        .call(serde_json::json!({"query": "test"}), &ctx)
        .await;
    assert!(result.is_err());
}
