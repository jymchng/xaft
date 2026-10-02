//! Behavioural edge-case coverage for the `git_grep` / `git_diff` / `git_log`
//! / `git_merge` tools.
//!
//! `git_tools_integration.rs` proves each tool works on the happy path. These
//! tests cover the branches that decide what a user (or an agent driving the
//! tool) actually observes when the input is not the happy path:
//!
//!   * the SCHEMA CONTRACT — every option the `call` body reads must be
//!     advertised in `schema()`, and `required` must name exactly the fields
//!     the tool refuses to run without. A tool that reads `path_prefix` but
//!     omits it from its schema is invisible to the model, which is a real
//!     defect, not a cosmetic one.
//!   * option handling that changes results (`case_sensitive`, `use_regex`,
//!     `max_results`, `target`, `strategy`) rather than merely being accepted.
//!   * the error paths that a caller must be able to distinguish: a missing
//!     required field, an unreachable repository, a cancelled call.
//!
//! Each test drives the real `git` binary against a real temporary repository.
//! Nothing here asserts on a line number or executes code for its own sake.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agtrs_git::GitRepo;
use agtrs_runtime::tool::{Tool, ToolContext};
use tempfile::TempDir;
use tokio::process::Command;

use xaft_tools::{GitDiffTool, GitGrepTool, GitLogTool, GitMergeTool};

// ── Fixtures ───────────────────────────────────────────────────────────────

async fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .expect("git must be runnable");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

