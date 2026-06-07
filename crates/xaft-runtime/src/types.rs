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

// ── F3 @-mention: UserMessage envelope ──────────────────────────────────────
//
// The TUI input bar sends messages to the runtime via a typed channel.
// F3 @-mention resolution produces a `Vec<ContentBlock>` (text +
// resolved `FileRef` blocks) that the runtime turns into the first user
// message via `Message::user_with_parts(parts)`. When no `@` tokens
// are present, the TUI can collapse to a plain text message via
// `UserMessage::Text(String)` — preserving the old `String` channel
// semantics without breaking call sites.
//
// Defined here (rather than in `xaft-tui`) so the runtime boundary type
// `RunRequest::user_message` can carry it without a `xaft-tui` dependency
// on the runtime side. `xaft-tui::user_message` re-exports this type.

use agtrs_runtime::transport::{ContentBlock, Message, MessageContent};

/// A message sent from the TUI input bar to the runtime.
#[derive(Debug, Clone)]
pub enum UserMessage {
    /// Plain text — no mention resolution happened (no `@` tokens in the
    /// input, or all mentions were inlined as text). Equivalent to the
    /// pre-F3 `String` channel payload.
    Text(String),
    /// Structured — one or more `ContentBlock`s produced by the F3
    /// `MentionResolver`. Always contains at least one block; empty
    /// vectors are normalised to `Text("")` by [`UserMessage::from_parts`].
    MultiPart(Vec<ContentBlock>),
}

impl UserMessage {
    /// Build a `UserMessage` from a list of content blocks. Collapses to
    /// `Text` when the vector is empty or a single `Text` block (matches
    /// the existing `MessageContent::from_parts` collapse behaviour).
    pub fn from_parts(parts: Vec<ContentBlock>) -> Self {
        match MessageContent::from_parts(parts) {
            MessageContent::Text(s) => UserMessage::Text(s),
            MessageContent::MultiPart(blocks) => UserMessage::MultiPart(blocks),
        }
    }

    /// Lossy string view of the message (for transcript rendering, log
    /// lines, and the audit log).
    pub fn as_text_lossy(&self) -> String {
        match self {
            UserMessage::Text(s) => s.clone(),
            UserMessage::MultiPart(blocks) => blocks
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.clone(),
                    ContentBlock::FileRef { path, .. } => format!("@{path}"),
                    ContentBlock::Image { .. } => "[image]".to_string(),
                    ContentBlock::ToolUse { name, .. } => format!("[tool_use:{name}]"),
                    ContentBlock::ToolResult { content, .. } => content.clone(),
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// Convert to an `agtrs_runtime::transport::Message` for the LLM.
    pub fn into_message(self) -> Message {
        match self {
            UserMessage::Text(s) => Message::user(s),
            UserMessage::MultiPart(parts) => Message::user_with_parts(parts),
        }
    }

    /// Returns `true` when the message contains at least one `FileRef`
    /// block (i.e. at least one mention was successfully resolved).
    pub fn has_file_refs(&self) -> bool {
        matches!(
            self,
            UserMessage::MultiPart(parts) if parts.iter().any(|b| matches!(b, ContentBlock::FileRef { .. }))
        )
    }
}

impl From<String> for UserMessage {
    fn from(s: String) -> Self {
        UserMessage::Text(s)
    }
}

impl From<&str> for UserMessage {
    fn from(s: &str) -> Self {
        UserMessage::Text(s.to_string())
    }
}
