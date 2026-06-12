//! Integration tests for the expanded git tools in xaft-tools.

use std::sync::Arc;

use agtrs_git::GitRepo;
use agtrs_runtime::tool::{Tool, ToolContext};
use tempfile::TempDir;
use tokio::process::Command;

use xaft_tools::{
    GitAddTool, GitBlameTool, GitBranchTool, GitCheckoutFilesTool, GitCommitStagedTool,
    GitCreateBranchTool, GitDiffTool, GitGrepTool, GitLogTool, GitMergeTool, GitPushTool,
    GitRemoteTool, GitShowTool, GitStashListTool, GitStashTool, GitTagTool, GitUnstageTool,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn setup_git_repo() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().to_path_buf();
    for args in &[
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "test@test.com"],
        vec!["config", "user.name", "Test"],
    ] {
        Command::new("git")
            .args(args)
            .current_dir(&p)
            .output()
            .await
            .unwrap();
    }
    std::fs::write(p.join("README.md"), "# Test\nHello World\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    (tmp, p)
}

fn ctx(id: &str) -> ToolContext {
    ToolContext::new(id)
}

// ── git_blame ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_blame_returns_entries() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitBlameTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({"path": "README.md"}), &ctx("t-blame"))
        .await
        .unwrap();
    assert!(!result.is_error, "blame error: {}", result.content);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert!(!arr.is_empty(), "blame should return entries");
    assert!(arr[0].get("sha").is_some());
    assert!(arr[0].get("author").is_some());
    assert!(arr[0].get("line_number").is_some());
    drop(tmp);
}

// ── git_show ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_show_shows_commit() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitShowTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({"ref": "HEAD"}), &ctx("t-show"))
        .await
        .unwrap();
    assert!(!result.is_error, "show error: {}", result.content);
    let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert!(parsed.get("sha").is_some());
    assert_eq!(parsed["subject"].as_str().unwrap_or(""), "initial");
    drop(tmp);
}

// ── git_branch ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_branch_lists_current() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitBranchTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({}), &ctx("t-branch"))
        .await
        .unwrap();
    assert!(!result.is_error, "branch error: {}", result.content);
    let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    let current = parsed["current"].as_str().unwrap_or("");
    assert!(!current.is_empty(), "current branch should not be empty");
    drop(tmp);
}

// ── git_stash_list ────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_stash_list_empty_initially() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitStashListTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({}), &ctx("t-stash-list"))
        .await
        .unwrap();
    assert!(!result.is_error, "stash list error: {}", result.content);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert!(arr.is_empty(), "stash should be empty initially");
    drop(tmp);
}

// ── git_remote ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_remote_empty_when_no_remote() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitRemoteTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({}), &ctx("t-remote"))
        .await
        .unwrap();
    assert!(!result.is_error, "remote error: {}", result.content);
    let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    let remotes = parsed["remotes"].as_array().unwrap();
    assert!(remotes.is_empty(), "no remotes configured");
    drop(tmp);
}

// ── git_grep ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_grep_finds_pattern() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitGrepTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({"pattern": "Hello"}), &ctx("t-grep"))
        .await
        .unwrap();
    assert!(!result.is_error, "grep error: {}", result.content);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert!(!arr.is_empty(), "grep should find 'Hello'");
    drop(tmp);
}

// ── git_add ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_add_stages_file() {
    let (tmp, p) = setup_git_repo().await;
    std::fs::write(p.join("new.txt"), "new content").unwrap();
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitAddTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({"paths": ["new.txt"]}), &ctx("t-add"))
        .await
        .unwrap();
    assert!(!result.is_error, "add error: {}", result.content);

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let status_str = String::from_utf8_lossy(&status.stdout);
    // new.txt should be staged (A) not untracked (??)
    assert!(
        status_str.contains('A'),
        "file should be staged: {}",
        status_str
    );
    drop(tmp);
}

// ── git_unstage ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_unstage_unstages_file() {
    let (tmp, p) = setup_git_repo().await;
    std::fs::write(p.join("staged.txt"), "staged").unwrap();
    Command::new("git")
        .args(["add", "staged.txt"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitUnstageTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"paths": ["staged.txt"]}),
            &ctx("t-unstage"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "unstage error: {}", result.content);

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.contains("??"),
        "file should be untracked after unstage: {}",
        status_str
    );
    drop(tmp);
}

// ── git_commit_staged: requires_confirmation ──────────────────────────────────