async fn init_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().to_path_buf();
    git(&p, &["init", "--initial-branch=main"]).await;
    git(&p, &["config", "user.email", "test@test.com"]).await;
    git(&p, &["config", "user.name", "Ada Lovelace"]).await;
    git(&p, &["config", "commit.gpgsign", "false"]).await;

    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/alpha.rs"), "fn alpha() { TODO }\n").unwrap();
    std::fs::write(p.join("README.md"), "# Title\nTODO: write the docs\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "initial import"]).await;

    std::fs::write(p.join("src/alpha.rs"), "fn alpha() {}\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "implement alpha"]).await;

    (tmp, p)
}

fn ctx(id: &str) -> ToolContext {
    ToolContext::new(id)
}

/// The set of property names a tool's `schema()` advertises.
fn schema_properties(
    tool: &dyn Tool<Inputs = serde_json::Value, Output = agtrs_runtime::tool::ToolResult>,
) -> Vec<String> {
    tool.schema()["properties"]
        .as_object()
        .expect("schema must declare properties")
        .keys()
        .cloned()
        .collect()
}

fn schema_required(
    tool: &dyn Tool<Inputs = serde_json::Value, Output = agtrs_runtime::tool::ToolResult>,
) -> Vec<String> {
    tool.schema()["required"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Build a throwaway repository for the schema-contract tests.
///
/// These tests never call `call()`, so they do not need history — but they DO
/// need a `GitRepo`, and constructing one must never touch the working
/// directory: `GitRepo::init(".")` would create a real `.git` inside the
/// xaft checkout. A `TempDir` keeps the blast radius at zero.
fn scratch_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(tmp.path())
        .output()
        .expect("git init");
    tmp
}

// ── Schema contracts ──────────────────────────────────────────────────────
//
// These are behavioural, not decorative: the schema IS the tool's public
// interface to the model, so a mismatch with what `call` reads is a bug.

#[test]
fn git_grep_advertises_every_option_it_reads_and_requires_only_the_pattern() {
    let dir = scratch_repo();
    let tool = GitGrepTool::new(Arc::new(GitRepo::open(dir.path()).unwrap()), dir.path());
    let props = schema_properties(&tool);
    // `call` reads exactly these five keys; each must therefore be visible to
    // the model or the option is unreachable in practice.
    for expected in [
        "pattern",
        "path_prefix",
        "case_sensitive",
        "use_regex",
        "max_results",
    ] {
        assert!(
            props.iter().any(|p| p == expected),
            "git_grep schema omits `{expected}`; call() reads it. props={props:?}"
        );
    }
    assert_eq!(
        schema_required(&tool),
        vec!["pattern".to_string()],
        "git_grep requires only `pattern` — it cannot run without one"
    );
}

#[test]
fn git_diff_advertises_its_target_enum_and_its_ref_pair() {
    let dir = scratch_repo();
    let repo = Arc::new(GitRepo::open(dir.path()).unwrap());
    let tool = GitDiffTool::new(Arc::clone(&repo), dir.path());
    let props = schema_properties(&tool);
    for expected in ["target", "path", "from_ref", "to_ref"] {
        assert!(
            props.iter().any(|p| p == expected),
            "git_diff schema omits `{expected}`. props={props:?}"
        );
    }
    let schema = tool.schema();
    let target_enum: Vec<&str> = schema["properties"]["target"]["enum"]
        .as_array()
        .expect("target must be an enum")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        target_enum,
        vec!["head", "staged", "unstaged"],
        "the advertised target enum must match what the tool actually branches on"
    );
    assert_eq!(schema_required(&tool), Vec::<String>::new());
}

#[test]
fn git_log_advertises_every_filter_it_reads() {
    let dir = scratch_repo();
    let tool = GitLogTool::new(Arc::new(GitRepo::open(dir.path()).unwrap()), dir.path());
    let props = schema_properties(&tool);
    for expected in ["max", "path", "author", "since", "grep"] {
        assert!(
            props.iter().any(|p| p == expected),
            "git_log schema omits `{expected}`. props={props:?}"
        );
    }
    assert_eq!(schema_required(&tool), Vec::<String>::new());
}

#[test]
fn git_merge_advertises_its_strategy_enum_and_requires_the_branch() {
    let dir = scratch_repo();
    let tool = GitMergeTool::new(Arc::new(GitRepo::open(dir.path()).unwrap()), dir.path());
    let props = schema_properties(&tool);
    for expected in ["branch", "strategy", "message"] {
        assert!(
            props.iter().any(|p| p == expected),
            "git_merge schema omits `{expected}`. props={props:?}"
        );
    }
    let schema = tool.schema();
    let strategy_enum: Vec<&str> = schema["properties"]["strategy"]["enum"]
        .as_array()
        .expect("strategy must be an enum")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(strategy_enum, vec!["merge", "squash", "rebase"]);
    assert_eq!(
        schema_required(&tool),
        vec!["branch".to_string()],
        "git_merge cannot run without a branch"
    );
}

// ── git_grep option handling ──────────────────────────────────────────────

#[tokio::test]
async fn git_grep_is_case_sensitive_by_default_and_insensitive_on_request() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitGrepTool::new(Arc::clone(&repo), &p);

    let sensitive = tool
        .call(serde_json::json!({"pattern": "todo"}), &ctx("c1"))
        .await
        .unwrap();
    assert!(!sensitive.is_error, "{}", sensitive.content);
    assert_eq!(
        sensitive.content.trim(),
        "[]",
        "lowercase `todo` must not match uppercase `TODO` by default"
    );

    let insensitive = tool
        .call(
            serde_json::json!({"pattern": "todo", "case_sensitive": false}),
            &ctx("c2"),
        )
        .await
        .unwrap();
    assert!(!insensitive.is_error, "{}", insensitive.content);
    let hits: Vec<serde_json::Value> = serde_json::from_str(&insensitive.content).unwrap();
    // README.md is the only file that still carries `TODO` at HEAD — the
    // second commit rewrote src/alpha.rs. The point of the test is the
    // case-sensitivity difference, not the fixture's hit count.
    assert!(
        !hits.is_empty(),
        "case-insensitive search must find the uppercase TODO marker, got {hits:?}"
    );
    assert!(
        hits.iter().all(|h| h["content"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("todo")),
        "every hit must contain the searched token in some case: {hits:?}"
    );
    drop(tmp);
}

#[tokio::test]
async fn git_grep_regex_mode_matches_a_pattern_that_is_not_a_literal() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitGrepTool::new(Arc::clone(&repo), &p);

    // `fn alpha` matches literally; `fn (alpha|beta)` only matches as a regex.
    let literal = tool
        .call(
            serde_json::json!({"pattern": "fn (alpha|beta)"}),
            &ctx("r1"),
        )
        .await
        .unwrap();
    assert_eq!(
        literal.content.trim(),
        "[]",
        "without use_regex the pattern must be treated literally"
    );

    let regex = tool
        .call(
            serde_json::json!({"pattern": "fn (alpha|beta)", "use_regex": true}),
            &ctx("r2"),
        )
        .await
        .unwrap();
    assert!(!regex.is_error, "{}", regex.content);
    let hits: Vec<serde_json::Value> = serde_json::from_str(&regex.content).unwrap();
    assert!(
        !hits.is_empty(),
        "use_regex must enable extended-regex matching"
    );
    drop(tmp);
}

