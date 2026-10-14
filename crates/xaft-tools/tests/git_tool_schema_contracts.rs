//! Schema-contract and safety-flag coverage for the git tools.
//!
//! `git_tools_integration.rs` proves the happy path for a handful of tools and
//! `git_tool_edge_cases.rs` covers `git_grep` / `git_diff` / `git_log` /
//! `git_merge` in depth. The remaining fifteen git tools had no test that ever
//! called `schema()`, `description()` or `requires_confirmation()`, so their
//! published interface to the model was entirely unverified.
//!
//! That interface is not decorative — it is the only thing the model sees:
//!
//!   * every key the `call` body reads must appear in `properties`, or the
//!     option is unreachable in practice;
//!   * `required` must name exactly the fields the tool refuses to run
//!     without. Too few and the model hits a runtime error it could have been
//!     warned about; too many and a usable default becomes impossible to omit;
//!   * `requires_confirmation` is a safety flag. A destructive tool that
//!     reports `false` runs without a prompt.
//!
//! Every expectation below was read off the tool's own `call` body, and the
//! error-path tests assert on the message a caller actually receives.

use std::path::Path;
use std::sync::Arc;

use agtrs_git::GitRepo;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext};
use tempfile::TempDir;

use xaft_tools::git::GitTools;
use xaft_tools::{
    GitAddTool, GitBlameTool, GitBranchTool, GitCheckoutFilesTool, GitCommitStagedTool,
    GitCreateBranchTool, GitDiffTool, GitGrepTool, GitLogTool, GitMergeTool, GitPushTool,
    GitRemoteTool, GitShowTool, GitStashListTool, GitStashPopTool, GitStashTool, GitStatusTool,
    GitTagTool, GitUnstageTool,
};

/// `(tool name, properties call() reads, required fields, needs confirmation)`.
const CONTRACTS: &[(&str, &[&str], &[&str], bool)] = &[
    ("git_add", &["paths", "all"], &[], false),
    (
        "git_blame",
        &["path", "start_line", "end_line"],
        &["path"],
        false,
    ),
    ("git_branch", &["all", "verbose"], &[], false),
    (
        "git_checkout_files",
        &["paths", "ref"],
        &["paths"],
        true,
    ),
    (
        "git_commit_staged",
        &["message", "allow_empty"],
        &["message"],
        true,
    ),
    (
        "git_create_branch",
        &["name", "from_ref", "checkout"],
        &["name"],
        false,
    ),
    (
        "git_diff",
        &["target", "path", "from_ref", "to_ref"],
        &[],
        false,
    ),
    (
        "git_grep",
        &[
            "pattern",
            "path_prefix",
            "case_sensitive",
            "use_regex",
            "max_results",
        ],
        &["pattern"],
        false,
    ),
    (
        "git_log",
        &["max", "path", "author", "since", "grep"],
        &[],
        false,
    ),
    (
        "git_merge",
        &["branch", "strategy", "message"],
        &["branch"],
        true,
    ),
    (
        "git_push",
        &["remote", "branch", "force", "set_upstream"],
        &[],
        true,
    ),
    ("git_remote", &[], &[], false),
    ("git_show", &["ref", "stat_only"], &[], false),
    ("git_stash", &["message", "include_untracked"], &[], false),
    ("git_stash_list", &[], &[], false),
    ("git_stash_pop", &["index"], &[], true),
    ("git_status", &[], &[], false),
    ("git_tag", &["pattern"], &[], false),
    ("git_unstage", &["paths", "all"], &[], false),
];

const FIXTURE: &str = "# xaft fixture\n";

/// A throwaway repository with one commit.
///
/// Never `GitRepo::init(".")`: that would drop a real `.git` inside the xaft
/// checkout. A `TempDir` keeps the blast radius at zero.
fn scratch_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("README.md"), FIXTURE).unwrap();
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.email", "test@test.com"],
        vec!["config", "user.name", "Ada Lovelace"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "."],
        vec!["commit", "-m", "initial import"],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(tmp.path())
            .output()
            .expect("git must be runnable");
        assert!(
            out.status.success(),
            "fixture setup `git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    tmp
}