#[tokio::test]
async fn git_commit_requires_confirmation() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitCommitStagedTool::new(Arc::clone(&repo), &p);
    assert!(
        tool.requires_confirmation(),
        "git_commit_staged should require confirmation"
    );
    drop(tmp);
}

// ── git_commit_staged: creates commit ─────────────────────────────────────────

#[tokio::test]
async fn git_commit_staged_creates_commit() {
    let (tmp, p) = setup_git_repo().await;
    std::fs::write(p.join("file2.txt"), "content").unwrap();
    Command::new("git")
        .args(["add", "file2.txt"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitCommitStagedTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"message": "add file2"}),
            &ctx("t-commit"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "commit error: {}", result.content);
    let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert!(parsed.get("sha").is_some());
    assert_eq!(parsed["subject"].as_str().unwrap_or(""), "add file2");
    drop(tmp);
}

// ── git_stash ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_stash_saves_changes() {
    let (tmp, p) = setup_git_repo().await;
    std::fs::write(p.join("README.md"), "# Modified").unwrap();
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitStashTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({"message": "WIP stash"}), &ctx("t-stash"))
        .await
        .unwrap();
    assert!(!result.is_error, "stash error: {}", result.content);

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.trim().is_empty(),
        "working tree should be clean after stash: {}",
        status_str
    );
    drop(tmp);
}

// ── git_create_branch ─────────────────────────────────────────────────────────

#[tokio::test]
async fn git_create_branch_creates_new_branch() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitCreateBranchTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"name": "feat/new-feature", "checkout": false}),
            &ctx("t-create-branch"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "create branch error: {}", result.content);

    let branches = Command::new("git")
        .args(["branch", "--list", "feat/new-feature"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let branches_str = String::from_utf8_lossy(&branches.stdout);
    assert!(
        branches_str.contains("feat/new-feature"),
        "branch should exist: {}",
        branches_str
    );
    drop(tmp);
}

// ── git_push: requires_confirmation ──────────────────────────────────────────

#[tokio::test]
async fn git_push_requires_confirmation() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitPushTool::new(Arc::clone(&repo), &p);
    assert!(
        tool.requires_confirmation(),
        "git_push should require confirmation"
    );
    drop(tmp);
}

// ── git_checkout_files: requires_confirmation ─────────────────────────────────

#[tokio::test]
async fn git_checkout_files_requires_confirmation() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitCheckoutFilesTool::new(Arc::clone(&repo), &p);
    assert!(
        tool.requires_confirmation(),
        "git_checkout_files should require confirmation"
    );
    drop(tmp);
}

// ── git_diff extended: staged diff ───────────────────────────────────────────

#[tokio::test]
async fn git_diff_extended_staged_diff() {
    let (tmp, p) = setup_git_repo().await;
    std::fs::write(p.join("staged.txt"), "staged content").unwrap();
    Command::new("git")
        .args(["add", "staged.txt"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitDiffTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"target": "staged"}),
            &ctx("t-diff-staged"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "diff staged error: {}", result.content);
    assert!(
        result.content.contains("staged.txt"),
        "staged diff should mention the staged file: {}",
        result.content
    );
    drop(tmp);
}

// ── git_log extended: path filter ────────────────────────────────────────────

#[tokio::test]
async fn git_log_extended_with_path_filter() {
    let (tmp, p) = setup_git_repo().await;
    // Add a second commit on a specific file
    std::fs::write(p.join("specific.txt"), "specific content").unwrap();
    Command::new("git")
        .args(["add", "specific.txt"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "add specific file"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitLogTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"path": "specific.txt"}),
            &ctx("t-log-path"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "log path error: {}", result.content);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert_eq!(arr.len(), 1, "only one commit touched specific.txt");
    assert!(
        arr[0]["subject"]
            .as_str()
            .unwrap_or("")
            .contains("add specific file")
    );
    drop(tmp);
}

// ── git_merge: requires_confirmation ─────────────────────────────────────────

#[tokio::test]
async fn git_merge_requires_confirmation() {
    let (tmp, p) = setup_git_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitMergeTool::new(Arc::clone(&repo), &p);
    assert!(
        tool.requires_confirmation(),
        "git_merge should require confirmation"
    );
    drop(tmp);
}

// ── git_tag ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn git_tag_lists_tags() {
    let (tmp, p) = setup_git_repo().await;
    Command::new("git")
        .args(["tag", "v1.0.0"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitTagTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({}), &ctx("t-tag"))
        .await
        .unwrap();
    assert!(!result.is_error, "tag error: {}", result.content);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"].as_str().unwrap(), "v1.0.0");
    drop(tmp);
}
