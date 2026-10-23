//! Handler for `xaft trust` (parity matrix row 47).
//!
//! Port of `agenthicc.cli.commands.trust`: review and trust the project
//! extensions a repository ships. The store and the trust rule live in
//! [`crate::trust`]; the loaders that obey them are [`crate::cli_plugins`]
//! and `xaft_tools::custom_tools` (`.xaft/tools/*.toml`).
//!
//! | subcommand | agenthicc | what it does |
//! | --- | --- | --- |
//! | `trust list` | `trust list` | every project extension and whether it is trusted |
//! | `trust add [PATH]` | `trust add [PATH]` | trust the *current content* of PATH (default: every project extension) |
//! | `trust revoke PATH` | `trust revoke PATH` | forget the record for PATH (or every table under a directory) |
//!
//! Matrix row 47 is named `trust cli`, after agenthicc's original command,
//! which hashed `.agenthicc/cli/*.py` into a project-local manifest. agenthicc
//! HEAD (PRD-250 S-02) replaced it with `list`/`add`/`revoke` over one
//! user-owned store; that is the shape implemented here, and `trust cli` is
//! deliberately not a command. `scripts/parity-row-reachability.py` declares
//! the rename (`trust cli` → `trust add`).
//!
//! Output lines are agenthicc's, including the column widths of `list`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use xaft_runtime::ExitCode;

use crate::args::TrustSubcommand;
use crate::error::XaftError;
use crate::trust::{ExtensionTrust, TrustStoreWrite, kind_for, project_extension_files, targets};

/// Execute `xaft trust <subcommand>` against the user's trust store.
pub async fn handle_trust(subcommand: &TrustSubcommand) -> Result<ExitCode, XaftError> {
    let cwd = std::env::current_dir()?;
    let trust = ExtensionTrust::for_user();
    let mut out = String::new();
    let code = render_trust(subcommand, &trust, &cwd, &mut out);
    print!("{out}");
    Ok(code)
}

/// Run `xaft trust` against an explicit store and project directory.
pub fn render_trust(
    subcommand: &TrustSubcommand,
    trust: &ExtensionTrust,
    project: &Path,
    out: &mut String,
) -> ExitCode {
    match subcommand {
        TrustSubcommand::List => list(trust, project, out),
        TrustSubcommand::Add { path } => {
            let targets: Vec<(PathBuf, &'static str)> = match path {
                Some(path) => targets(&absolute(project, path))
                    .into_iter()
                    .map(|t| {
                        let kind = kind_for(&t);
                        (t, kind)
                    })
                    .collect(),
                None => project_extension_files(project),
            };
            add(trust, targets, out)
        }
        TrustSubcommand::Revoke { path } => revoke(trust, project, path, out),
    }
}

fn absolute(project: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project.join(path)
    }
}

fn list(trust: &ExtensionTrust, project: &Path, out: &mut String) -> ExitCode {
    let files = project_extension_files(project);
    if files.is_empty() {
        let _ = writeln!(out, "No project extensions under .xaft/.");
    }
    for (path, kind) in &files {
        let status = match trust.check(path, kind) {
            None => "trusted".to_string(),
            Some(refusal) => refusal.reason,
        };
        let _ = writeln!(out, "  {status:<30} {kind:<9} {}", path.display());
    }
    let _ = writeln!(
        out,
        "\nTrust store: {} ({} record(s))",
        trust.store_path().display(),
        trust.records().len()
    );
    ExitCode::SUCCESS
}

fn add(
    trust: &ExtensionTrust,
    targets: Vec<(PathBuf, &'static str)>,
    out: &mut String,
) -> ExitCode {
    if targets.is_empty() {
        let _ = writeln!(out, "Nothing to trust.");
        return ExitCode::SUCCESS;
    }
    let mut code = ExitCode::SUCCESS;
    for (target, kind) in targets {
        if !target.is_file() {
            let _ = writeln!(out, "  skipped (not a file): {}", target.display());
            continue;
        }
        match trust.trust(&target, kind) {
            Ok(record) => {
                let short: String = record.sha256.chars().take(16).collect();
                let _ = writeln!(
                    out,
                    "  trusted {kind:<9} {}  (sha256 {short}…)",
                    record.path
                );
            }
            Err(e) => {
                let _ = writeln!(out, "error: could not trust {}: {e}", target.display());
                code = ExitCode::TASK_FAILED;
            }
        }
    }
    let _ = writeln!(
        out,
        "\nRecorded in {}. Editing a file revokes its trust.",
        trust.store_path().display()
    );
    code
}

fn revoke(trust: &ExtensionTrust, project: &Path, path: &Path, out: &mut String) -> ExitCode {
    let mut revoked = Vec::new();
    for target in targets(&absolute(project, path)) {
        match trust.revoke(&target) {
            Ok(true) => revoked.push(target),
            Ok(false) => {}
            Err(e) => {
                let _ = writeln!(out, "error: could not revoke {}: {e}", target.display());
                return ExitCode::TASK_FAILED;
            }
        }
    }
    for target in &revoked {
        let _ = writeln!(out, "  revoked {}", target.display());
    }
    if revoked.is_empty() {
        let _ = writeln!(out, "No trust records for {}.", path.display());
    }
    ExitCode::SUCCESS
}