fn open_repo(dir: &TempDir) -> Arc<GitRepo> {
    Arc::new(GitRepo::open(dir.path()).expect("fixture must be a repository"))
}

/// Every git tool, erased to a common type so one table can drive them all.
fn all_git_tools(repo: &Arc<GitRepo>, path: &Path) -> Vec<Arc<ErasedTool>> {
    vec![
        Arc::new(GitAddTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitBlameTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitBranchTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitCheckoutFilesTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitCommitStagedTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitCreateBranchTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitDiffTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitGrepTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitLogTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitMergeTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitPushTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitRemoteTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitShowTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitStashTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitStashListTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitStashPopTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitStatusTool::new(Arc::clone(repo))) as Arc<ErasedTool>,
        Arc::new(GitTagTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
        Arc::new(GitUnstageTool::new(Arc::clone(repo), path)) as Arc<ErasedTool>,
    ]
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

// ── Schema contracts ────────────────────────────────────────────────────────

#[test]
fn every_git_tool_publishes_the_options_its_call_body_reads() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let tools = all_git_tools(&repo, dir.path());

    assert_eq!(
        tools.len(),
        CONTRACTS.len(),
        "the table must cover every git tool; add a row when a tool is added"
    );

    for (tool, (name, properties, required, _)) in tools.iter().zip(CONTRACTS) {
        assert_eq!(tool.name(), *name, "tool names are the model's only handle");

        let schema = tool.schema();
        assert_eq!(
            schema["type"], "object",
            "{name}: a tool schema must be a JSON object"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "{name}: unknown options must be rejected, not silently ignored"
        );

        let declared = sorted(
            schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{name}: schema must declare `properties`"))
                .keys()
                .cloned()
                .collect(),
        );
        let expected = sorted(properties.iter().map(|s| s.to_string()).collect());
        assert_eq!(
            declared, expected,
            "{name}: the advertised properties must be exactly what call() reads"
        );

        let advertised_required = sorted(
            schema["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        );
        let expected_required = sorted(required.iter().map(|s| s.to_string()).collect());
        assert_eq!(
            advertised_required, expected_required,
            "{name}: `required` must name exactly the fields call() refuses to run without"
        );

        assert!(
            !tool.description().is_empty(),
            "{name}: a tool the model cannot read about is unusable"
        );
    }
}

#[test]
fn destructive_git_tools_require_confirmation_and_read_only_ones_do_not() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let tools = all_git_tools(&repo, dir.path());

    for (tool, (name, _, _, needs_confirmation)) in tools.iter().zip(CONTRACTS) {
        assert_eq!(
            tool.requires_confirmation(),
            *needs_confirmation,
            "{name}: the confirmation flag is a safety property, not a hint"
        );
    }

    // Pin the two that would be actively dangerous if the flag regressed.
    for name in ["git_push", "git_checkout_files"] {
        let tool = tools
            .iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("{name} must be in the tool set"));
        assert!(
            tool.requires_confirmation(),
            "{name} must never run unattended"
        );
    }
}

// ── The `GitTools` builder ──────────────────────────────────────────────────

#[test]
fn git_tools_builder_exposes_the_documented_tool_sets() {
    let dir = scratch_repo();
    let builder = GitTools::open(dir.path()).expect("fixture must be a repository");

    let all_tools = builder.all();
    let all: Vec<&str> = all_tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        all,
        vec!["git_status", "git_diff", "git_log"],
        "`all()` is the read-only trio, in a stable order"
    );

    assert_eq!(builder.status().name(), "git_status");

    let pair_tools = builder.status_and_diff();
    let pair: Vec<&str> = pair_tools.iter().map(|t| t.name()).collect();
    assert_eq!(pair, vec!["git_status", "git_diff"]);
}

#[test]
fn git_tools_builder_refuses_a_directory_that_is_not_a_repository() {
    let dir = TempDir::new().unwrap();
    assert!(
        GitTools::open(dir.path()).is_err(),
        "opening a non-repository must fail rather than fabricate a repo"
    );
}

