//! Handler tests for `xaft skills add` (parity matrix row 46).
//!
//! The handler is driven through `render_add` with a fake fetcher, so no test
//! touches the network, and with an explicit project and global directory, so
//! no test touches the operator's home. What is pinned:
//!
//! * the exact `Installed skill <slug> (<scope>) at <path>` line;
//! * that an installed skill is one the real `SkillLoader` then loads — the
//!   property that makes the command more than a file copier;
//! * agenthicc's `--skill a,b` / `--all` argument shape;
//! * failures print `error: …` and exit 1 (the recorded divergence from
//!   agenthicc's exit 0).

use std::path::Path;

use clap::Parser;
use xaft_cli::args::{Commands, SkillsAddArgs, SkillsSubcommand, XaftCli};
use xaft_cli::commands::skills::{install_request, render_add};
use xaft_runtime::ExitCode;
use xaft_skills::SkillLoader;
use xaft_skills::installer::{SkillFetcher, SkillInstallError};

/// Serves one canned body for one URL; anything else is a network error.
struct OneUrl(&'static str, &'static str);

impl SkillFetcher for OneUrl {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, SkillInstallError> {
        if url == self.0 {
            Ok(self.1.as_bytes().to_vec())
        } else {
            Err(SkillInstallError(
                "skill download failed due to a network error".into(),
            ))
        }
    }
}

fn add(source: &str) -> SkillsAddArgs {
    SkillsAddArgs {
        source: source.into(),
        ..Default::default()
    }
}

fn write_skill(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

async fn run(args: &SkillsAddArgs, project: &Path, global: &Path) -> (ExitCode, String) {
    let mut out = String::new();
    let code = render_add(args, project, Some(global), &OneUrl("", ""), &mut out).await;
    (code, out)
}

#[test]
fn skills_add_parses_agenthicc_flags() {
    let cli = XaftCli::try_parse_from([
        "xaft",
        "skills",
        "add",
        "owner/repo",
        "--skill",
        "a, b,,c",
        "--name",
        "x",
        "--global",
    ])
    .unwrap();
    let Some(Commands::Skills(cmd)) = cli.command else {
        panic!("expected Skills");
    };
    let SkillsSubcommand::Add(args) = cmd.subcommand;
    assert!(args.global && !args.project);
    let request = install_request(&args, Path::new("/p"), None);
    assert_eq!(request.skill_names, vec!["a", "b", "c"]);
    assert_eq!(request.name, "x");

    let all = SkillsAddArgs { all: true, ..args };
    assert!(
        install_request(&all, Path::new("/p"), None)
            .skill_names
            .is_empty()
    );
}

#[test]
fn global_and_project_together_is_a_usage_error() {
    let err = XaftCli::try_parse_from(["xaft", "skills", "add", "x", "--global", "--project"])
        .unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[tokio::test]
async fn a_local_skill_installs_and_the_loader_then_sees_it() {
    let project = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join("review"),
        "---\nname: review\ndescription: Review diffs\n---\nRead the whole diff first.\n",
    );

    let (code, out) = run(&add("review"), project.path(), global.path()).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let installed = project.path().join(".xaft/skills/review");
    assert_eq!(
        out,
        format!(
            "Installed skill review (project) at {}\n",
            installed.display()
        )
    );

    let skills = SkillLoader::for_working_dir(project.path())
        .load_all()
        .await;
    let review = skills.iter().find(|s| s.name == "review").expect("loaded");
    assert_eq!(review.description, "Review diffs");
}

#[tokio::test]
async fn global_scope_reports_global_and_lands_in_the_global_directory() {
    let project = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    write_skill(&project.path().join("fmt"), "Format before committing.\n");
    let args = SkillsAddArgs {
        global: true,
        ..add("fmt")
    };
    let (code, out) = run(&args, project.path(), global.path()).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("(global) at"), "{out}");
    assert!(global.path().join("fmt/SKILL.md").is_file());
}

#[tokio::test]
async fn a_repository_source_prints_one_line_per_skill() {
    let project = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    let repo = project.path().join("vendor-skills");
    write_skill(&repo.join("skills/one"), "one\n");
    write_skill(&repo.join("skills/two"), "two\n");
    let (code, out) = run(&add("vendor-skills"), project.path(), global.path()).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("Installed skill one (project) at "));
    assert!(lines[1].starts_with("Installed skill two (project) at "));
}

#[tokio::test]
async fn a_remote_skill_md_is_fetched_and_installed() {
    let project = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    let mut out = String::new();
    let fetcher = OneUrl(
        "https://example.com/skills/deploy/SKILL.md",
        "Deploy with care.\n",
    );
    let code = render_add(
        &add("https://example.com/skills/deploy/SKILL.md"),
        project.path(),
        Some(global.path()),
        &fetcher,
        &mut out,
    )
    .await;
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    assert!(out.starts_with("Installed skill deploy (project) at "));
}

#[tokio::test]
async fn a_failure_prints_error_and_exits_one() {
    let project = tempfile::tempdir().unwrap();
    let global = tempfile::tempdir().unwrap();
    let (code, out) = run(&add("./nowhere"), project.path(), global.path()).await;
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(out, "error: skill source does not exist: ./nowhere\n");

    let (code, out) = run(
        &add("http://example.com/x/SKILL.md"),
        project.path(),
        global.path(),
    )
    .await;
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(out, "error: remote skill sources must use HTTPS\n");
}
