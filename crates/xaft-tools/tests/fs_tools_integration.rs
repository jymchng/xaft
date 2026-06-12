//! Integration tests for the 14 new filesystem tools added in PRD 53.

use std::sync::Arc;

use agtrs_runtime::tool::{Tool, ToolContext};
use agtrs_workspace::WorkspaceStore;
use tempfile::TempDir;
use tokio::fs;

use xaft_tools::{
    AppendToFileTool, CopyFileTool, CreateDirectoryTool, DeleteFileTool, DiffFilesTool,
    FileStatToolFs, FsWorkspaceStore, GlobToolFs, ListFilesTool, MoveFileTool, PatchFileTool,
    ReadManyTool, RemoveDirectoryTool, SearchFilesTool, TreeToolFs,
};

// ── Helper utilities ──────────────────────────────────────────────────────────

fn ctx(id: &str) -> ToolContext {
    ToolContext::new(id)
}

fn fs_store(root: &std::path::Path) -> Arc<dyn WorkspaceStore> {
    Arc::new(FsWorkspaceStore::new(root)) as Arc<dyn WorkspaceStore>
}

// ── glob ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn glob_finds_matching_files() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src")).await.unwrap();
    fs::write(tmp.path().join("src/main.rs"), "fn main() {}")
        .await
        .unwrap();
    fs::write(tmp.path().join("src/lib.rs"), "pub fn lib() {}")
        .await
        .unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = GlobToolFs::new(store, tmp.path());
    let result = tool
        .call(serde_json::json!({"pattern": "src/*.rs"}), &ctx("g1"))
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
    assert!(
        paths.contains(&"src/main.rs".to_string()),
        "paths: {paths:?}"
    );
    assert!(
        paths.contains(&"src/lib.rs".to_string()),
        "paths: {paths:?}"
    );
    assert!(!paths.iter().any(|p| p.ends_with(".toml")));
}

#[tokio::test]
async fn glob_recursive_star_star() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("a/b")).await.unwrap();
    fs::write(tmp.path().join("a/b/deep.rs"), "").await.unwrap();
    fs::write(tmp.path().join("top.rs"), "").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = GlobToolFs::new(store, tmp.path());
    let result = tool
        .call(serde_json::json!({"pattern": "**/*.rs"}), &ctx("g2"))
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
    assert!(
        paths.contains(&"a/b/deep.rs".to_string()),
        "paths: {paths:?}"
    );
    assert!(paths.contains(&"top.rs".to_string()), "paths: {paths:?}");
}

// ── file_stat ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn file_stat_returns_metadata() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("hello.txt"), "hello world")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = FileStatToolFs::new(store, tmp.path());
    let result = tool
        .call(serde_json::json!({"path": "hello.txt"}), &ctx("fs1"))
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let stat: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(stat["exists"], true);
    assert_eq!(stat["is_file"], true);
    assert_eq!(stat["is_dir"], false);
    assert!(stat["size_bytes"].as_u64().unwrap() > 0);
    assert!(stat["modified_secs"].as_u64().is_some());
}

#[tokio::test]
async fn file_stat_missing_returns_exists_false() {
    let tmp = TempDir::new().unwrap();
    let store = fs_store(tmp.path());
    let tool = FileStatToolFs::new(store, tmp.path());
    let result = tool
        .call(serde_json::json!({"path": "nosuchfile.txt"}), &ctx("fs2"))
        .await
        .unwrap();

    assert!(!result.is_error);
    let stat: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(stat["exists"], false);
}

// ── tree ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tree_renders_directory() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src")).await.unwrap();
    fs::write(tmp.path().join("src/main.rs"), "").await.unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = TreeToolFs::new(store, tmp.path());
    let result = tool.call(serde_json::json!({}), &ctx("tr1")).await.unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    assert!(
        result.content.contains("src/"),
        "content: {}",
        result.content
    );
    assert!(
        result.content.contains("main.rs"),
        "content: {}",
        result.content
    );
    assert!(
        result.content.contains("Cargo.toml"),
        "content: {}",
        result.content
    );
}