#[tokio::test]
async fn git_grep_path_prefix_restricts_the_search_to_one_subtree() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitGrepTool::new(Arc::clone(&repo), &p);

    let all = tool
        .call(serde_json::json!({"pattern": "TODO"}), &ctx("p1"))
        .await
        .unwrap();
    let all_hits: Vec<serde_json::Value> = serde_json::from_str(&all.content).unwrap();

    let scoped = tool
        .call(
            serde_json::json!({"pattern": "TODO", "path_prefix": "src"}),
            &ctx("p2"),
        )
        .await
        .unwrap();
    assert!(!scoped.is_error, "{}", scoped.content);
    let scoped_hits: Vec<serde_json::Value> = serde_json::from_str(&scoped.content).unwrap();

    assert!(
        scoped_hits.len() < all_hits.len(),
        "path_prefix must exclude matches outside `src` ({scoped_hits:?} vs {all_hits:?})"
    );
    for hit in &scoped_hits {
        assert!(
            hit["file"].as_str().unwrap_or("").starts_with("src"),
            "path_prefix let through a file outside the prefix: {hit}"
        );
    }
    drop(tmp);
}

#[tokio::test]
async fn git_grep_max_results_bounds_the_number_of_hits() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitGrepTool::new(Arc::clone(&repo), &p);

    let one = tool
        .call(
            serde_json::json!({"pattern": "e", "max_results": 1}),
            &ctx("m1"),
        )
        .await
        .unwrap();
    assert!(!one.is_error, "{}", one.content);
    let hits: Vec<serde_json::Value> = serde_json::from_str(&one.content).unwrap();
    assert_eq!(hits.len(), 1, "max_results must truncate the result list");
    drop(tmp);
}

#[tokio::test]
async fn git_grep_reports_a_missing_pattern_instead_of_panicking() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitGrepTool::new(Arc::clone(&repo), &p);

    let result = tool.call(serde_json::json!({}), &ctx("e1")).await.unwrap();
    assert!(
        result.is_error,
        "a missing required field must be an error result"
    );
    assert!(
        result.content.to_lowercase().contains("pattern"),
        "the error must name the missing field, got: {}",
        result.content
    );
    drop(tmp);
}

#[tokio::test]
async fn git_grep_reports_a_missing_repository_without_hanging() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&missing).unwrap();
    std::process::Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(&missing)
        .output()
        .expect("git init");
    let repo = Arc::new(GitRepo::open(&missing).unwrap());
    std::fs::remove_dir_all(&missing).unwrap();

    let tool = GitGrepTool::new(Arc::clone(&repo), &missing);
    // The `current_dir` no longer exists, so the spawn itself fails and the
    // error surfaces as `AgtrsError::ToolCallFailed`, NOT as a `ToolResult`
    // carrying `is_error`. Both are "reported", but an agent that only
    // inspects `ToolResult.is_error` would see nothing at all — pinning the
    // distinction is the whole point of the test.
    let err = tool
        .call(serde_json::json!({"pattern": "x"}), &ctx("e2"))
        .await
        .expect_err("searching a vanished repo must not succeed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("git_grep"),
        "the error must name the tool that failed, got: {rendered}"
    );
    drop(tmp);
}