// ── Error paths: the message a caller actually receives ─────────────────────

#[tokio::test]
async fn git_create_branch_rejects_an_empty_name() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let tool = GitCreateBranchTool::new(Arc::clone(&repo), dir.path());

    // `require_str` accepts an empty string, so the empty-name guard is the
    // only thing standing between "" and `git checkout -b ''`.
    let result = tool
        .call(serde_json::json!({ "name": "" }), &ToolContext::new("cb1"))
        .await
        .unwrap();
    assert!(result.is_error, "an empty branch name must be rejected");
    assert!(
        result.content.contains("must not be empty"),
        "the error must say why, got: {}",
        result.content
    );
}

#[tokio::test]
async fn git_create_branch_honours_from_ref_and_the_checkout_flag() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let tool = GitCreateBranchTool::new(Arc::clone(&repo), dir.path());

    // checkout: false -> `git branch <name> <ref>`, leaving HEAD alone.
    let no_checkout = tool
        .call(
            serde_json::json!({ "name": "feature/no-checkout", "from_ref": "HEAD", "checkout": false }),
            &ToolContext::new("cb2"),
        )
        .await
        .unwrap();
    assert!(!no_checkout.is_error, "{}", no_checkout.content);
    let parsed: serde_json::Value = serde_json::from_str(&no_checkout.content).unwrap();
    assert_eq!(parsed["checked_out"], serde_json::json!(false));
    assert_eq!(parsed["created"], serde_json::json!(true));

    // checkout defaults to true -> `git checkout -b <name> <ref>`.
    let checked_out = tool
        .call(
            serde_json::json!({ "name": "feature/checked-out", "from_ref": "HEAD" }),
            &ToolContext::new("cb3"),
        )
        .await
        .unwrap();
    assert!(!checked_out.is_error, "{}", checked_out.content);
    let parsed: serde_json::Value = serde_json::from_str(&checked_out.content).unwrap();
    assert_eq!(
        parsed["checked_out"],
        serde_json::json!(true),
        "omitting `checkout` must check the branch out"
    );
}

#[tokio::test]
async fn git_add_and_git_unstage_require_paths_or_all() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let ctx = ToolContext::new("au1");

    let add = GitAddTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(add.is_error, "`git_add` with no arguments must fail");
    assert!(
        add.content.contains("'paths' or 'all'"),
        "got: {}",
        add.content
    );

    let unstage = GitUnstageTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(unstage.is_error, "`git_unstage` with no arguments must fail");
    assert!(
        unstage.content.contains("'paths' or 'all'"),
        "got: {}",
        unstage.content
    );
}

#[tokio::test]
async fn git_commit_staged_rejects_an_empty_message_and_reports_nothing_staged() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let tool = GitCommitStagedTool::new(Arc::clone(&repo), dir.path());
    let ctx = ToolContext::new("cs1");

    let blank = tool
        .call(serde_json::json!({ "message": "   " }), &ctx)
        .await
        .unwrap();
    assert!(blank.is_error, "a whitespace-only message must be rejected");
    assert!(
        blank.content.contains("must not be empty"),
        "got: {}",
        blank.content
    );

    // Nothing is staged in a freshly created fixture, and allow_empty is off,
    // so the tool must say so instead of creating an empty commit.
    let nothing_staged = tool
        .call(serde_json::json!({ "message": "feat: nothing" }), &ctx)
        .await
        .unwrap();
    assert!(nothing_staged.is_error, "nothing is staged; this must fail");
    assert!(
        nothing_staged.content.contains("nothing staged"),
        "the error must be actionable, got: {}",
        nothing_staged.content
    );
    assert!(
        nothing_staged.content.contains("allow_empty"),
        "the error must name the escape hatch, got: {}",
        nothing_staged.content
    );
}

