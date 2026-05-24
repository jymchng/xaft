//! Shell execution tools.
//!
//! [`BashExecTool`] wraps [`agtrs_shell::CommandExecutor`] with policy and
//! sandbox enforcement. For typed commands (cargo check, cargo test, etc.)
//! see [`agtrs_shell::ShellToolSet`].

pub mod bash_exec;

pub use bash_exec::BashExecTool;