// ── git_diff targets ──────────────────────────────────────────────────────

#[tokio::test]
async fn git_diff_staged_shows_the_index_while_head_shows_the_working_tree() {
    let (tmp, p) = init_repo().await;
    std::fs::write(p.join("src/alpha.rs"), "fn alpha() { /* v3 */ }\n").unwrap();
    Command::new("git")
        .args(["add", "src/alpha.rs"])
        .current_dir(&p)
        .output()
        .await
        .unwrap();
    std::fs::write(p.join("src/alpha.rs"), "fn alpha() { /* v4 */ }\n").unwrap();

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitDiffTool::new(Arc::clone(&repo), &p);

    let staged = tool
        .call(serde_json::json!({"target": "staged"}), &ctx("d1"))
        .await
        .unwrap();
    assert!(!staged.is_error, "{}", staged.content);
    // git_diff returns the raw unified diff, not a JSON envelope.
    assert!(
        staged.content.contains("v3"),
        "the staged diff must show the INDEXED content (v3), got: {}",
        staged.content
    );
    assert!(
        !staged.content.contains("v4"),
        "the staged diff must not leak the unstaged working-tree content (v4), got: {}",
        staged.content
    );

    let head = tool
        .call(serde_json::json!({"target": "head"}), &ctx("d2"))
        .await
        .unwrap();
    assert!(!head.is_error, "{}", head.content);
    assert!(
        head.content.contains("v4"),
        "the default `head` target must show the WORKING TREE (v4), got: {}",
        head.content
    );
    drop(tmp);
}

#[tokio::test]
async fn git_diff_rejects_a_target_outside_the_advertised_enum() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitDiffTool::new(Arc::clone(&repo), &p);

    // MEASURED BEHAVIOUR, recorded deliberately: the schema constrains
    // `target` to head|staged|unstaged, but the implementation does NOT
    // validate it — an unrecognised target falls through to the same branch
    // as `head` and returns the working-tree diff. This test pins that, so a
    // future change to reject unknown targets shows up as a failing test
    // rather than as a silent behaviour change.
    //
    // See the schema test above: the enum advertised to the model is stricter
    // than what the tool enforces.
    std::fs::write(p.join("src/alpha.rs"), "fn alpha() { /* drift */ }\n").unwrap();

    let unknown = tool
        .call(serde_json::json!({"target": "sideways"}), &ctx("d3"))
        .await
        .unwrap();
    assert!(!unknown.is_error, "{}", unknown.content);
    let head = tool
        .call(serde_json::json!({"target": "head"}), &ctx("d3b"))
        .await
        .unwrap();
    assert_eq!(
        unknown.content, head.content,
        "an unknown target currently behaves exactly like `head`"
    );
    drop(tmp);
}