#[tokio::test]
async fn git_push_refuses_to_force_push_a_protected_branch() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let tool = GitPushTool::new(Arc::clone(&repo), dir.path());

    for protected in ["main", "master"] {
        // No remote is configured, so if the guard did NOT fire this would try
        // to reach the network. The guard must return first.
        let result = tool
            .call(
                serde_json::json!({ "branch": protected, "force": true }),
                &ToolContext::new("p1"),
            )
            .await
            .unwrap();
        assert!(
            result.is_error,
            "force-pushing to `{protected}` must be refused"
        );
        assert!(
            result.content.contains("protected branch"),
            "got: {}",
            result.content
        );
    }
}

#[tokio::test]
async fn git_blame_and_git_checkout_files_name_the_field_they_require() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let ctx = ToolContext::new("bf1");

    let blame = GitBlameTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(blame.is_error, "`git_blame` cannot run without a path");
    assert!(blame.content.contains("path"), "got: {}", blame.content);

    let checkout = GitCheckoutFilesTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(
        checkout.is_error,
        "`git_checkout_files` cannot run without paths"
    );
    assert!(
        checkout.content.contains("paths"),
        "got: {}",
        checkout.content
    );
}

// ── Read-only tools answer on a real repository ─────────────────────────────

#[tokio::test]
async fn read_only_git_tools_answer_on_a_real_repository() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let ctx = ToolContext::new("ro1");

    let status = GitStatusTool::new(Arc::clone(&repo))
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(!status.is_error, "git_status: {}", status.content);

    let remote = GitRemoteTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(!remote.is_error, "git_remote: {}", remote.content);

    let branch = GitBranchTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(!branch.is_error, "git_branch: {}", branch.content);
    assert!(
        branch.content.contains("main"),
        "the current branch must be reported, got: {}",
        branch.content
    );

    let show = GitShowTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({ "ref": "HEAD" }), &ctx)
        .await
        .unwrap();
    assert!(!show.is_error, "git_show: {}", show.content);

    let tag = GitTagTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(!tag.is_error, "git_tag: {}", tag.content);

    let blame = GitBlameTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({ "path": "README.md" }), &ctx)
        .await
        .unwrap();
    assert!(!blame.is_error, "git_blame: {}", blame.content);
    let lines: Vec<serde_json::Value> = serde_json::from_str(&blame.content).unwrap();
    assert_eq!(lines.len(), 1, "README.md has exactly one line");

    // The line-range filter must be a real filter, not decoration.
    let none = GitBlameTool::new(Arc::clone(&repo), dir.path())
        .call(
            serde_json::json!({ "path": "README.md", "start_line": 99 }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!none.is_error, "{}", none.content);
    let none: Vec<serde_json::Value> = serde_json::from_str(&none.content).unwrap();
    assert!(
        none.is_empty(),
        "start_line beyond the file must yield no lines, got: {none:?}"
    );
}

#[tokio::test]
async fn git_stash_round_trips_through_list_and_pop() {
    let dir = scratch_repo();
    let repo = open_repo(&dir);
    let ctx = ToolContext::new("st1");
    let readme = dir.path().join("README.md");

    std::fs::write(&readme, "# changed\n").unwrap();

    let stash = GitStashTool::new(Arc::clone(&repo), dir.path())
        .call(
            serde_json::json!({ "message": "wip: readme" }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!stash.is_error, "git_stash: {}", stash.content);
    let parsed: serde_json::Value = serde_json::from_str(&stash.content).unwrap();
    assert_eq!(parsed["stash_ref"], serde_json::json!("stash@{0}"));
    assert_eq!(
        parsed["message"],
        serde_json::json!("wip: readme"),
        "an explicit stash message must be echoed back"
    );
    assert_eq!(
        std::fs::read_to_string(&readme).unwrap(),
        FIXTURE,
        "stashing must restore the working tree"
    );

    let list = GitStashListTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(!list.is_error, "git_stash_list: {}", list.content);
    assert!(
        list.content.contains("wip: readme"),
        "the stash must be listed, got: {}",
        list.content
    );

    let pop = GitStashPopTool::new(Arc::clone(&repo), dir.path())
        .call(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(!pop.is_error, "git_stash_pop: {}", pop.content);
    assert_eq!(
        std::fs::read_to_string(&readme).unwrap(),
        "# changed\n",
        "popping must bring the stashed change back"
    );
}
