//! Stable, non-sensitive error values for the browser tools.
//!
//! Port of agenthicc's `tools/cloakbrowser/errors.py`. Every string here is
//! part of the agent-facing contract: an agent branches on `error_kind`, and
//! the `error` text is the only explanation it ever sees. Neither may carry a
//! URL, a cookie, a filesystem path, page content or a stack trace — the
//! original failure is logged at `debug` by *category*, never by value.

use std::fmt;

/// Machine-readable browser failure categories returned to an agent.
///
/// The wire values are agenthicc's `BrowserErrorKind` values verbatim,
/// including its one deliberate alias: `NOT_FOUND` and `STALE_PAGE` are the
/// same string (`stale_page`), because "that page id is not open" and "that
/// page went away" need the same recovery — open a page again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserErrorKind {
    /// The backend is disabled or has no destinations configured.
    Disabled,
    /// An optional dependency is missing.
    DependencyMissing,
    /// The browser binary is not installed.
    BinaryMissing,
    /// The browser could not be started or reached.
    BrowserUnavailable,
    /// The backend reported itself unhealthy.
    Unhealthy,
    /// An operator approval is required first.
    ApprovalRequired,
    /// The destination, input or configuration is refused by policy.
    PolicyDenied,
    /// The agent supplied a malformed argument.
    InvalidInput,
    /// The page id is not (or no longer) open.
    StalePage,
    /// A per-session or per-turn quota is exhausted.
    LimitExceeded,
    /// A produced artifact exceeds its configured ceiling.
    OutputLimit,
    /// The operation did not finish within its deadline.
    Timeout,
    /// A network-level failure (resolution, connection, navigation).
    Network,
    /// Any other failure; the detail is deliberately withheld.
    Execution,
    /// The browser session was closed.
    Closed,
    /// The operation was cancelled and the session closed defensively.
    Cancelled,
}

impl BrowserErrorKind {
    /// agenthicc's `BrowserErrorKind.NOT_FOUND` — an alias of [`Self::StalePage`].
    pub const NOT_FOUND: BrowserErrorKind = BrowserErrorKind::StalePage;

    /// The wire value an agent sees in `error_kind` / `status`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "not_configured",
            Self::DependencyMissing => "dependency_missing",
            Self::BinaryMissing => "binary_missing",
            Self::BrowserUnavailable => "browser_unavailable",
            Self::Unhealthy => "unhealthy",
            Self::ApprovalRequired => "approval_required",
            Self::PolicyDenied => "policy_denied",
            Self::InvalidInput => "invalid_input",
            Self::StalePage => "stale_page",
            Self::LimitExceeded => "limit_exceeded",
            Self::OutputLimit => "output_limit",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Execution => "execution",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a wire value back into a kind (agenthicc `BrowserErrorKind(value)`).
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "not_configured" => Self::Disabled,
            "dependency_missing" => Self::DependencyMissing,
            "binary_missing" => Self::BinaryMissing,
            "browser_unavailable" => Self::BrowserUnavailable,
            "unhealthy" => Self::Unhealthy,
            "approval_required" => Self::ApprovalRequired,
            "policy_denied" => Self::PolicyDenied,
            "invalid_input" => Self::InvalidInput,
            "stale_page" => Self::StalePage,
            "limit_exceeded" => Self::LimitExceeded,
            "output_limit" => Self::OutputLimit,
            "timeout" => Self::Timeout,
            "network" => Self::Network,
            "execution" => Self::Execution,
            "closed" => Self::Closed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

impl fmt::Display for BrowserErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An expected browser failure with a message that is safe for model output.
///
/// agenthicc's `BrowserToolError`. The message is chosen by the code that
/// raises it — always a fixed sentence — so it can be returned verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserToolError {
    /// The machine-readable category.
    pub kind: BrowserErrorKind,
    /// The safe, fixed explanation.
    pub message: String,
}

impl BrowserToolError {
    /// Build an error from a kind and a fixed sentence.
    pub fn new(kind: BrowserErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for BrowserToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for BrowserToolError {}

/// Every way a browser client operation can fail.
///
/// agenthicc's `_safe` wrapper sorts Python exceptions into four buckets:
/// a `BrowserToolError` keeps its own kind and message, a `TimeoutError`
/// becomes `timeout`, an `OSError` becomes `network`, and anything else
/// becomes a generic `execution` failure whose detail is withheld. This enum
/// is those four buckets made explicit, so a Rust client cannot accidentally
/// leak an error's `Display` text into a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserFailure {
    /// An expected failure with a safe message.
    Tool(BrowserToolError),
    /// A deadline expired.
    Timeout,
    /// A network-level failure.
    Network,
    /// Any other failure. The string is a short *category* for debug logs
    /// (for example `"cdp protocol error"`), never a value from the page.
    Execution(&'static str),
}

impl BrowserFailure {
    /// Shorthand for an expected failure.
    pub fn tool(kind: BrowserErrorKind, message: impl Into<String>) -> Self {
        Self::Tool(BrowserToolError::new(kind, message))
    }
}

impl From<BrowserToolError> for BrowserFailure {
    fn from(value: BrowserToolError) -> Self {
        Self::Tool(value)
    }
}

impl fmt::Display for BrowserFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tool(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("timeout"),
            Self::Network => f.write_str("network failure"),
            Self::Execution(category) => write!(f, "execution failure ({category})"),
        }
    }
}

impl std::error::Error for BrowserFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire values are agenthicc's, and they round-trip.
    #[test]
    fn wire_values_match_agenthicc_and_round_trip() {
        let expected = [
            (BrowserErrorKind::Disabled, "not_configured"),
            (BrowserErrorKind::DependencyMissing, "dependency_missing"),
            (BrowserErrorKind::BinaryMissing, "binary_missing"),
            (BrowserErrorKind::BrowserUnavailable, "browser_unavailable"),
            (BrowserErrorKind::Unhealthy, "unhealthy"),
            (BrowserErrorKind::ApprovalRequired, "approval_required"),
            (BrowserErrorKind::PolicyDenied, "policy_denied"),
            (BrowserErrorKind::InvalidInput, "invalid_input"),
            (BrowserErrorKind::StalePage, "stale_page"),
            (BrowserErrorKind::LimitExceeded, "limit_exceeded"),
            (BrowserErrorKind::OutputLimit, "output_limit"),
            (BrowserErrorKind::Timeout, "timeout"),
            (BrowserErrorKind::Network, "network"),
            (BrowserErrorKind::Execution, "execution"),
            (BrowserErrorKind::Closed, "closed"),
            (BrowserErrorKind::Cancelled, "cancelled"),
        ];
        for (kind, wire) in expected {
            assert_eq!(kind.as_str(), wire);
            assert_eq!(BrowserErrorKind::from_wire(wire), Some(kind));
        }
        assert_eq!(BrowserErrorKind::from_wire("unknown-status"), None);
    }

    /// agenthicc aliases NOT_FOUND to STALE_PAGE; keep the alias.
    #[test]
    fn not_found_is_stale_page() {
        assert_eq!(BrowserErrorKind::NOT_FOUND.as_str(), "stale_page");
    }
}