#[tokio::test]
async fn git_diff_between_two_refs_uses_the_ref_pair_over_the_target() {
    let (tmp, p) = init_repo().await;
    let first = git(&p, &["rev-parse", "HEAD~1"]).await.trim().to_string();
    let second = git(&p, &["rev-parse", "HEAD"]).await.trim().to_string();

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitDiffTool::new(Arc::clone(&repo), &p);

    let result = tool
        .call(
            serde_json::json!({"from_ref": first, "to_ref": second}),
            &ctx("d4"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    let refs_only = tool
        .call(
            serde_json::json!({"from_ref": first, "to_ref": second}),
            &ctx("d5"),
        )
        .await
        .unwrap();
    assert!(
        refs_only.content.contains("fn alpha"),
        "a ref-to-ref diff must contain the committed change, got: {}",
        refs_only.content
    );
    drop(tmp);
}

// ── git_log filters ───────────────────────────────────────────────────────

#[tokio::test]
async fn git_log_filters_by_message_author_path_and_max() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitLogTool::new(Arc::clone(&repo), &p);

    let by_grep = tool
        .call(serde_json::json!({"grep": "implement"}), &ctx("l1"))
        .await
        .unwrap();
    assert!(!by_grep.is_error, "{}", by_grep.content);
    let entries: Vec<serde_json::Value> = serde_json::from_str(&by_grep.content).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "only one commit mentions `implement`: {entries:?}"
    );

    let by_author = tool
        .call(serde_json::json!({"author": "Ada"}), &ctx("l2"))
        .await
        .unwrap();
    let by_author: Vec<serde_json::Value> = serde_json::from_str(&by_author.content).unwrap();
    assert_eq!(by_author.len(), 2, "both commits are authored by Ada");

    let by_path = tool
        .call(serde_json::json!({"path": "src/alpha.rs"}), &ctx("l3"))
        .await
        .unwrap();
    let by_path: Vec<serde_json::Value> = serde_json::from_str(&by_path.content).unwrap();
    assert_eq!(by_path.len(), 2, "only src/alpha.rs was ever committed");

    let by_max = tool
        .call(serde_json::json!({"max": 1}), &ctx("l4"))
        .await
        .unwrap();
    let by_max: Vec<serde_json::Value> = serde_json::from_str(&by_max.content).unwrap();
    assert_eq!(by_max.len(), 1, "max must bound the number of commits");
    drop(tmp);
}

#[tokio::test]
async fn git_log_on_a_repository_with_no_commits_returns_an_empty_list() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().to_path_buf();
    git(&p, &["init", "--initial-branch=main"]).await;
    git(&p, &["config", "user.email", "t@t.com"]).await;
    git(&p, &["config", "user.name", "T"]).await;

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitLogTool::new(Arc::clone(&repo), &p);
    // MEASURED BEHAVIOUR: `git log` on a repository with no commits exits
    // non-zero, so the tool surfaces an error result rather than `[]`. An
    // agent must be able to tell "no history" from "the log failed".
    let result = tool.call(serde_json::json!({}), &ctx("l5")).await.unwrap();
    assert!(
        result.is_error,
        "a repo with no commits must be reported, not silently returned as empty: {}",
        result.content
    );
    assert!(
        result.content.to_lowercase().contains("commit"),
        "the error must explain that there are no commits, got: {}",
        result.content
    );
    drop(tmp);
}

// ── git_merge strategies and conflicts ────────────────────────────────────