#[tokio::test]
async fn tree_skips_target_and_git() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).await.unwrap();
    fs::create_dir_all(tmp.path().join("target")).await.unwrap();
    fs::write(tmp.path().join("src.rs"), "").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = TreeToolFs::new(store, tmp.path());
    let result = tool.call(serde_json::json!({}), &ctx("tr2")).await.unwrap();

    assert!(!result.is_error);
    assert!(
        !result.content.contains(".git"),
        "should skip .git: {}",
        result.content
    );
    assert!(
        !result.content.contains("target"),
        "should skip target: {}",
        result.content
    );
}

// ── diff_files ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn diff_files_produces_unified_diff() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.rs"), "fn foo() {}\nfn bar() {}\n")
        .await
        .unwrap();
    fs::write(tmp.path().join("b.rs"), "fn foo() {}\nfn baz() {}\n")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = DiffFilesTool::new(store);
    let result = tool
        .call(
            serde_json::json!({"path_a": "a.rs", "path_b": "b.rs"}),
            &ctx("df1"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    // Should show removal of bar and addition of baz
    assert!(
        result.content.contains("bar") && result.content.contains("baz"),
        "diff content: {}",
        result.content
    );
}

// ── read_many ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_many_reads_multiple_files() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.rs"), "fn a() {}")
        .await
        .unwrap();
    fs::write(tmp.path().join("b.rs"), "fn b() {}")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = ReadManyTool::new(store);
    let result = tool
        .call(serde_json::json!({"paths": ["a.rs", "b.rs"]}), &ctx("rm1"))
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let items: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0]["content"].as_str().unwrap().contains("fn a"));
    assert!(items[1]["content"].as_str().unwrap().contains("fn b"));
}

#[tokio::test]
async fn read_many_reports_error_per_missing_file() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("exists.rs"), "ok").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = ReadManyTool::new(store);
    let result = tool
        .call(
            serde_json::json!({"paths": ["exists.rs", "missing.rs"]}),
            &ctx("rm2"),
        )
        .await
        .unwrap();

    assert!(!result.is_error);
    let items: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert_eq!(items[0]["error"], serde_json::Value::Null);
    assert!(items[1]["error"].as_str().is_some());
}

// ── search_files ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_files_finds_by_name() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src")).await.unwrap();
    fs::write(tmp.path().join("src/main.rs"), "").await.unwrap();
    fs::write(tmp.path().join("src/lib.rs"), "").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = SearchFilesTool::new(store);
    let result = tool
        .call(serde_json::json!({"name": "main.rs"}), &ctx("sf1"))
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
    assert!(
        paths.contains(&"src/main.rs".to_string()),
        "paths: {paths:?}"
    );
    assert!(!paths.iter().any(|p| p.contains("lib")));
}

// ── move_file ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn move_file_renames_file() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("old.rs"), "content")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = MoveFileTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"source": "old.rs", "destination": "new.rs"}),
            &ctx("mv1"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    assert!(!tmp.path().join("old.rs").exists(), "source should be gone");
    assert!(
        tmp.path().join("new.rs").exists(),
        "destination should exist"
    );
}

// ── copy_file ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn copy_file_duplicates_file() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("src.rs"), "hello").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = CopyFileTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"source": "src.rs", "destination": "dst.rs"}),
            &ctx("cp1"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    assert!(
        tmp.path().join("src.rs").exists(),
        "source should still exist"
    );
    assert!(
        tmp.path().join("dst.rs").exists(),
        "destination should exist"
    );
    let content = fs::read_to_string(tmp.path().join("dst.rs")).await.unwrap();
    assert_eq!(content, "hello");
}

