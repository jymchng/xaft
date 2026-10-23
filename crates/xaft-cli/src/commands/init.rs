//! Handler for `xaft init` (parity matrix row 57).
//!
//! Port of `agenthicc.cli.commands.init.init_project`: create the project
//! scaffold in the current directory and report, one line per path, what was
//! created, overwritten or preserved. The scaffold itself lives in
//! [`crate::project_init`].
//!
//! # Output
//!
//! Byte-for-byte agenthicc's shape:
//!
//! ```text
//! Initialized /abs/project
//! Created .xaft
//! Created AGENTS.md
//! Created .xaft/xaft.toml
//! ```
//!
//! `Created` lines come first, then `Overwrote`, then `Preserved`, each with a
//! path relative to the project root. `--write` is accepted and answered with
//! the same compatibility note agenthicc prints.
//!
//! # One deliberate divergence: the exit code
//!
//! agenthicc prints `error: <reason>` and returns normally, so a refused
//! scaffold exits 0. xaft prints the same line and exits 1. `init` is the
//! command a provisioning script runs; a scaffold that was refused must not
//! read as success to the script that depends on it. The `mcp` family keeps
//! agenthicc's exit-0 convention because it is an established, documented
//! contract there; `init` has no such history to preserve.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use xaft_runtime::ExitCode;

use crate::args::InitArgs;
use crate::error::XaftError;
use crate::project_init::initialize_project;

/// The compatibility note agenthicc prints for `--write`.
pub const WRITE_NOTE: &str =
    "Note: --write is retained for compatibility; init now writes by default.";

/// Execute `xaft init` in the current directory.
pub async fn handle_init(args: &InitArgs) -> Result<ExitCode, XaftError> {
    let root = std::env::current_dir()?;
    let mut out = String::new();
    let code = render_init(args, &root, &mut out);
    print!("{out}");
    Ok(code)
}

/// Run `xaft init` against `root`, writing every output line into `out`.
///
/// The seam tests drive: it takes the root explicitly, so a test never has to
/// change the process's working directory.
pub fn render_init(args: &InitArgs, root: &Path, out: &mut String) -> ExitCode {
    let result = match initialize_project(root, args.force) {
        Ok(result) => result,
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            return ExitCode::TASK_FAILED;
        }
    };

    let _ = writeln!(out, "Initialized {}", result.root.display());
    for (label, paths) in [
        ("Created", &result.created),
        ("Overwrote", &result.overwritten),
        ("Preserved", &result.preserved),
    ] {
        for path in paths {
            let _ = writeln!(out, "{label} {}", relative(path, &result.root).display());
        }
    }
    if args.write {
        let _ = writeln!(out, "{WRITE_NOTE}");
    }
    ExitCode::SUCCESS
}

fn relative(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}
