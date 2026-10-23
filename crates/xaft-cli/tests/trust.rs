//! Handler tests for `xaft trust` (parity matrix row 47).
//!
//! `render_trust` is driven against a temp trust store and a temp project, so
//! no test touches `~/.xaft`. The output lines are agenthicc's
//! `cli/commands/trust.py`, including `list`'s `{status:<30} {kind:<9}`
//! columns. The behaviour that makes the command real — the CLI-plugin loader
//! refusing an untrusted table and accepting a trusted one — is asserted here
//! through `cli_plugins::discover`, the function `xaft_cli::run` calls before
//! parsing.

use std::path::Path;

use clap::Parser;
use xaft_cli::args::{Commands, TrustSubcommand, XaftCli};
use xaft_cli::cli_plugins::discover;
use xaft_cli::commands::trust::render_trust;
use xaft_cli::trust::ExtensionTrust;
use xaft_runtime::ExitCode;

const TABLE: &str = "[[command]]\npath = [\"hello\"]\nrun = \"echo hi\"\n";

struct Fixture {
    _dir: tempfile::TempDir,
    project: std::path::PathBuf,
    trust: ExtensionTrust,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let home = dir.path().join("home");
    let trust = ExtensionTrust::new(home.join("trust.json"), vec![home]);
    Fixture {
        _dir: dir,
        project,
        trust,
    }
}

fn write_table(project: &Path, body: &str) -> std::path::PathBuf {
    let file = project.join(".xaft/cli.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, body).unwrap();
    file
}

fn run(f: &Fixture, sub: TrustSubcommand) -> (ExitCode, String) {
    let mut out = String::new();
    let code = render_trust(&sub, &f.trust, &f.project, &mut out);
    (code, out)
}

#[test]
fn every_trust_spelling_parses() {
    for argv in [
        vec!["xaft", "trust", "list"],
        vec!["xaft", "trust", "add"],
        vec!["xaft", "trust", "add", ".xaft/cli.toml"],
        vec!["xaft", "trust", "revoke", ".xaft/cli.toml"],
        vec!["xaft", "trust", "cli"],
    ] {
        let cli = XaftCli::try_parse_from(&argv).unwrap();
        assert!(matches!(cli.command, Some(Commands::Trust(_))), "{argv:?}");
    }
    assert!(XaftCli::try_parse_from(["xaft", "trust", "revoke"]).is_err());
}

#[test]
fn list_with_no_extensions_still_names_the_store() {
    let f = fixture();
    let (code, out) = run(&f, TrustSubcommand::List);
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!(
            "No project extensions under .xaft/.\n\nTrust store: {} (0 record(s))\n",
            f.trust.store_path().display()
        )
    );
}

#[test]
fn add_list_revoke_round_trip_with_agenthicc_lines() {
    let f = fixture();
    let file = write_table(&f.project, TABLE);
    let resolved = file.canonicalize().unwrap();

    let (_, before) = run(&f, TrustSubcommand::List);
    assert!(
        before.starts_with(&format!(
            "  {:<30} {:<9} {}\n",
            "not trusted",
            "cli",
            file.display()
        )),
        "{before}"
    );

    let (code, added) = run(&f, TrustSubcommand::Add { path: None });
    assert_eq!(code, ExitCode::SUCCESS);
    let first = added.lines().next().unwrap();
    assert!(
        first.starts_with(&format!(
            "  trusted cli       {}  (sha256 ",
            resolved.display()
        )),
        "{first}"
    );
    assert!(first.ends_with("…)"), "{first}");
    assert!(added.ends_with(&format!(
        "\nRecorded in {}. Editing a file revokes its trust.\n",
        f.trust.store_path().display()
    )));

    let (_, after) = run(&f, TrustSubcommand::List);
    assert!(
        after.starts_with(&format!("  {:<30} cli", "trusted")),
        "{after}"
    );
    assert!(after.contains("(1 record(s))"));

    let (code, revoked) = run(
        &f,
        TrustSubcommand::Revoke {
            path: ".xaft/cli.toml".into(),
        },
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(revoked, format!("  revoked {}\n", file.display()));

    let (_, again) = run(
        &f,
        TrustSubcommand::Revoke {
            path: ".xaft/cli.toml".into(),
        },
    );
    assert_eq!(again, "No trust records for .xaft/cli.toml.\n");
}

#[test]
fn add_with_nothing_to_trust_says_so() {
    let f = fixture();
    let (code, out) = run(&f, TrustSubcommand::Add { path: None });
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, "Nothing to trust.\n");
}

#[test]
fn add_of_a_missing_path_is_skipped() {
    let f = fixture();
    let (_, out) = run(
        &f,
        TrustSubcommand::Add {
            path: Some("missing.toml".into()),
        },
    );
    assert!(out.starts_with("  skipped (not a file): "), "{out}");
}

#[test]
fn trust_cli_is_the_cli_plugin_case_of_add() {
    let f = fixture();
    let (_, none) = run(&f, TrustSubcommand::Cli);
    assert!(none.starts_with("No CLI plugins found at .xaft/cli.toml\n"));

    write_table(&f.project, TABLE);
    let (code, out) = run(&f, TrustSubcommand::Cli);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.starts_with("  trusted cli "), "{out}");
    assert_eq!(f.trust.records().len(), 1);
}

#[test]
fn trust_is_what_lets_the_loader_use_a_project_table() {
    let f = fixture();
    let file = write_table(&f.project, TABLE);

    let untrusted = discover(&f.project, None, &f.trust, &[], false).unwrap();
    assert!(untrusted.registry.is_empty());
    assert!(untrusted.warnings[0].contains("skipping untrusted cli extension"));

    run(&f, TrustSubcommand::Add { path: None });
    let trusted = discover(&f.project, None, &f.trust, &[], false).unwrap();
    assert_eq!(trusted.registry.commands().len(), 1);
    assert_eq!(trusted.registry.commands()[0].path, vec!["hello"]);

    // Editing the reviewed file revokes the trust without any command.
    std::fs::write(&file, format!("{TABLE}# tampered\n")).unwrap();
    let edited = discover(&f.project, None, &f.trust, &[], false).unwrap();
    assert!(edited.registry.is_empty());
    let (_, listing) = run(&f, TrustSubcommand::List);
    assert!(
        listing.contains("changed since it was trusted"),
        "{listing}"
    );
}
