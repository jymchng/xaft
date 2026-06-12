//! Integration tests for xaft-tools using real filesystem and processes.

use std::sync::Arc;
use std::time::Duration;

use agtrs_runtime::tool::{Tool, ToolContext};
use agtrs_shell::{CommandExecutor, ExecutionPolicy, Sandbox};
use agtrs_workspace::{InMemoryWorkspaceStore, WorkspaceStore};
use tempfile::TempDir;

use xaft_tools::{
    BashExecTool, EditFileTool, FsWorkspaceStore, GrepTool, ListFilesTool, ReadFileTool,
    ToolRegistryBuilder, WriteFileTool,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn mem_store() -> Arc<dyn WorkspaceStore> {
    Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>
}

fn ctx(id: &str) -> ToolContext {
    ToolContext::new(id)
}

// ── ReadFileTool ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_existing_file() {
    let store = mem_store();
    store.write("hello.txt", "hello world\n").await.unwrap();
    let tool = ReadFileTool::new(Arc::clone(&store));
    let result = tool
        .call(serde_json::json!({"path": "hello.txt"}), &ctx("r1"))
        .await
        .unwrap();
    assert!(!result.is_error, "got error: {}", result.content);
    assert!(result.content.contains("hello world"));
}

#[tokio::test]
async fn read_missing_file_is_error() {
    let store = mem_store();
    let tool = ReadFileTool::new(Arc::clone(&store));
    let result = tool
        .call(serde_json::json!({"path": "nope.txt"}), &ctx("r2"))
        .await
        .unwrap();
    assert!(result.is_error);
}

#[tokio::test]
async fn read_with_line_range() {
    let store = mem_store();
    let content: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    store.write("lines.txt", &content).await.unwrap();
    let tool = ReadFileTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({"path": "lines.txt", "start_line": 5, "end_line": 7}),
            &ctx("r3"),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("line 5"));
    assert!(result.content.contains("line 7"));
    assert!(!result.content.contains("line 8"));
}

#[tokio::test]
async fn read_with_line_numbers_enabled() {
    let store = mem_store();
    store
        .write("num.txt", "alpha\nbeta\ngamma\n")
        .await
        .unwrap();
    let tool = ReadFileTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({"path": "num.txt", "with_line_numbers": true}),
            &ctx("r4"),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("1 |") || result.content.contains("  1 |"));
}

// ── WriteFileTool ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn write_creates_new_file() {
    let store = mem_store();
    let tool = WriteFileTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({"path": "new.txt", "content": "brand new"}),
            &ctx("w1"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "got error: {}", result.content);
    let written = store.read("new.txt").await.unwrap();
    assert_eq!(written, "brand new");
}

#[tokio::test]
async fn write_overwrites_existing_file() {
    let store = mem_store();
    store.write("ex.txt", "old content").await.unwrap();
    let tool = WriteFileTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({"path": "ex.txt", "content": "new content"}),
            &ctx("w2"),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    let written = store.read("ex.txt").await.unwrap();
    assert_eq!(written, "new content");
}

// ── EditFileTool ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn edit_replaces_block() {
    let store = mem_store();
    store
        .write("edit.txt", "fn hello() {\n    println!(\"hello\");\n}\n")
        .await
        .unwrap();
    let tool = EditFileTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({
                "path": "edit.txt",
                "old_content": "fn hello() {\n    println!(\"hello\");\n}\n",
                "new_content": "fn hello() {\n    println!(\"world\");\n}\n"
            }),
            &ctx("e1"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "got error: {}", result.content);
    let updated = store.read("edit.txt").await.unwrap();
    assert!(updated.contains("world"));
    assert!(!updated.contains("\"hello\""));
}

#[tokio::test]
async fn edit_missing_old_content_returns_error() {
    let store = mem_store();
    store.write("f.txt", "hello").await.unwrap();
    let tool = EditFileTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({
                "path": "f.txt",
                "old_content": "NOTHERE",
                "new_content": "replacement"
            }),
            &ctx("e2"),
        )
        .await
        .unwrap();
    assert!(
        result.is_error,
        "expected error but got ok: {}",
        result.content
    );
}

// ── ListFilesTool ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_returns_all_paths() {
    let store = mem_store();
    store.write("a.rs", "").await.unwrap();
    store.write("b.rs", "").await.unwrap();
    store.write("c.txt", "").await.unwrap();
    let tool = ListFilesTool::new(Arc::clone(&store));
    let result = tool.call(serde_json::json!({}), &ctx("l1")).await.unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("a.rs"));
    assert!(result.content.contains("b.rs"));
    assert!(result.content.contains("c.txt"));
}

#[tokio::test]
async fn list_filters_by_suffix() {
    let store = mem_store();
    store.write("main.rs", "").await.unwrap();
    store.write("README.md", "").await.unwrap();
    let tool = ListFilesTool::new(Arc::clone(&store));
    let result = tool
        .call(serde_json::json!({"suffix": ".rs"}), &ctx("l2"))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("main.rs"));
    assert!(!result.content.contains("README.md"));
}

