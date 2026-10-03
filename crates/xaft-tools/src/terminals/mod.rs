//! Background terminal subsystem.
//!
//! Provides long-lived, inspectable background processes ("terminals") that an
//! agent can start, poll, and stop, together with a bounded, redacted JSONL
//! transcript of their output.
//!
//! # Safety properties
//!
//! These invariants are enforced by the types in this module and are covered by
//! the `manager` and `record` unit tests beside them:
//!
//! * Every child is spawned into its own process group, and teardown signals the
//!   **group** (`killpg`) rather than the direct child, so orphaned descendants
//!   are reaped as well.
//! * A PID is never signalled after it has been persisted to disk and reloaded.
//!   Persisted PIDs are treated as advisory only, so a recycled PID belonging to
//!   an unrelated process can never be killed.
//! * Any `cwd` that does not resolve inside the workspace root is rejected
//!   before a process is spawned.
//! * Output is redacted and bounded *before* it reaches the JSONL transcript.

pub mod manager;
pub mod record;
pub mod redact;
pub mod tools;

pub use manager::{TerminalManager, TerminalManagerConfig, validate_timeout};
pub use record::{TerminalRecord, TerminalState, TerminalStore};
pub use tools::{InspectTerminalTool, StopTerminalTool, WaitTerminalReadyTool, WaitTerminalTool};
