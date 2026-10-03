//! Shell execution tools.
//!
//! [`BashExecTool`] wraps [`agtrs_shell::CommandExecutor`] with policy and
//! sandbox enforcement, passing one string to `bash -c`.
//! [`RunCommandTool`] uses the same executor and policy but spawns an argv
//! directly, so no shell ever re-parses the arguments.
//! [`RunPythonTool`] and [`RunPythonExprTool`] run a Python snippet behind an
//! explicit interpreter probe, because unlike agenthicc — which reuses its own
//! `sys.executable` — xaft is a Rust binary with no interpreter to assume.
//!
//! For typed commands (cargo check, cargo test, etc.) see
//! [`agtrs_shell::ShellToolSet`].

pub mod bash_exec;
pub mod run_command;
pub mod run_python;

pub use bash_exec::BashExecTool;
pub use run_command::RunCommandTool;
pub use run_python::{RunPythonExprTool, RunPythonTool};
