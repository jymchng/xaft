//! Handler tests for `xaft worktrees` (parity matrix rows 50–53).
//!
//! Nothing here is mocked: every test builds a real Git repository with the
//! system `git`, creates an orchestration through the public
//! `ParallelCoordinator` API — the same writer agenthicc's worker tooling
//! uses — and then drives the CLI handler, `render_worktrees`, against the
//! same `ManifestStore`. The manifest store and the worktree root are temp
//! directories, so no test touches `~/.xaft`.
//!
//! What is pinned:
//!
//! * `list`'s exact line shape and empty state, and `--json`'s
//!   `indent=2, sort_keys=True` rendering;
//! * `show`'s not-found message and exit 1;
//! * `integrate` merging real committed work into the parent branch, and a
//!   conflict reported as `"ok": false` with exit 0, as agenthicc does;
//! * `recover` reconciling a manifest with `git worktree list`.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use xaft_cli::args::{Commands, WorktreesSubcommand, XaftCli};
use xaft_cli::commands::worktrees::{NO_ORCHESTRATIONS, render_worktrees};
use xaft_cli::worktrees::{ManifestStore, ParallelCoordinator, WorktreeManager};
use xaft_runtime::ExitCode;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "xaft-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "xaft-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git must be installed");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct World {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    worktrees: PathBuf,
    store: ManifestStore,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "xaft-test"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let worktrees = dir.path().join("worktrees");
        let store = ManifestStore::new(dir.path().join("orchestrations"));
        Self {
            repo: repo.canonicalize().unwrap(),
            worktrees,
            store,
            _dir: dir,
        }
    }

    fn coordinator(&self) -> ParallelCoordinator {
        ParallelCoordinator::new(
            WorktreeManager::with_root(&self.repo, &self.worktrees).unwrap(),
            self.store.clone(),
            "parent-session",
        )
    }

    /// An orchestration with one dispatched task whose worker committed
    /// `file`, with completion evidence recorded.
    fn completed_task(&self, id: &str, file: &str, body: &str) -> PathBuf {
        let c = self.coordinator();
        c.create(Some(id)).unwrap();
        c.add_task(id, "t1", "do the thing", &[]).unwrap();
        let task = c.dispatch(id, "t1").unwrap();
        let path = PathBuf::from(
            &c.manifest(id)
                .unwrap()
                .worktree(&task.worktree_id)
                .unwrap()
                .path,
        );
        std::fs::write(path.join(file), body).unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-q", "-m", "worker"]);
        c.inspect_task(id, "t1").unwrap();
        path
    }

    fn run(&self, sub: WorktreesSubcommand) -> (ExitCode, String) {
        let mut out = String::new();
        let code = render_worktrees(&sub, &self.store, Some(&self.worktrees), &mut out);
        (code, out)
    }
}

#[test]
fn the_four_commands_parse_with_agenthicc_arguments() {
    let cases: Vec<Vec<&str>> = vec![
        vec!["xaft", "worktrees", "list", "--json"],
        vec!["xaft", "worktrees", "show", "abc", "--json"],
        vec!["xaft", "worktrees", "recover", "abc", "--repository", "/r"],
        vec!["xaft", "worktrees", "integrate", "abc", "t1"],
    ];
    for argv in cases {
        let cli = XaftCli::try_parse_from(&argv).unwrap();
        assert!(
            matches!(cli.command, Some(Commands::Worktrees(_))),
            "{argv:?}"
        );
    }
    let cli = XaftCli::try_parse_from(["xaft", "worktrees", "integrate", "o", "t"]).unwrap();
    let Some(Commands::Worktrees(cmd)) = cli.command else {
        unreachable!()
    };
    let WorktreesSubcommand::Integrate { repository, .. } = cmd.subcommand else {
        panic!("expected Integrate")
    };
    assert_eq!(
        repository,
        PathBuf::from("."),
        "--repository defaults to '.'"
    );
}

#[test]
fn list_prints_the_empty_state_and_an_empty_json_array() {
    let world = World::new();
    assert_eq!(
        world.run(WorktreesSubcommand::List { json: false }),
        (ExitCode::SUCCESS, format!("{NO_ORCHESTRATIONS}\n"))
    );
    assert_eq!(
        world.run(WorktreesSubcommand::List { json: true }),
        (ExitCode::SUCCESS, "[]\n".to_string())
    );
}

#[test]
fn list_prints_one_line_per_orchestration_in_agenthiccs_shape() {
    let world = World::new();
    let c = world.coordinator();
    c.create(Some("0123456789abcdef0123")).unwrap();
    c.add_task("0123456789abcdef0123", "a", "x", &[]).unwrap();
    c.add_task("0123456789abcdef0123", "b", "y", &["a".into()])
        .unwrap();

    let (code, out) = world.run(WorktreesSubcommand::List { json: false });
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "0123456789abcdef  planning      2 task(s)  parent=parent-session\n"
    );

    let (_, json) = world.run(WorktreesSubcommand::List { json: true });
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed[0]["orchestration_id"], "0123456789abcdef0123");
    assert_eq!(parsed[0]["tasks"].as_array().unwrap().len(), 2);
    assert!(
        json.starts_with("[\n  {\n    \"base_commit\": "),
        "keys must be sorted and indented by two: {json}"
    );
}

