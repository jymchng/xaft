//! Handler for `xaft skills` (parity matrix row 46).
//!
//! Port of `agenthicc.cli.commands.skills`: one subcommand, `add`, that hands
//! its arguments to the installer in [`xaft_skills::installer`] and prints one
//! line per installed skill:
//!
//! ```text
//! Installed skill rust-review (project) at /abs/project/.xaft/skills/rust-review
//! ```
//!
//! `--skill` takes a comma-separated list of repository skills and `--all`
//! clears it, exactly as agenthicc's `skills_add` does.
//!
//! # One deliberate divergence: the exit code
//!
//! agenthicc prints `error: <reason>` and returns normally, so a failed install
//! exits 0. xaft prints the same line and exits 1: `skills add` is routinely
//! scripted (bootstrap scripts, CI images), and a download that failed must not
//! read as an install that succeeded. See also `commands::init`, which makes
//! the same choice for the same reason.

use std::fmt::Write as _;
use std::path::Path;

use xaft_runtime::ExitCode;
use xaft_skills::installer::{InstallRequest, SkillFetcher, install_skills_with};

use crate::args::{SkillsAddArgs, SkillsSubcommand};
use crate::error::XaftError;

/// Execute `xaft skills <subcommand>`.
pub async fn handle_skills(subcommand: &SkillsSubcommand) -> Result<ExitCode, XaftError> {
    let cwd = std::env::current_dir()?;
    let mut out = String::new();
    let code = match subcommand {
        SkillsSubcommand::Add(args) => {
            render_add(
                args,
                &cwd,
                None,
                &xaft_skills::installer::HttpFetcher,
                &mut out,
            )
            .await
        }
    };
    print!("{out}");
    Ok(code)
}

/// Build the installer request `skills add` sends.
///
/// `--skill a,b` becomes `["a", "b"]` with blanks dropped; `--all` empties the
/// list, which the installer reads as "every skill in the repository".
pub fn install_request(
    args: &SkillsAddArgs,
    project_dir: &Path,
    global_dir: Option<&Path>,
) -> InstallRequest {
    let skill_names = if args.all {
        Vec::new()
    } else {
        args.skill
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    InstallRequest {
        source: args.source.clone(),
        global_scope: args.global,
        project_scope: args.project,
        name: args.name.clone(),
        skill_names,
        project_dir: project_dir.to_path_buf(),
        global_dir: global_dir.map(Path::to_path_buf),
    }
}

/// Run `skills add`, writing its output into `out`.
///
/// `global_dir` overrides the user skills directory and `fetcher` the network,
/// so tests can drive the whole command without a home directory or a socket.
pub async fn render_add<F: SkillFetcher>(
    args: &SkillsAddArgs,
    project_dir: &Path,
    global_dir: Option<&Path>,
    fetcher: &F,
    out: &mut String,
) -> ExitCode {
    let request = install_request(args, project_dir, global_dir);
    match install_skills_with(&request, fetcher).await {
        Ok(results) => {
            for result in results {
                let _ = writeln!(
                    out,
                    "Installed skill {} ({}) at {}",
                    result.slug,
                    result.scope,
                    result.path.display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            ExitCode::TASK_FAILED
        }
    }
}
