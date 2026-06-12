//! Agent-executor end-to-end tests for git tools.
//!
//! Uses `MockLlmProvider` + `MockTransport` — no real API calls.
//! Sets up a real git repo and verifies tool calls produce valid JSON results.

use std::sync::Arc;

use agtrs_git::GitRepo;
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use agtrs_runtime::tool::{Tool, ToolContext};
use tempfile::TempDir;
use tokio::process::Command;

use xaft_tools::{GitAddTool, GitBlameTool, GitCommitStagedTool, GitStatusTool};

// ── Setup ─────────────────────────────────────────────────────────────────────

async fn setup_repo() -> (TempDir, std::path::PathBuf, Arc<GitRepo>) {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().to_path_buf();
    for args in &[
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "agent@test.com"],
        vec!["config", "user.name", "TestAgent"],
    ] {
        Command::new("git")
            .args(args)
            .current_dir(&p)
            .output()
            .await
            .unwrap();
    }
    std::fs::write(p.join("README.md"), "# Project\nInitial content\n").unwrap();
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
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    (tmp, p, repo)
}

fn ctx(id: &str) -> ToolContext {
    ToolContext::new(id)
}

// ── 1. Agent can check git status and blame ────────────────────────────────────

#[tokio::test]
async fn agent_can_check_git_status_and_blame() {
    let (tmp, p, repo) = setup_repo().await;

    // Mock an LLM transport — not actually used in this test since we call tools directly,
    // but demonstrates the pattern from the PRD
    let transport = Arc::new(MockTransport::new());
    transport.queue_text("Working tree is clean.").await;
    let _llm = Arc::new(MockLlmProvider::new(transport));

    // Simulate the agent calling git_status
    let status_tool = GitStatusTool::new(Arc::clone(&repo));
    let status_result = status_tool
        .call(serde_json::json!({}), &ctx("e2e-status"))
        .await
        .unwrap();
    assert!(
        !status_result.is_error,
        "status error: {}",
        status_result.content
    );
    // Clean repo: either "[]" or "nothing to commit"
    assert!(
        status_result.content.contains("clean") || status_result.content.contains("[]"),
        "expected clean status: {}",
        status_result.content
    );

    // Simulate the agent calling git_blame on README.md
    let blame_tool = GitBlameTool::new(Arc::clone(&repo), &p);
    let blame_result = blame_tool
        .call(serde_json::json!({"path": "README.md"}), &ctx("e2e-blame"))
        .await
        .unwrap();
    assert!(
        !blame_result.is_error,
        "blame error: {}",
        blame_result.content
    );

    // Validate it's valid JSON with expected fields
    let arr: Vec<serde_json::Value> = serde_json::from_str(&blame_result.content)
        .expect("blame result should be valid JSON array");
    assert!(!arr.is_empty(), "blame should return line entries");
    assert!(arr[0].get("sha").is_some(), "blame entry should have sha");
    assert!(
        arr[0].get("author").is_some(),
        "blame entry should have author"
    );
    assert!(
        arr[0].get("line_number").is_some(),
        "blame entry should have line_number"
    );
    assert!(
        arr[0].get("content").is_some(),
        "blame entry should have content"
    );

    drop(tmp);
}

// ── 2. Agent can stage and commit ─────────────────────────────────────────────

#[tokio::test]
async fn agent_can_stage_and_commit() {
    let (tmp, p, repo) = setup_repo().await;

    // Create an unstaged file
    std::fs::write(
        p.join("feature.rs"),
        "pub fn hello() { println!(\"hello\"); }\n",
    )
    .unwrap();

    // Mock LLM transport
    let transport = Arc::new(MockTransport::new());
    transport.queue_text("Staged and committed.").await;
    let _llm = Arc::new(MockLlmProvider::new(transport));

    // Agent calls git_add
    let add_tool = GitAddTool::new(Arc::clone(&repo), &p);
    let add_result = add_tool
        .call(
            serde_json::json!({"paths": ["feature.rs"]}),
            &ctx("e2e-add"),
        )
        .await
        .unwrap();
    assert!(!add_result.is_error, "add error: {}", add_result.content);

    // Verify the file is staged
    let status_out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let status_str = String::from_utf8_lossy(&status_out.stdout);
    assert!(
        status_str.contains('A'),
        "feature.rs should be staged (A): {}",
        status_str
    );

    // Agent calls git_commit_staged (requires_confirmation = true, but tool logic works)
    let commit_tool = GitCommitStagedTool::new(Arc::clone(&repo), &p);
    let commit_result = commit_tool
        .call(
            serde_json::json!({"message": "feat: add hello function"}),
            &ctx("e2e-commit"),
        )
        .await
        .unwrap();
    assert!(
        !commit_result.is_error,
        "commit error: {}",
        commit_result.content
    );

    // Validate the result is valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&commit_result.content).expect("commit result should be valid JSON");
    assert!(parsed.get("sha").is_some(), "commit result should have sha");
    assert_eq!(
        parsed["subject"].as_str().unwrap_or(""),
        "feat: add hello function"
    );

    // Verify the commit was actually created
    let log_out = Command::new("git")
        .args(["log", "--oneline", "-2"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    let log_str = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        log_str.contains("feat: add hello function"),
        "commit should appear in git log: {}",
        log_str
    );

    drop(tmp);
}
