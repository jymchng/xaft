//! Background terminal subsystem.
//!
//! Provides long-lived, inspectable background processes ("terminals") that an
//! agent can start, poll, and stop, together with a bounded, redacted JSONL
//! transcript of their output.
//!
//! # Safety properties
//!
//! These invariants are enforced by the types in this module and are pinned by
//! the inline `mod tests` in the files named beside each one — there is no
//! separate integration test file, because the properties are not separable
//! from the code that owns them:
//!
//! * Every child is spawned into its own process group, and teardown signals the
//!   **group** (`killpg`) rather than the direct child, so orphaned descendants
//!   are reaped as well. Pin: `stop_reaches_a_grandchild_through_the_process_group`
//!   in [`manager`].
//! * A PID is never signalled after it has been persisted to disk and reloaded.
//!   Persisted PIDs are treated as advisory only, so a recycled PID belonging to
//!   an unrelated process can never be killed. Pins: `an_orphaned_terminal_cannot_be_stopped`
//!   in [`manager`] and `a_record_with_an_unknown_state_deserialises_as_orphaned`
//!   in [`record`].
//! * Any `cwd` that does not resolve inside the workspace root is rejected
//!   before a process is spawned. Pin: `a_cwd_outside_the_workspace_is_refused_before_any_spawn`
//!   in [`manager`].
//! * Output is redacted and bounded *before* it reaches the JSONL transcript.
//!   Pin: `output_is_redacted_before_it_is_persisted` in [`manager`].
//!
//! `scripts/doc-test-name-check.py --check` asserts that every one of those names
//! resolves to a real `fn`, because a doc header that names a test which does not
//! exist reads as "this property is pinned" when it is not.

pub mod manager;
pub mod record;
pub mod redact;
pub mod tools;

pub use manager::{TerminalManager, TerminalManagerConfig, validate_timeout};
pub use record::{TerminalRecord, TerminalState, TerminalStore};
pub use tools::{InspectTerminalTool, StopTerminalTool, WaitTerminalReadyTool, WaitTerminalTool};
