//! Shell execution tools.
//!
//! [`BashExecTool`] wraps [`agtrs_shell::CommandExecutor`] with policy and
//! sandbox enforcement, passing one string to `bash -c`.
//! [`RunCommandTool`] uses the same executor and policy but spawns an argv
//! directly, so no shell ever re-parses the arguments.
//!
//! For typed commands (cargo check, cargo test, etc.) see
//! [`agtrs_shell::ShellToolSet`].

pub mod bash_exec;
pub mod run_command;

pub use bash_exec::BashExecTool;
pub use run_command::RunCommandTool;
