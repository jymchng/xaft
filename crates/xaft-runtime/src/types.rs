//! Core types shared between xaft-cli and xaft-runtime.

/// Process exit code for the xaft binary.
///
/// Standard UNIX conventions:
/// - `0` = success
/// - `1` = general error / task failed
/// - `2` = usage error (bad arguments)
/// - `130` = cancelled by user (Ctrl-C)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub u8);

impl ExitCode {
    /// Task completed successfully.
    pub const SUCCESS: Self = Self(0);
    /// Task failed (agent could not complete the request).
    pub const TASK_FAILED: Self = Self(1);
    /// Usage error (invalid arguments or config).
    pub const USAGE_ERROR: Self = Self(2);
    /// Configuration error.
    pub const CONFIG_ERROR: Self = Self(3);
    /// Cancelled by user (Ctrl-C or explicit cancel).
    pub const CANCELLED: Self = Self(130);
    /// Budget exhausted.
    pub const BUDGET_EXCEEDED: Self = Self(4);

    /// Return the numeric code.
    pub fn code(self) -> u8 {
        self.0
    }

    /// Return `true` if this is a success exit.
    pub fn is_success(self) -> bool {
        self.0 == 0
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        std::process::ExitCode::from(code.0)
    }
}

impl std::fmt::Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