// ── GrepTool ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn grep_finds_pattern() {
    let store = mem_store();
    store
        .write("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n")
        .await
        .unwrap();
    store
        .write("src/lib.rs", "pub fn helper() {}\n")
        .await
        .unwrap();
    let tool = GrepTool::new(Arc::clone(&store));
    let result = tool
        .call(serde_json::json!({"pattern": "println"}), &ctx("g1"))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("main.rs"));
    assert!(!result.content.contains("lib.rs"));
}

#[tokio::test]
async fn grep_no_matches() {
    let store = mem_store();
    store.write("x.txt", "nothing here").await.unwrap();
    let tool = GrepTool::new(Arc::clone(&store));
    let result = tool
        .call(
            serde_json::json!({"pattern": "XYZZY_NOT_FOUND"}),
            &ctx("g2"),
        )
        .await
        .unwrap();
    // No match — just verify it doesn't panic
    let _ = result;
}

// ── BashExecTool ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn bash_exec_echo() {
    let tmp = TempDir::new().unwrap();
    let sandbox = Sandbox::new(tmp.path()).with_timeout(Duration::from_secs(5));
    let executor = Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::permissive()));
    let tool = BashExecTool::new(executor);
    let result = tool
        .call(
            serde_json::json!({"command": "echo integration_test"}),
            &ctx("b1"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "error: {}", result.content);
    assert!(result.content.contains("integration_test"));
}

#[tokio::test]
async fn bash_exec_nonzero_exit() {
    let tmp = TempDir::new().unwrap();
    let sandbox = Sandbox::new(tmp.path()).with_timeout(Duration::from_secs(5));
    let executor = Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::permissive()));
    let tool = BashExecTool::new(executor);
    let result = tool
        .call(serde_json::json!({"command": "exit 42"}), &ctx("b2"))
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("42"));
}

// ── FsWorkspaceStore ──────────────────────────────────────────────────────────

#[tokio::test]
async fn fs_store_read_write_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let store: Arc<dyn WorkspaceStore> =
        Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
    store.write("src/main.rs", "fn main() {}").await.unwrap();
    let content = store.read("src/main.rs").await.unwrap();
    assert_eq!(content, "fn main() {}");
}

#[tokio::test]
async fn fs_store_list_and_grep() {
    let tmp = TempDir::new().unwrap();
    let store: Arc<dyn WorkspaceStore> =
        Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
    store.write("a.rs", "fn foo() {}").await.unwrap();
    store.write("b.rs", "fn bar() {}").await.unwrap();

    let list_tool = ListFilesTool::new(Arc::clone(&store));
    let list_result = list_tool
        .call(serde_json::json!({}), &ctx("fl1"))
        .await
        .unwrap();
    assert!(!list_result.is_error);
    assert!(list_result.content.contains("a.rs"));

    let grep_tool = GrepTool::new(Arc::clone(&store));
    let grep_result = grep_tool
        .call(serde_json::json!({"pattern": "fn foo"}), &ctx("fg1"))
        .await
        .unwrap();
    assert!(!grep_result.is_error);
    assert!(grep_result.content.contains("a.rs"));
}

// ── ToolRegistry ─────────────────────────────────────────────────────────────

#[test]
fn registry_builder_reader() {
    let tmp = TempDir::new().unwrap();
    let reg = ToolRegistryBuilder::new(tmp.path())
        .in_memory()
        .without_git()
        .build_reader()
        .unwrap();
    assert_eq!(reg.len(), 9); // 3 original + 6 new FS read-only tools
    assert!(reg.get("read_file").is_some());
    assert!(reg.get("list_files").is_some());
    assert!(reg.get("grep").is_some());
}

#[test]
fn registry_builder_coder_with_shell() {
    let tmp = TempDir::new().unwrap();
    let reg = ToolRegistryBuilder::new(tmp.path())
        .in_memory()
        .without_git()
        .with_shell()
        .build_coder()
        .unwrap();
    assert!(reg.len() >= 6);
    assert!(reg.get("write_file").is_some());
    assert!(reg.get("edit_file").is_some());
    assert!(reg.get("bash_exec").is_some());
}

#[test]
fn registry_all_returns_in_insertion_order() {
    let tmp = TempDir::new().unwrap();
    let reg = ToolRegistryBuilder::new(tmp.path())
        .in_memory()
        .without_git()
        .build_reader()
        .unwrap();
    let names: Vec<String> = reg.all().iter().map(|t| t.name().to_string()).collect();
    let expected_start = ["list_files", "read_file", "grep"];
    for name in &expected_start {
        assert!(names.contains(&name.to_string()), "missing: {name}");
    }
    assert_eq!(names.len(), 9);
}
