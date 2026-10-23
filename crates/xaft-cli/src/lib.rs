//! `xaft-cli` — CLI argument parsing, tracing init, and command dispatch.
//!
//! This crate owns the boundary between the user's shell and the xaft runtime.
//! It parses arguments with `clap`, loads configuration via `xaft-config`, and
//! dispatches to the appropriate handler via `xaft-runtime`.
//!
//! # Architecture
//!
//! ```text
//! main()
//!   └── xaft_cli::run(runtime, sessions)
//!         ├── XaftCli::parse()           [clap]
//!         ├── tracing_init::init()       [tracing-subscriber]
//!         └── dispatch(cli, runtime, sessions)
//!               ├── run::handle_run()
//!               ├── config::handle_config()
//!               ├── session::handle_session()
//!               ├── jobs::handle_jobs()
//!               ├── runs::handle_runs()
//!               ├── version::handle_version()
//!               └── completions::handle_completions()
//! ```
//!
//! `runtime` and `sessions` are two views of ONE store: `main.rs` builds a
//! single `xaft_session::SessionManager`, gives its `session_store()` to the
//! runtime and the manager itself to the CLI as a
//! [`SessionBackend`](xaft_runtime::session_backend::SessionBackend). So
//! `xaft session list` (runtime) and `xaft session inspect` (backend) cannot
//! disagree about which sessions exist.
//!
//! # Usage from `main.rs`
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use xaft_runtime::dispatch::StubRuntime;
//!
//! #[tokio::main]
//! async fn main() {
//!     let runtime = Arc::new(StubRuntime);
//!     xaft_cli::run(runtime, None).await;
//! }
//! ```

#![deny(missing_docs)]

pub mod args;
pub mod cli_plugins;
pub mod commands;
pub mod dispatch;
pub mod error;
pub mod jobs;
pub mod mcp;
pub mod project_init;
pub mod runs;
pub mod tracing_init;
pub mod trust;

pub use args::XaftCli;
pub use dispatch::{dispatch, run};
pub use error::XaftError;
