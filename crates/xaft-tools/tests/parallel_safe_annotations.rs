//! Tests verifying that xaft tools have correct `parallel_safe()` annotations.
//!
//! PRD 56: Read-only tools must return `parallel_safe() = true`;
//!         write/destructive tools must return `false` (the default).

use std::sync::Arc;

use agtrs_runtime::tool::Tool;
use agtrs_workspace::InMemoryWorkspaceStore;
use xaft_tools::{
    EditFileTool, GitDiffTool, GitLogTool, GitStatusTool, GrepTool, ListFilesTool, ReadFileTool,
    WriteFileTool,
};

fn empty_workspace() -> Arc<dyn agtrs_workspace::WorkspaceStore> {
    Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn agtrs_workspace::WorkspaceStore>
}

fn make_git_repo() -> Arc<agtrs_git::GitRepo> {
    let dir = tempfile::tempdir().expect("tempdir");
    // Init a minimal git repo so GitRepo::open succeeds.
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    let repo = agtrs_git::GitRepo::open(dir.path()).expect("open git repo");
    // Keep the tempdir alive by leaking it (test process is short-lived).
    std::mem::forget(dir);
    Arc::new(repo)
}

// ─── FS read-only tools ───────────────────────────────────────────────────────

#[test]
fn all_read_only_fs_tools_are_parallel_safe() {
    let ws = empty_workspace();

    let read_file = ReadFileTool::new(Arc::clone(&ws));
    assert!(
        read_file.parallel_safe(),
        "ReadFileTool must be parallel_safe"
    );

    let list_files = ListFilesTool::new(Arc::clone(&ws));
    assert!(
        list_files.parallel_safe(),
        "ListFilesTool must be parallel_safe"
    );

    let grep = GrepTool::new(Arc::clone(&ws));
    assert!(grep.parallel_safe(), "GrepTool must be parallel_safe");
}

// ─── FS write tools ───────────────────────────────────────────────────────────

#[test]
fn all_write_fs_tools_are_not_parallel_safe() {
    let ws = empty_workspace();

    let write_file = WriteFileTool::new(Arc::clone(&ws));
    assert!(
        !write_file.parallel_safe(),
        "WriteFileTool must NOT be parallel_safe"
    );

    let edit_file = EditFileTool::new(Arc::clone(&ws));
    assert!(
        !edit_file.parallel_safe(),
        "EditFileTool must NOT be parallel_safe"
    );
}

// ─── Git read-only tools ──────────────────────────────────────────────────────

#[test]
fn all_read_only_git_tools_are_parallel_safe() {
    let repo = make_git_repo();

    let status = GitStatusTool::new(Arc::clone(&repo));
    assert!(
        status.parallel_safe(),
        "GitStatusTool must be parallel_safe"
    );

    let diff = GitDiffTool::new(Arc::clone(&repo), std::path::PathBuf::from("."));
    assert!(diff.parallel_safe(), "GitDiffTool must be parallel_safe");

    let log = GitLogTool::new(Arc::clone(&repo), std::path::PathBuf::from("."));
    assert!(log.parallel_safe(), "GitLogTool must be parallel_safe");
}