#[test]
fn show_prints_the_manifest_and_a_missing_one_exits_one() {
    let world = World::new();
    world.coordinator().create(Some("o1")).unwrap();
    let (code, out) = world.run(WorktreesSubcommand::Show {
        orchestration_id: "o1".into(),
        json: false,
    });
    assert_eq!(code, ExitCode::SUCCESS);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["parent_branch"], "main");
    assert_eq!(parsed["status"], "planning");

    let (code, out) = world.run(WorktreesSubcommand::Show {
        orchestration_id: "nope".into(),
        json: true,
    });
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(out, "Orchestration not found: nope\n");

    let (code, out) = world.run(WorktreesSubcommand::Show {
        orchestration_id: "../escape".into(),
        json: false,
    });
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(
        out,
        "Unable to show orchestration: invalid orchestration id\n"
    );
}

#[test]
fn integrate_merges_the_workers_commit_into_the_parent_branch() {
    let world = World::new();
    world.completed_task("o1", "feature.txt", "done\n");

    let (code, out) = world.run(WorktreesSubcommand::Integrate {
        orchestration_id: "o1".into(),
        task_id: "t1".into(),
        repository: world.repo.clone(),
    });
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    let result: serde_json::Value = serde_json::from_str(&out).unwrap();
    let keys: Vec<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "branch",
            "conflict_paths",
            "head_commit",
            "message",
            "ok",
            "repository"
        ]
    );
    assert_eq!(result["ok"], true);
    assert!(world.repo.join("feature.txt").is_file());
    assert_eq!(
        result["head_commit"],
        git(&world.repo, &["rev-parse", "HEAD"])
    );

    let manifest = world.store.get("o1").unwrap();
    assert_eq!(manifest.task("t1").unwrap().status.as_str(), "integrated");
}

#[test]
fn a_conflict_is_reported_in_the_result_and_exits_zero() {
    let world = World::new();
    world.completed_task("o1", "README.md", "worker\n");
    std::fs::write(world.repo.join("README.md"), "coordinator\n").unwrap();
    git(&world.repo, &["commit", "-q", "-am", "coordinator"]);

    let (code, out) = world.run(WorktreesSubcommand::Integrate {
        orchestration_id: "o1".into(),
        task_id: "t1".into(),
        repository: world.repo.clone(),
    });
    assert_eq!(code, ExitCode::SUCCESS);
    let result: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["conflict_paths"], serde_json::json!(["README.md"]));
    assert_eq!(world.store.get("o1").unwrap().status.as_str(), "reviewing");
}

#[test]
fn integrate_failures_use_agenthiccs_message_and_exit_one() {
    let world = World::new();
    let (code, out) = world.run(WorktreesSubcommand::Integrate {
        orchestration_id: "missing".into(),
        task_id: "t1".into(),
        repository: world.repo.clone(),
    });
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(out, "Unable to integrate task: missing\n");

    // A task without completion evidence is refused before Git is touched.
    let c = world.coordinator();
    c.create(Some("o2")).unwrap();
    c.add_task("o2", "t1", "x", &[]).unwrap();
    let (code, out) = world.run(WorktreesSubcommand::Integrate {
        orchestration_id: "o2".into(),
        task_id: "t1".into(),
        repository: world.repo.clone(),
    });
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(
        out,
        "Unable to integrate task: task must have completion evidence before integration: t1\n"
    );
}

#[test]
fn recover_reconciles_the_manifest_with_git() {
    let world = World::new();
    let path = world.completed_task("o1", "feature.txt", "done\n");
    std::fs::remove_dir_all(&path).unwrap();

    let (code, out) = world.run(WorktreesSubcommand::Recover {
        orchestration_id: "o1".into(),
        repository: world.repo.clone(),
    });
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    let manifest: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(manifest["status"], "recovering");
    let worktree = &manifest["worktrees"][0];
    assert_eq!(worktree["status"], "orphaned");
    assert_eq!(worktree["last_error"], "worktree path is missing");
    assert_eq!(world.store.get("o1").unwrap().status.as_str(), "recovering");
}

#[test]
fn recover_outside_a_repository_is_refused() {
    let world = World::new();
    world.coordinator().create(Some("o1")).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (code, out) = world.run(WorktreesSubcommand::Recover {
        orchestration_id: "o1".into(),
        repository: elsewhere.path().to_path_buf(),
    });
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert!(
        out.starts_with("Unable to recover orchestration: not a Git repository:"),
        "{out}"
    );
}
