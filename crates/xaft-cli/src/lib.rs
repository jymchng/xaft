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
//!   └── xaft_cli::run(runtime)
//!         ├── XaftCli::parse()           [clap]
//!         ├── tracing_init::init()       [tracing-subscriber]
//!         └── dispatch(cli, runtime)
//!               ├── run::handle_run()
//!               ├── config::handle_config()
//!               ├── session::handle_session()
//!               ├── version::handle_version()
//!               └── completions::handle_completions()
//! ```
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
//!     xaft_cli::run(runtime).await;
//! }
//! ```

#![deny(missing_docs)]

pub mod args;
pub mod commands;
pub mod dispatch;
pub mod error;
pub mod tracing_init;

pub use args::XaftCli;
pub use dispatch::{dispatch, run};
pub use error::XaftError;