// ── delete_file ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_file_requires_confirm_flag() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("file.rs"), "x").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = DeleteFileTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"path": "file.rs", "confirm": false}),
            &ctx("del1"),
        )
        .await
        .unwrap();

    assert!(result.is_error, "should refuse without confirm");
    assert!(
        tmp.path().join("file.rs").exists(),
        "file should still exist"
    );
}

#[tokio::test]
async fn delete_file_removes_when_confirmed() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("file.rs"), "x").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = DeleteFileTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"path": "file.rs", "confirm": true}),
            &ctx("del2"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    assert!(!tmp.path().join("file.rs").exists(), "file should be gone");
}

// ── create_directory ──────────────────────────────────────────────────────────

#[tokio::test]
async fn create_directory_creates_nested() {
    let tmp = TempDir::new().unwrap();
    let store = fs_store(tmp.path());
    let tool = CreateDirectoryTool::new(store, tmp.path());
    let result = tool
        .call(serde_json::json!({"path": "a/b/c"}), &ctx("cd1"))
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    assert!(tmp.path().join("a/b/c").is_dir());
}

// ── remove_directory ──────────────────────────────────────────────────────────

#[tokio::test]
async fn remove_directory_removes_empty() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("empty_dir"))
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = RemoveDirectoryTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"path": "empty_dir", "confirm": true}),
            &ctx("rd1"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    assert!(!tmp.path().join("empty_dir").exists());
}

#[tokio::test]
async fn remove_directory_requires_recursive_for_non_empty() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("dir")).await.unwrap();
    fs::write(tmp.path().join("dir/file.rs"), "x")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = RemoveDirectoryTool::new(store, tmp.path());
    // Without recursive, should fail on non-empty dir
    let result = tool
        .call(
            serde_json::json!({"path": "dir", "recursive": false, "confirm": true}),
            &ctx("rd2"),
        )
        .await;

    // Either Err or is_error since dir is non-empty
    assert!(
        result.is_err() || result.unwrap().is_error,
        "should fail on non-empty without recursive"
    );
}

// ── append_to_file ────────────────────────────────────────────────────────────

#[tokio::test]
async fn append_to_file_creates_if_missing() {
    let tmp = TempDir::new().unwrap();
    let store = fs_store(tmp.path());
    let tool = AppendToFileTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"path": "new.log", "content": "hello\n"}),
            &ctx("af1"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let content = fs::read_to_string(tmp.path().join("new.log"))
        .await
        .unwrap();
    assert_eq!(content, "hello\n");
}

#[tokio::test]
async fn append_to_file_appends_to_existing() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("log.txt"), "line1\n")
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = AppendToFileTool::new(store, tmp.path());
    let result = tool
        .call(
            serde_json::json!({"path": "log.txt", "content": "line2\n"}),
            &ctx("af2"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let content = fs::read_to_string(tmp.path().join("log.txt"))
        .await
        .unwrap();
    assert_eq!(content, "line1\nline2\n");
}

// ── patch_file ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn patch_file_applies_unified_diff() {
    let tmp = TempDir::new().unwrap();
    let original = "fn main() {\n    println!(\"hello\");\n}\n";
    fs::write(tmp.path().join("main.rs"), original)
        .await
        .unwrap();

    let store = fs_store(tmp.path());
    let tool = PatchFileTool::new(store, tmp.path());

    // Build a valid patch
    let new_content = "fn main() {\n    println!(\"world\");\n}\n";
    let patch = diffy::create_patch(original, new_content);
    let patch_str = patch.to_string();

    let result = tool
        .call(
            serde_json::json!({"path": "main.rs", "patch": patch_str}),
            &ctx("pf1"),
        )
        .await
        .unwrap();

    assert!(!result.is_error, "error: {}", result.content);
    let updated = fs::read_to_string(tmp.path().join("main.rs"))
        .await
        .unwrap();
    assert!(updated.contains("world"), "updated: {updated}");
    assert!(!updated.contains("hello"), "updated: {updated}");
}

