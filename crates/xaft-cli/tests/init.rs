//! Handler tests for `xaft init` (parity matrix row 57).
//!
//! Every test drives the real handler, `render_init`, against a temp
//! directory and asserts on the exact stdout agenthicc's `init_project`
//! prints, plus what landed on disk. The root is passed explicitly, so no
//! test changes the process's working directory.

use clap::Parser;
use xaft_cli::args::{Commands, InitArgs, XaftCli};
use xaft_cli::commands::init::{WRITE_NOTE, render_init};
use xaft_runtime::ExitCode;

fn run(args: &InitArgs, root: &std::path::Path) -> (ExitCode, String) {
    let mut out = String::new();
    let code = render_init(args, root, &mut out);
    (code, out)
}

#[test]
fn init_parses_as_a_top_level_command_with_its_two_flags() {
    let cli = XaftCli::try_parse_from(["xaft", "init", "--force", "--write"]).unwrap();
    let Some(Commands::Init(args)) = cli.command else {
        panic!("expected Init");
    };
    assert!(args.force && args.write);
}

#[test]
fn a_fresh_project_prints_initialized_then_created_lines() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = run(&InitArgs::default(), dir.path());
    assert_eq!(code, ExitCode::SUCCESS);
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(
        out,
        format!(
            "Initialized {}\nCreated .xaft\nCreated AGENTS.md\nCreated .xaft/xaft.toml\n",
            root.display()
        )
    );
    assert!(root.join("AGENTS.md").is_file());
    assert!(root.join(".xaft/xaft.toml").is_file());
}

#[test]
fn a_second_run_preserves_everything() {
    let dir = tempfile::tempdir().unwrap();
    run(&InitArgs::default(), dir.path());
    std::fs::write(dir.path().join("AGENTS.md"), "keep me\n").unwrap();
    let (code, out) = run(&InitArgs::default(), dir.path());
    assert_eq!(code, ExitCode::SUCCESS);
    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(
        lines,
        vec![
            "Preserved .xaft",
            "Preserved AGENTS.md",
            "Preserved .xaft/xaft.toml"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn force_reports_overwrote_lines_after_created_ones() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "old").unwrap();
    let args = InitArgs {
        force: true,
        write: false,
    };
    let (_, out) = run(&args, dir.path());
    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(
        lines,
        vec![
            "Created .xaft",
            "Created .xaft/xaft.toml",
            "Overwrote AGENTS.md"
        ]
    );
}

#[test]
fn write_is_accepted_and_answered_with_the_compatibility_note() {
    let dir = tempfile::tempdir().unwrap();
    let args = InitArgs {
        force: false,
        write: true,
    };
    let (code, out) = run(&args, dir.path());
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out.lines().last(), Some(WRITE_NOTE));
}

#[test]
fn a_refused_scaffold_prints_error_and_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".xaft"), "a file, not a directory").unwrap();
    let (code, out) = run(&InitArgs::default(), dir.path());
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert!(
        out.starts_with("error: Project config path is not a directory:"),
        "{out}"
    );
}

#[test]
fn the_written_config_is_inert_for_the_real_loader() {
    // The project layer outranks the user layer, so a template that set any
    // value would silently override the user's config. Load through the real
    // loader with the project dir pointed at the scaffold and compare.
    let dir = tempfile::tempdir().unwrap();
    run(&InitArgs::default(), dir.path());
    let overrides = xaft_config::CliOverrides {
        project_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let loaded = xaft_config::ConfigLoader::load(&overrides).unwrap();
    let baseline = xaft_config::ConfigLoader::load(&xaft_config::CliOverrides {
        project_dir: Some(tempfile::tempdir().unwrap().path().to_path_buf()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&baseline).unwrap(),
        "an all-comment template must not change the effective config"
    );
}
