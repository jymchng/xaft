//! CLI-level error types with user-friendly display and exit codes.

use xaft_runtime::ExitCode;

/// All errors that can occur during CLI processing.
#[derive(Debug, thiserror::Error)]
pub enum XaftError {
    /// Configuration loading or validation failed.
    #[error("configuration error: {0}")]
    Config(#[from] xaft_config::ConfigError),

    /// Runtime execution error.
    #[error("{0}")]
    Runtime(#[from] xaft_runtime::RuntimeError),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid CLI usage.
    #[error("usage error: {0}")]
    Usage(String),

    /// Feature not yet implemented.
    #[error("not yet implemented: {0}")]
    NotImplemented(String),
}

impl XaftError {
    /// Return the exit code for this error.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Config(_) => ExitCode::CONFIG_ERROR,
            Self::Runtime(e) => e.exit_code(),
            Self::Io(_) => ExitCode::TASK_FAILED,
            Self::Usage(_) => ExitCode::USAGE_ERROR,
            Self::NotImplemented(_) => ExitCode::TASK_FAILED,
        }
    }

    /// Print a user-friendly error message to stderr.
    pub fn print_diagnostic(&self) {
        eprintln!();
        eprintln!("  \x1b[31merror\x1b[0m: {self}");
        eprintln!();

        // Add helpful hints for common errors
        match self {
            Self::Config(xaft_config::ConfigError::MissingApiKey { provider, env_var }) => {
                eprintln!("  \x1b[33mhint\x1b[0m: Set your API key:");
                eprintln!("    export {env_var}=your-api-key");
                eprintln!();
                eprintln!("  Or add it to your config file:");
                eprintln!("    [provider.{provider}]");
                eprintln!("    api_key = \"your-api-key\"");
                eprintln!();
            }
            Self::Config(xaft_config::ConfigError::Validation { section, message }) => {
                eprintln!("  \x1b[33mhint\x1b[0m: Check [{section}] in your config file.");
                eprintln!("  Run `xaft config show` to see the resolved config.");
                eprintln!();
                let _ = message; // already included in the error message
            }
            Self::Usage(_) => {
                eprintln!("  \x1b[33mhint\x1b[0m: Run `xaft --help` for usage information.");
                eprintln!();
            }
            _ => {}
        }
    }
}