// ── path traversal for all tools ──────────────────────────────────────────────

#[tokio::test]
async fn path_traversal_rejected_for_all_tools() {
    let tmp = TempDir::new().unwrap();
    let store = fs_store(tmp.path());

    // GlobToolFs
    let glob_tool = GlobToolFs::new(Arc::clone(&store), tmp.path());
    let r = glob_tool
        .call(serde_json::json!({"pattern": "../**/*.rs"}), &ctx("pt1"))
        .await;
    assert!(r.is_err() || r.unwrap().is_error);

    // FileStatToolFs
    let stat_tool = FileStatToolFs::new(Arc::clone(&store), tmp.path());
    let r = stat_tool
        .call(serde_json::json!({"path": "../../etc/passwd"}), &ctx("pt2"))
        .await;
    assert!(r.is_err() || r.unwrap().is_error);

    // DiffFilesTool
    let diff_tool = DiffFilesTool::new(Arc::clone(&store));
    let r = diff_tool
        .call(
            serde_json::json!({"path_a": "../a.rs", "path_b": "b.rs"}),
            &ctx("pt3"),
        )
        .await;
    assert!(r.is_err() || r.unwrap().is_error);

    // ReadManyTool — returns per-file error records
    let read_many_tool = ReadManyTool::new(Arc::clone(&store));
    let r = read_many_tool
        .call(serde_json::json!({"paths": ["../secret.txt"]}), &ctx("pt4"))
        .await
        .unwrap();
    assert!(!r.is_error); // returns error inside array
    let items: Vec<serde_json::Value> = serde_json::from_str(&r.content).unwrap();
    assert!(items[0]["error"].as_str().is_some());

    // DeleteFileTool
    let del_tool = DeleteFileTool::new(Arc::clone(&store), tmp.path());
    let r = del_tool
        .call(
            serde_json::json!({"path": "../outside.rs", "confirm": true}),
            &ctx("pt5"),
        )
        .await;
    assert!(r.is_err() || r.unwrap().is_error);

    // CreateDirectoryTool
    let mkdir_tool = CreateDirectoryTool::new(Arc::clone(&store), tmp.path());
    let r = mkdir_tool
        .call(serde_json::json!({"path": "../../hack"}), &ctx("pt6"))
        .await;
    assert!(r.is_err() || r.unwrap().is_error);

    // AppendToFileTool
    let append_tool = AppendToFileTool::new(Arc::clone(&store), tmp.path());
    let r = append_tool
        .call(
            serde_json::json!({"path": "../evil.txt", "content": "hacked"}),
            &ctx("pt7"),
        )
        .await;
    assert!(r.is_err() || r.unwrap().is_error);
}

// ── list_files recursive option ───────────────────────────────────────────────

#[tokio::test]
async fn list_files_recursive_option() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src")).await.unwrap();
    fs::write(tmp.path().join("src/main.rs"), "").await.unwrap();
    fs::write(tmp.path().join("top.rs"), "").await.unwrap();

    let store = fs_store(tmp.path());
    let tool = ListFilesTool::new(store);

    // Default (recursive=true) should include src/main.rs
    let result = tool.call(serde_json::json!({}), &ctx("lf1")).await.unwrap();
    assert!(!result.is_error);
    assert!(
        result.content.contains("src/main.rs"),
        "content: {}",
        result.content
    );
    assert!(
        result.content.contains("top.rs"),
        "content: {}",
        result.content
    );

    // recursive=false should exclude src/main.rs (in subdirectory)
    let result = tool
        .call(serde_json::json!({"recursive": false}), &ctx("lf2"))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(
        !result.content.contains("src/main.rs"),
        "should not have subdirfile: {}",
        result.content
    );
    assert!(
        result.content.contains("top.rs"),
        "should have top-level: {}",
        result.content
    );
}