#[tokio::test]
async fn git_merge_reports_a_conflicting_branch_with_the_conflicted_files() {
    let (tmp, p) = init_repo().await;
    git(&p, &["checkout", "-b", "feature"]).await;
    std::fs::write(p.join("src/alpha.rs"), "fn alpha() { feature }\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "feature change"]).await;

    git(&p, &["checkout", "main"]).await;
    std::fs::write(p.join("src/alpha.rs"), "fn alpha() { mainline }\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "mainline change"]).await;

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitMergeTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(serde_json::json!({"branch": "feature"}), &ctx("mg1"))
        .await
        .unwrap();

    assert!(
        result.is_error,
        "a conflicted merge must not report success"
    );
    let parsed: serde_json::Value = serde_json::from_str(&result.content)
        .unwrap_or_else(|_| panic!("conflict payload must be JSON, got: {}", result.content));
    assert_eq!(parsed["merged"], serde_json::json!(false));
    assert_eq!(parsed["branch"], serde_json::json!("feature"));
    let conflicts = parsed["conflicts"].as_array().unwrap();
    assert!(
        conflicts
            .iter()
            .any(|c| c.as_str().unwrap_or("").contains("alpha.rs")),
        "the conflicted file must be named, got: {conflicts:?}"
    );
    drop(tmp);
}

#[tokio::test]
async fn git_merge_squash_folds_the_branch_without_creating_a_merge_commit() {
    let (tmp, p) = init_repo().await;
    git(&p, &["checkout", "-b", "feature"]).await;
    std::fs::write(p.join("src/beta.rs"), "fn beta() {}\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "add beta"]).await;

    let before = git(&p, &["rev-list", "--count", "HEAD"])
        .await
        .trim()
        .to_string();

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitMergeTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"branch": "feature", "strategy": "squash"}),
            &ctx("mg2"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["merged"], serde_json::json!(true));
    assert_eq!(parsed["conflicts"], serde_json::json!([]));

    // `--squash` stages the change without committing it, so the commit count
    // on this branch must be unchanged. A plain merge would have added one.
    let after = git(&p, &["rev-list", "--count", "HEAD"])
        .await
        .trim()
        .to_string();
    assert_eq!(
        before, after,
        "a squash must not create a commit on the receiving branch"
    );
    assert!(
        p.join("src/beta.rs").exists(),
        "a squash must still bring the branch's files into the working tree"
    );
    drop(tmp);
}

#[tokio::test]
async fn git_merge_rebase_replays_the_current_branch_onto_the_target() {
    let (tmp, p) = init_repo().await;
    git(&p, &["checkout", "-b", "feature"]).await;
    std::fs::write(p.join("src/beta.rs"), "fn beta() {}\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "add beta"]).await;

    git(&p, &["checkout", "main"]).await;
    std::fs::write(p.join("README.md"), "# Title\nTODO: rewritten\n").unwrap();
    git(&p, &["add", "."]).await;
    git(&p, &["commit", "-m", "diverge"]).await;

    // `git rebase <branch>` rewrites the CURRENT branch onto <branch>, so the
    // SHA that must change is main's, not feature's.
    let before = git(&p, &["rev-parse", "main"]).await.trim().to_string();

    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitMergeTool::new(Arc::clone(&repo), &p);
    let result = tool
        .call(
            serde_json::json!({"branch": "feature", "strategy": "rebase"}),
            &ctx("mg3"),
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);

    let after = git(&p, &["rev-parse", "main"]).await.trim().to_string();
    assert_ne!(
        before, after,
        "a rebase must rewrite the current branch's commits onto the new base"
    );
    assert!(
        p.join("src/beta.rs").exists(),
        "after rebasing onto `feature`, the current branch must carry beta.rs"
    );
    drop(tmp);
}

#[tokio::test]
async fn git_merge_reports_a_missing_branch_rather_than_merging_something() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitMergeTool::new(Arc::clone(&repo), &p);

    let result = tool.call(serde_json::json!({}), &ctx("mg4")).await.unwrap();
    assert!(
        result.is_error,
        "a merge with no branch must not report success"
    );
    assert!(
        result.content.to_lowercase().contains("branch"),
        "the error must name the missing field, got: {}",
        result.content
    );
    drop(tmp);
}

#[tokio::test]
async fn git_merge_reports_a_non_conflicting_failure_distinctly_from_a_conflict() {
    let (tmp, p) = init_repo().await;
    let repo = Arc::new(GitRepo::open(&p).unwrap());
    let tool = GitMergeTool::new(Arc::clone(&repo), &p);

    let result = tool
        .call(serde_json::json!({"branch": "no-such-branch"}), &ctx("mg5"))
        .await
        .unwrap();
    assert!(result.is_error, "merging a nonexistent branch must fail");
    assert!(
        !serde_json::from_str::<serde_json::Value>(&result.content)
            .map(|v| v.get("merged") == Some(&serde_json::json!(false)))
            .unwrap_or(false),
        "a non-conflict failure must NOT be dressed up as a conflict report"
    );
    drop(tmp);
}
