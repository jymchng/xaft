//! F3 @-mention signal helper builders.
//!
//! The TUI emits F3-specific signals via the shared `SignalBus`. These
//! helpers convert internal types ([`EscapeInfo`], [`FileRef`]
//! content blocks) into the public signal payload types defined in
//! `xaft_agent::signals`.
//!
//! Kept in a dedicated module so the wiring in `state.rs` stays small
//! and the signal payload construction is testable in isolation.

use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::transport::{ContentBlock, EscapeInfo, FileRefContent};

use xaft_agent::signals::{
    EscapeSignalEntry, XaftEscapeMentionApproved, XaftEscapeMentionDenied, XaftFileRefAttached,
    XaftFileRefNotFound, XaftMentionsResolved,
};

/// Build an [`EscapeSignalEntry`] from an [`EscapeInfo`]. The
/// `raw_token` defaults to `absolute_path` because at submit time we
/// have already lost the original token text by the time the dialog
/// runs (the parser fed it through `expand()` which consumed it). The
/// token text is recoverable from the originating `MentionToken` if
/// needed; v0.2 keeps this simple and the audit log is still meaningful.
pub fn escape_signal_entry(info: &EscapeInfo, raw_token: &str) -> EscapeSignalEntry {
    EscapeSignalEntry {
        raw_token: raw_token.to_string(),
        reason: reason_label(info.reason).to_string(),
        absolute_path: info.absolute_path.clone(),
        byte_size: info.byte_size,
        depth: info.depth,
    }
}

/// Build a [`XaftFileRefAttached`] from a `ContentBlock::FileRef`.
/// Returns `None` if the block is not a `FileRef`.
pub fn file_ref_attached_entry(
    path: &str,
    content: &FileRefContent,
    byte_size: u64,
    line_count: u64,
    sha256: &str,
    escape: Option<&EscapeInfo>,
) -> XaftFileRefAttached {
    let canonical_path = escape
        .map(|e| e.absolute_path.clone())
        .unwrap_or_else(|| path.to_string());
    XaftFileRefAttached {
        path: path.to_string(),
        canonical_path,
        byte_size,
        line_count,
        sha256: sha256.to_string(),
        is_escape: escape.is_some(),
    }
}

/// Build a [`XaftMentionsResolved`] signal from aggregate counts and
/// totals. The `waller_count` is the number of mentions that produced
/// warnings (file not found, too large, etc.) and `escape_count` is the
/// number of paths classified as workspace escapes.
pub fn mentions_resolved_entry(
    mention_count: usize,
    resolved_count: usize,
    warning_count: usize,
    escape_count: usize,
    total_bytes: u64,
) -> XaftMentionsResolved {
    XaftMentionsResolved {
        mention_count,
        resolved_count,
        warning_count,
        escape_count,
        total_bytes,
    }
}

/// Build a [`XaftFileRefNotFound`] signal from a path and reason. The
/// reason should be a human-readable string such as
/// `"file not found"`, `"too large"`, `"not text or image"`, etc.
pub fn file_ref_not_found_entry(path: &str, reason: &str) -> XaftFileRefNotFound {
    XaftFileRefNotFound {
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

/// Emit a `XaftEscapeMentionApproved` signal. Convenience wrapper so
/// callers don't need to construct the entries themselves.
pub async fn emit_escape_approved(bus: &SignalBus, mentions: &[EscapeInfo], session_wide: bool) {
    let entries: Vec<EscapeSignalEntry> = mentions
        .iter()
        .map(|m| escape_signal_entry(m, &m.absolute_path))
        .collect();
    bus.emit(XaftEscapeMentionApproved {
        tokens: entries,
        session_wide,
    })
    .await;
}

/// Emit a `XaftEscapeMentionDenied` signal.
pub async fn emit_escape_denied(bus: &SignalBus, mentions: &[EscapeInfo], reason: &str) {
    let entries: Vec<EscapeSignalEntry> = mentions
        .iter()
        .map(|m| escape_signal_entry(m, &m.absolute_path))
        .collect();
    bus.emit(XaftEscapeMentionDenied {
        tokens: entries,
        reason: reason.to_string(),
    })
    .await;
}

/// Map an `EscapeReason` to a human-readable label. Mirrors the
/// internal label used by `confirm.rs`.
fn reason_label(r: agtrs_runtime::transport::EscapeReason) -> &'static str {
    use agtrs_runtime::transport::EscapeReason;
    match r {
        EscapeReason::Absolute => "absolute",
        EscapeReason::ParentTraversal => "parent_traversal",
        EscapeReason::HomeExpansion => "home_expansion",
    }
}

/// Helper: extract all `FileRef` blocks from a `Vec<ContentBlock>`, in
/// order. Used by the per-file attached-signal emission.
pub fn file_ref_blocks(blocks: &[ContentBlock]) -> Vec<&ContentBlock> {
    blocks
        .iter()
        .filter(|b| matches!(b, ContentBlock::FileRef { .. }))
        .collect()
}

/// Best-effort path extraction from a `MentionError`. Used by the TUI
/// submit handler to attach a `XaftFileRefNotFound` signal with the
/// path the user typed. For some error variants the path is implicit
/// in the error display; we return the path string when present, or
/// the `Display` form as a fallback.
pub fn path_from_warning(err: &crate::mention::MentionError) -> String {
    use crate::mention::MentionError;
    match err {
        MentionError::EmptyPath => String::new(),
        MentionError::FileNotFound { path } => path.clone(),
        MentionError::EscapeRejected { raw, .. } => raw.clone(),
        MentionError::TooLarge { path, .. } => path.clone(),
        MentionError::NotTextOrImage { path, .. } => path.clone(),
        MentionError::IoError { path, .. } => path.clone(),
    }
}
