//! End-to-end tests that simulate agent tool-use pipelines for the new
//! filesystem tools. These tests verify tools work correctly when called
//! in the patterns an agent would use them — chaining glob → read_file,
//! read_file → diff_files, etc.
//!
//! Note: True XaftRuntime e2e tests would require xaft-runtime as a
//! dev-dependency, which would create a circular dependency (xaft-runtime
//! depends on xaft-tools). These tests use the Tool trait directly,
//! which is the same code path agents use.

use std::sync::Arc;

use agtrs_runtime::tool::{Tool, ToolContext};
use agtrs_workspace::WorkspaceStore;
use tempfile::TempDir;
use tokio::fs;

use xaft_tools::{
    DiffFilesTool, FileStatToolFs, FsWorkspaceStore, GlobToolFs, ReadFileTool, ReadManyTool,
    TreeToolFs,
};

fn ctx(id: &str) -> ToolContext {
    ToolContext::new(id)
}

fn fs_store(root: &std::path::Path) -> Arc<dyn WorkspaceStore> {
    Arc::new(FsWorkspaceStore::new(root)) as Arc<dyn WorkspaceStore>
}

/// An agent would typically:
/// 1. Call glob to discover Rust files
/// 2. Call read_file on each found file
///
/// Verify this chain works end-to-end.
#[tokio::test]
async fn agent_can_use_glob_to_find_rust_files() {
    // Setup: TempDir with some .rs files
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src")).await.unwrap();
    fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    fs::write(
        tmp.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .await
    .unwrap();
    fs::write(tmp.path().join("README.md"), "# Docs")
        .await
        .unwrap();

    let store = fs_store(tmp.path());

    // Step 1: Agent calls glob to find .rs files
    let glob_tool = GlobToolFs::new(Arc::clone(&store), tmp.path());
    let glob_result = glob_tool
        .call(serde_json::json!({"pattern": "**/*.rs"}), &ctx("e2e-g1"))
        .await
        .unwrap();

    assert!(!glob_result.is_error, "glob error: {}", glob_result.content);
    let paths: Vec<String> = serde_json::from_str(&glob_result.content).unwrap();
    assert!(!paths.is_empty(), "should find at least one .rs file");
    assert!(
        paths.contains(&"src/main.rs".to_string()),
        "should find main.rs, got: {paths:?}"
    );
    assert!(
        paths.iter().all(|p| p.ends_with(".rs")),
        "should only return .rs files, got: {paths:?}"
    );

    // Step 2: Agent calls read_file on each found path
    let read_tool = ReadFileTool::new(Arc::clone(&store));
    for path in &paths {
        let read_result = read_tool
            .call(serde_json::json!({"path": path}), &ctx("e2e-r1"))
            .await
            .unwrap();
        assert!(
            !read_result.is_error,
            "read_file failed for {path}: {}",
            read_result.content
        );
        assert!(
            !read_result.content.is_empty(),
            "file {path} should not be empty"
        );
    }
}

/// An agent doing code review might:
/// 1. Read the original file
/// 2. Create a modified version
/// 3. Diff the two files to see what changed
#[tokio::test]
async fn agent_can_read_then_diff_files() {
    let tmp = TempDir::new().unwrap();
    let original = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    let modified = "fn add(a: i32, b: i32) -> i32 {\n    // add two numbers\n    a + b\n}\n";

    fs::write(tmp.path().join("original.rs"), original)
        .await
        .unwrap();
    fs::write(tmp.path().join("modified.rs"), modified)
        .await
        .unwrap();

    let store = fs_store(tmp.path());

    // Step 1: Read both files
    let read_tool = ReadFileTool::new(Arc::clone(&store));
    let orig_result = read_tool
        .call(serde_json::json!({"path": "original.rs"}), &ctx("e2e-rd1"))
        .await
        .unwrap();
    assert!(!orig_result.is_error, "error: {}", orig_result.content);

    let mod_result = read_tool
        .call(serde_json::json!({"path": "modified.rs"}), &ctx("e2e-rd2"))
        .await
        .unwrap();
    assert!(!mod_result.is_error, "error: {}", mod_result.content);

    // Step 2: Diff the two files
    let diff_tool = DiffFilesTool::new(Arc::clone(&store));
    let diff_result = diff_tool
        .call(
            serde_json::json!({
                "path_a": "original.rs",
                "path_b": "modified.rs"
            }),
            &ctx("e2e-diff1"),
        )
        .await
        .unwrap();

    assert!(
        !diff_result.is_error,
        "diff_files error: {}",
        diff_result.content
    );
    // The diff should show the added comment
    assert!(
        diff_result.content.contains("add two numbers"),
        "diff should contain the new comment: {}",
        diff_result.content
    );
    assert!(
        diff_result.content.contains('+'),
        "diff should contain additions: {}",
        diff_result.content
    );
}

/// An agent exploring a workspace might:
/// 1. Call tree to understand directory structure
/// 2. Call file_stat to check metadata of interesting files
/// 3. Call read_many to batch-read multiple files
#[tokio::test]
async fn agent_can_explore_workspace_with_tree_and_stat() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src")).await.unwrap();
    fs::create_dir_all(tmp.path().join("tests")).await.unwrap();
    fs::write(tmp.path().join("src/main.rs"), "fn main() {}")
        .await
        .unwrap();
    fs::write(tmp.path().join("src/lib.rs"), "pub fn lib() {}")
        .await
        .unwrap();
    fs::write(
        tmp.path().join("tests/integration.rs"),
        "#[test] fn it_works() {}",
    )
    .await
    .unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"test\"")
        .await
        .unwrap();

    let store = fs_store(tmp.path());

    // Step 1: Explore directory tree
    let tree_tool = TreeToolFs::new(Arc::clone(&store), tmp.path());
    let tree_result = tree_tool
        .call(serde_json::json!({"depth": 3}), &ctx("e2e-tr1"))
        .await
        .unwrap();

    assert!(!tree_result.is_error, "tree error: {}", tree_result.content);
    assert!(
        tree_result.content.contains("src/"),
        "tree: {}",
        tree_result.content
    );
    assert!(
        tree_result.content.contains("tests/"),
        "tree: {}",
        tree_result.content
    );

    // Step 2: Check stats on a specific file
    let stat_tool = FileStatToolFs::new(Arc::clone(&store), tmp.path());
    let stat_result = stat_tool
        .call(serde_json::json!({"path": "src/main.rs"}), &ctx("e2e-st1"))
        .await
        .unwrap();

    assert!(!stat_result.is_error, "stat error: {}", stat_result.content);
    let stat: serde_json::Value = serde_json::from_str(&stat_result.content).unwrap();
    assert_eq!(stat["exists"], true);
    assert_eq!(stat["is_file"], true);

    // Step 3: Batch read multiple files
    let read_many_tool = ReadManyTool::new(Arc::clone(&store));
    let read_many_result = read_many_tool
        .call(
            serde_json::json!({
                "paths": ["src/main.rs", "src/lib.rs", "Cargo.toml"]
            }),
            &ctx("e2e-rm1"),
        )
        .await
        .unwrap();

    assert!(
        !read_many_result.is_error,
        "read_many error: {}",
        read_many_result.content
    );
    let items: Vec<serde_json::Value> = serde_json::from_str(&read_many_result.content).unwrap();
    assert_eq!(items.len(), 3, "should have 3 results");
    assert!(
        items.iter().all(|i| i["error"] == serde_json::Value::Null),
        "no errors expected, got: {items:?}"
    );
}
