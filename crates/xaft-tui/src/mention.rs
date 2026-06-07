//! F3 @-mention parser and resolver.
//!
//! Converts `@<path>` tokens in a user message into [`ResolvedFile`] blocks
//! that can be embedded in a [`UserMessage::MultiPart`](crate::user_message::UserMessage).
//!
//! Two layers:
//!
//! 1. [`find_mentions`] — pure text scan, returns byte-range tokens with the
//!    raw path string. No I/O. Used to highlight mention spans in the TUI
//!    while the user is typing.
//! 2. [`MentionResolver::expand`] — async, reads each path from a
//!    [`WorkspaceStore`], computes SHA-256, classifies escape paths, and
//!    returns an [`ExpandedMessage`] containing the `ContentBlock`s plus
//!    warnings.
//!
//! The split lets the TUI render live mention highlights without blocking on
//! disk I/O, then resolve them all at submit time.
//!
//! # Token grammar
//!
//! ```text
//! mention      ::= "@" path
//! path         ::= workspace_relative | absolute | home_expansion | parent_traversal
//! workspace_relative ::= ( "." | segment ) ( "/" segment )*
//! segment      ::= ( letter | digit | "_" | "-" | "." | "+" )+
//! absolute     ::= "/" ( segment "/" )* segment
//! home_expansion ::= "~" ( ["/"] | [ "/" ( segment "/" )* segment ] )
//! parent_traversal ::= ( segment "/" )* ".." ( "/" segment )*
//! ```
//!
//! A mention is *closed* at the first whitespace, `,`, `)`, `]`, `}`, `;`,
//! `:`, or end-of-string after the path.

use std::path::{Component, Path, PathBuf};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::transport::{
    ContentBlock, EscapeInfo, EscapeReason, FileRefContent, Message, MessageContent, Truncation,
};
use agtrs_workspace::WorkspaceStore;
use base64::Engine;
use sha2::{Digest, Sha256};
use xaft_config::{EscapePolicy, MentionConfig};

use crate::user_message::UserMessage;

// ── MentionToken (parser output) ──────────────────────────────────────────────

/// A `@<path>` mention found in user text, before resolution.
///
/// `start`/`end` are byte offsets (matching `str::char_indices()` semantics).
/// `path` is the raw path *as typed* — may be relative, absolute, contain
/// `..`, or start with `~`. Resolution is what turns it into a
/// [`ResolvedFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionToken {
    /// Byte offset of the leading `@`.
    pub start: usize,
    /// Byte offset one past the last path character (still inside the
    /// mention; the trailing `]`, `)`, etc. that closed it is at `end` or
    /// later).
    pub end: usize,
    /// The literal path text (without the leading `@`).
    pub path: String,
    /// `true` if the path is multi-component (contains `/` or `..`). When
    /// `false`, the resolver probes the workspace for single-name matches.
    pub is_multi_component: bool,
    /// `true` if the path is classified as a workspace escape (per §5.2 of
    /// PRD 30a). Multi-component relative paths with `..` segments,
    /// absolute paths, and home-expansion paths are all escapes.
    pub is_escape: bool,
    /// When `is_escape` is `true`, the reason it escaped. `None` for
    /// workspace-relative paths.
    pub escape_reason: Option<EscapeReason>,
    /// Number of `..` segments in the path (0 for non-traversal paths).
    pub escape_depth: u32,
}

// ── MentionError ─────────────────────────────────────────────────────────────

/// A failure encountered while resolving a mention. Attached to
/// [`ExpandedMessage::warnings`] so the TUI can show the user what went
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionError {
    /// The path was empty (e.g. the user typed just `@`).
    EmptyPath,
    /// The workspace store had no file at the (sanitized) path.
    FileNotFound {
        /// The workspace-relative path that was probed.
        path: String,
    },
    /// The path was classified as an escape but the configured
    /// [`EscapePolicy::Never`] is in effect, so the mention was rejected.
    EscapeRejected {
        /// The literal escape path the user typed.
        raw: String,
        /// The classified reason.
        reason: EscapeReason,
    },
    /// The file exceeds [`MentionConfig::resolver_max_file_bytes`].
    TooLarge {
        /// The path that was rejected.
        path: String,
        /// The on-disk size in bytes.
        size: u64,
        /// The cap that was exceeded.
        cap: u64,
    },
    /// The file is neither UTF-8 text nor a recognised image format.
    NotTextOrImage {
        /// The path that was rejected.
        path: String,
        /// The first 8 bytes of the file, hex-encoded, for diagnostic display.
        head_hex: String,
    },
    /// The store reported an I/O error.
    IoError {
        /// The path that failed.
        path: String,
        /// The error message (e.g. permission denied).
        message: String,
    },
}

impl std::fmt::Display for MentionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MentionError::EmptyPath => write!(f, "empty @-mention (path was empty)"),
            MentionError::FileNotFound { path } => {
                write!(f, "workspace has no file at {path:?}")
            }
            MentionError::EscapeRejected { raw, reason } => write!(
                f,
                "escape mention rejected by policy 'never': {raw:?} ({reason:?})"
            ),
            MentionError::TooLarge { path, size, cap } => {
                write!(f, "file {path:?} is {size} bytes (cap {cap}); skipped")
            }
            MentionError::NotTextOrImage { path, head_hex } => write!(
                f,
                "file {path:?} is neither UTF-8 text nor a recognised image (head: {head_hex})"
            ),
            MentionError::IoError { path, message } => {
                write!(f, "I/O error reading {path:?}: {message}")
            }
        }
    }
}

impl std::error::Error for MentionError {}

// ── ResolvedFile ──────────────────────────────────────────────────────────────

/// A mention that has been resolved to actual file content. Becomes a
/// [`ContentBlock::FileRef`] in the final message.
#[derive(Debug, Clone)]
pub struct ResolvedFile {
    /// The literal path the user typed (preserved for transcript display).
    pub path: String,
    /// The resolved file content (text with lossy UTF-8 replacement, or
    /// base64-encoded image bytes).
    pub content: FileRefContent,
    /// Truncation metadata, if the file exceeded the inline caps.
    pub truncation: Option<Truncation>,
    /// Original byte size on disk (pre-truncation).
    pub byte_size: u64,
    /// Original line count on disk (pre-truncation). 0 for images.
    pub line_count: u64,
    /// SHA-256 of the **original** file content, hex-encoded lowercase.
    pub sha256: String,
    /// `Some` when the file path escaped the workspace. The TUI's
    /// per-submission confirmation dialog filters escape mentions out
    /// before they reach the runtime; this field is only populated when
    /// the user has approved the attachment.
    pub escape: Option<EscapeInfo>,
}

impl ResolvedFile {
    /// Convert this resolved file into a [`ContentBlock::FileRef`].
    pub fn into_content_block(self) -> ContentBlock {
        let Self {
            path,
            content,
            truncation,
            byte_size,
            line_count,
            sha256,
            escape,
        } = self;
        ContentBlock::FileRef {
            path,
            content,
            truncation,
            byte_size,
            line_count,
            sha256,
            escape,
        }
    }
}

// ── ExpandedMessage ──────────────────────────────────────────────────────────

/// Output of [`MentionResolver::expand`]: the user's original text
/// re-rendered as a sequence of content blocks, plus per-mention warnings
/// and the list of escape mentions that need a confirmation dialog before
/// they can be sent.
#[derive(Debug, Clone)]
pub struct ExpandedMessage {
    /// Content blocks in the order the user typed them. Text tokens are
    /// [`ContentBlock::Text`]; resolved mentions are
    /// [`ContentBlock::FileRef`]; failed mentions are inlined back as
    /// `@<path>` text in a `Text` block plus a warning in `warnings`.
    pub parts: Vec<ContentBlock>,
    /// Per-mention warnings (file not found, too large, I/O error, etc.).
    /// The TUI renders these as transient inline messages.
    pub warnings: Vec<MentionError>,
    /// Information about each escape mention that was resolved. The TUI
    /// uses this to show the confirmation dialog. The `EscapeInfo` is
    /// already populated on each [`ResolvedFile::escape`]; this list is
    /// exposed separately so the dialog can show "3 files escaped" without
    /// iterating `parts`.
    pub escape_mentions: Vec<EscapeInfo>,
    /// The mention tokens (in source order) that the parser found. Used
    /// for transcript rendering and for tests.
    pub tokens: Vec<MentionToken>,
}

impl ExpandedMessage {
    /// Convert to a [`UserMessage`], collapsing to `Text` when there are
    /// no multipart blocks.
    pub fn into_user_message(self) -> UserMessage {
        UserMessage::from_parts(self.parts)
    }

    /// Convert to a [`Message`] (the first user turn the runtime will see).
    pub fn into_message(self) -> Message {
        self.into_user_message().into_message()
    }

    /// Convenience: collapse to a `MessageContent` (used by tests and by
    /// the runtime boundary).
    pub fn into_message_content(self) -> MessageContent {
        match MessageContent::from_parts(self.parts.clone()) {
            // from_parts collapses empty/single-Text; mirror that here.
            MessageContent::Text(s) => MessageContent::Text(s),
            MessageContent::MultiPart(_) => MessageContent::MultiPart(self.parts),
        }
    }
}

// ── MentionResolver ──────────────────────────────────────────────────────────

/// Resolves `@<path>` mentions in a user message against a [`WorkspaceStore`].
///
/// Stateless; the only inputs are the text, the store, and the config.
/// Each call to [`expand`](Self::expand) is independent.
pub struct MentionResolver;

impl MentionResolver {
    /// Create a new resolver.
    pub fn new() -> Self {
        Self
    }

    /// Find all `@<path>` mentions in `text`. Pure function — no I/O.
    ///
    /// Returns tokens in source order. Overlapping mentions are not
    /// possible (each starts with `@`, so they cannot nest).
    pub fn find_mentions(text: &str) -> Vec<MentionToken> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'@' {
                // Skip email-like `@<word>` mid-sentence when preceded by
                // a non-whitespace character? Per PRD 30a §4.1 we treat
                // every `@` as a potential mention start, but we close the
                // path at non-path characters. This means "user@host"
                // parses as a mention of "host" — acceptable for v1
                // (PRD explicitly notes this edge case in §17.4).
                if let Some((tok_end, raw_path)) = scan_mention_path(&text[i + 1..]) {
                    let start = i;
                    let end = i + 1 + tok_end;
                    let is_multi_component = raw_path.contains('/') || raw_path == "..";
                    let (is_escape, escape_reason, escape_depth) = classify_escape(&raw_path);
                    if !raw_path.is_empty() {
                        out.push(MentionToken {
                            start,
                            end,
                            path: raw_path.to_string(),
                            is_multi_component,
                            is_escape,
                            escape_reason,
                            escape_depth,
                        });
                    }
                    i = end;
                    continue;
                }
            }
            // Advance by one UTF-8 char (not by one byte) to avoid
            // splitting a multi-byte codepoint.
            i += next_char_len(&text[i..]);
        }
        out
    }

    /// Resolve all mentions in `text` against `store` and produce an
    /// [`ExpandedMessage`].
    ///
    /// Reads happen in order. Workspace-relative single-component names
    /// (e.g. `@README.md`) are probed via [`WorkspaceStore::exists`] first;
    /// everything else (multi-component relative, absolute, home,
    /// traversal) is read directly. The caller is expected to enforce
    /// [`MentionConfig::escape_policy`] by passing an `EscapePolicy::Never`
    /// config to suppress escape mention reads.
    pub async fn expand(
        text: &str,
        store: &dyn WorkspaceStore,
        config: &MentionConfig,
    ) -> ExpandedMessage {
        let tokens = Self::find_mentions(text);
        let mut parts: Vec<ContentBlock> = Vec::new();
        let mut warnings: Vec<MentionError> = Vec::new();
        let mut escape_mentions: Vec<EscapeInfo> = Vec::new();
        let mut last_byte: usize = 0;

        for token in &tokens {
            // Push the text between the previous mention and this one.
            if token.start > last_byte {
                let slice = &text[last_byte..token.start];
                parts.push(ContentBlock::Text {
                    text: slice.to_string(),
                });
            }

            match resolve_one(token, store, config).await {
                Ok(Some(resolved)) => {
                    if let Some(esc) = &resolved.escape {
                        escape_mentions.push(esc.clone());
                    }
                    parts.push(resolved.into_content_block());
                }
                Ok(None) => {
                    // Resolver deliberately skipped (e.g. silent on
                    // allowlisted escape under `Always` policy — handled
                    // inside resolve_one). Nothing to push; but a token
                    // should not normally return Ok(None) under any
                    // current policy.
                }
                Err(e) => {
                    // Inline the literal `@<path>` so the user can see
                    // what was skipped.
                    parts.push(ContentBlock::Text {
                        text: format!("@{}", token.path),
                    });
                    warnings.push(e);
                }
            }
            last_byte = token.end;
        }
        // Trailing text after the last mention.
        if last_byte < text.len() {
            parts.push(ContentBlock::Text {
                text: text[last_byte..].to_string(),
            });
        }

        ExpandedMessage {
            parts,
            warnings,
            escape_mentions,
            tokens,
        }
    }
}

impl Default for MentionResolver {
    fn default() -> Self {
        Self::new()
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Scan a path starting at the front of `s` (which is the text *after* the
/// leading `@`). Returns the byte length consumed and the path string.
fn scan_mention_path(s: &str) -> Option<(usize, &str)> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut end = 0;
    for (i, b) in bytes.iter().enumerate() {
        if is_path_char(*b) {
            end = i + 1;
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    // `s` is &str indexed by byte, but we walked by single bytes; trim a
    // partial trailing codepoint (shouldn't happen because is_path_char
    // is ASCII, but be safe).
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Some((end, &s[..end]))
}

/// True if `b` is allowed in a mention path.
fn is_path_char(b: u8) -> bool {
    matches!(b,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' |
        b'_' | b'-' | b'.' | b'/' | b'~' | b'+' | b'='
    )
}

/// Advance by one UTF-8 char.
fn next_char_len(s: &str) -> usize {
    s.chars().next().map_or(1, |c| c.len_utf8())
}

/// Classify a path as workspace-relative or escape.
///
/// Returns `(is_escape, reason, depth)`:
/// - workspace-relative → `(false, None, 0)`
/// - absolute → `(true, Some(Absolute), 0)`
/// - parent-traversal → `(true, Some(ParentTraversal), depth)`
/// - home-expansion → `(true, Some(HomeExpansion), 0)`
pub(crate) fn classify_escape(path: &str) -> (bool, Option<EscapeReason>, u32) {
    if path.starts_with('/') {
        return (true, Some(EscapeReason::Absolute), 0);
    }
    if path == "~" || path.starts_with("~/") || path.starts_with("~user/") {
        return (true, Some(EscapeReason::HomeExpansion), 0);
    }
    let depth = count_parent_traversal(path);
    if depth > 0 {
        return (true, Some(EscapeReason::ParentTraversal), depth);
    }
    (false, None, 0)
}

/// Count the number of `..` segments in `path`. Used to populate
/// `EscapeInfo::depth`.
pub(crate) fn count_parent_traversal(path: &str) -> u32 {
    Path::new(path)
        .components()
        .filter(|c| matches!(c, Component::ParentDir))
        .count() as u32
}

/// Canonicalise a workspace-relative path by stripping leading `./`
/// segments. Returns `None` if the path is empty or root-only.
pub(crate) fn normalise_relative(path: &str) -> Option<String> {
    let p = Path::new(path);
    let mut out = PathBuf::new();
    let mut any = false;
    for c in p.components() {
        match c {
            Component::CurDir => {} // skip "."
            Component::ParentDir => {
                // Caller is expected to check escape depth separately.
                out.push("..");
                any = true;
            }
            Component::Normal(s) => {
                out.push(s);
                any = true;
            }
            Component::RootDir => {
                // workspace-relative path should not start with `/`
                out.push("/");
                any = true;
            }
            Component::Prefix(_) => {
                out.push(c.as_os_str());
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    Some(out.to_string_lossy().replace('\\', "/"))
}

/// Resolve a single mention token to a [`ResolvedFile`]. Reads the file
/// via the workspace store, computes SHA-256, applies truncation, and
/// classifies the path.
async fn resolve_one(
    token: &MentionToken,
    store: &dyn WorkspaceStore,
    config: &MentionConfig,
) -> Result<Option<ResolvedFile>, MentionError> {
    let raw = token.path.trim();
    if raw.is_empty() {
        return Err(MentionError::EmptyPath);
    }

    let (is_escape, reason, depth) = classify_escape(raw);
    if is_escape {
        match config.escape_policy {
            EscapePolicy::Never => {
                return Err(MentionError::EscapeRejected {
                    raw: raw.to_string(),
                    reason: reason.unwrap_or(EscapeReason::Absolute),
                });
            }
            EscapePolicy::Always => {
                // Silently attach; fall through to the read path below.
            }
            EscapePolicy::Confirm => {
                // Read the file (we need the bytes for the dialog display)
                // but the caller is expected to gate submission on the
                // dialog result. We attach EscapeInfo so the dialog can
                // render absolute paths + size + reason.
            }
        }
    }

    // Compute the workspace-relative path we will pass to the store.
    // Escape paths are NOT sanitized; we read them as if the store were a
    // disk-backed store. Workspace-relative paths go through the same
    // sanitize/normalize routine as WriteFileTool.
    let (read_path, canonical_abs) = if is_escape {
        let abs = canonicalise_escape_path(raw).unwrap_or_else(|| PathBuf::from(raw));
        (abs.to_string_lossy().to_string(), Some(abs))
    } else {
        let normalised = normalise_relative(raw).ok_or_else(|| MentionError::EmptyPath)?;
        (normalised.clone(), None)
    };

    // Read up to the cap; use head_bytes to avoid materialising giant
    // files. If the file is under the cap, head_bytes returns the full
    // content (truncated = false).
    let max_bytes = if is_escape {
        config.resolver_max_file_bytes.max(config.max_inline_bytes)
    } else {
        config.resolver_max_file_bytes
    };
    let (bytes, was_truncated) = match store.head_bytes(&read_path, max_bytes).await {
        Ok(pair) => pair,
        Err(AgtrsError::Other(msg)) if msg.contains("not found") => {
            return Err(MentionError::FileNotFound { path: read_path });
        }
        Err(e) => {
            return Err(MentionError::IoError {
                path: read_path,
                message: e.to_string(),
            });
        }
    };

    let total_size = if was_truncated {
        // head_bytes can't tell us the true size without a second call;
        // for in-memory stores the second call is cheap so we ask
        // read_bytes. For disk-backed stores we'd want a stat here, but
        // the default WorkspaceStore doesn't expose that yet.
        match store.read_bytes(&read_path).await {
            Ok(b) => b.len() as u64,
            Err(_) => bytes.len() as u64,
        }
    } else {
        bytes.len() as u64
    };

    if total_size > config.resolver_max_file_bytes as u64 {
        return Err(MentionError::TooLarge {
            path: read_path,
            size: total_size,
            cap: config.resolver_max_file_bytes as u64,
        });
    }

    // SHA-256 of the (possibly truncated) bytes. The Truncation struct
    // distinguishes "truncated" from "full"; the LLM sees the truncated
    // body but the audit log can compare sha256 of the bytes we actually
    // sent. NOTE: PRD 30a §5.4 says "SHA-256 of the **original** file
    // content". For truncated files, we record the sha256 of what we
    // actually sent; a future improvement could compute the full-file
    // sha256 via a streaming hasher, but head_bytes truncates first.
    // Documented in 30b §11.1 as a known limitation of the default
    // head_bytes implementation.
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hex::encode(hasher.finalize());

    // Detect image vs text. Magic-byte table: PNG, JPEG, GIF, WebP.
    let (content, line_count) = if let Some(media_type) = detect_image_mime(&bytes) {
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let content = FileRefContent::Image {
            media_type,
            data_base64,
        };
        (content, 0u64)
    } else if let Ok(text) = std::str::from_utf8(&bytes) {
        // Apply line/byte truncation per the inline caps.
        let (shown_text, truncation) = truncate_text(
            text,
            config.max_inline_lines as u32,
            config.max_inline_bytes as u64,
        );
        let line_count = text.lines().count() as u64;
        (FileRefContent::Text(shown_text), line_count)
    } else {
        return Err(MentionError::NotTextOrImage {
            path: read_path,
            head_hex: hex::encode(&bytes[..bytes.len().min(8)]),
        });
    };

    let truncation = if was_truncated {
        // File exceeded the resolver byte cap; record at the byte level.
        let total_lines = line_count; // we lost the original line count for truncated binary-ish reads
        Some(Truncation {
            shown_lines: line_count,
            total_lines,
            shown_bytes: bytes.len() as u64,
            total_bytes: total_size,
        })
    } else {
        match &content {
            FileRefContent::Text(t) => {
                let total_lines = t.lines().count() as u64;
                let total_bytes = t.len() as u64;
                if total_lines < line_count || total_bytes < total_size {
                    Some(Truncation {
                        shown_lines: total_lines,
                        total_lines: line_count,
                        shown_bytes: total_bytes,
                        total_bytes: total_size,
                    })
                } else {
                    None
                }
            }
            FileRefContent::Image { .. } => None,
        }
    };

    let escape = if is_escape {
        Some(EscapeInfo {
            reason: reason.unwrap_or(EscapeReason::Absolute),
            absolute_path: canonical_abs
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| read_path.clone()),
            depth,
            byte_size: total_size,
        })
    } else {
        None
    };

    Ok(Some(ResolvedFile {
        path: raw.to_string(),
        content,
        truncation,
        byte_size: total_size,
        line_count,
        sha256,
        escape,
    }))
}

/// Truncate `text` to at most `max_lines` lines and `max_bytes` bytes.
/// Returns `(shown_text, Some(Truncation{...}))` if either cap was hit.
fn truncate_text(text: &str, max_lines: u32, max_bytes: u64) -> (String, Option<Truncation>) {
    let total_lines = text.lines().count() as u64;
    let total_bytes = text.len() as u64;
    let mut truncated = false;
    let mut shown_lines = total_lines;
    let mut shown_bytes = total_bytes;
    let mut out = text.to_string();
    if total_lines > max_lines as u64 {
        // Find the byte offset of the max_lines-th line end.
        let mut count = 0u32;
        let mut cut = 0usize;
        for (i, _) in text.match_indices('\n') {
            count += 1;
            cut = i + 1;
            if count == max_lines {
                break;
            }
        }
        if count < max_lines {
            // No newline found; cut at end of text.
            cut = text.len();
        }
        out.truncate(cut);
        shown_lines = max_lines as u64;
        truncated = true;
    }
    if out.len() as u64 > max_bytes {
        out.truncate(max_bytes as usize);
        shown_bytes = max_bytes;
        truncated = true;
    } else {
        shown_bytes = out.len() as u64;
    }
    let trunc = if truncated {
        Some(Truncation {
            shown_lines,
            total_lines,
            shown_bytes,
            total_bytes,
        })
    } else {
        None
    };
    (out, trunc)
}

/// Detect image MIME type from magic bytes. Returns `None` for unknown.
fn detect_image_mime(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        Some("image/png".to_string())
    } else if bytes.len() >= 3 && &bytes[..3] == b"\xFF\xD8\xFF" {
        Some("image/jpeg".to_string())
    } else if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        Some("image/gif".to_string())
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp".to_string())
    } else {
        None
    }
}

/// Canonicalise an escape path. Best-effort: expands `~` to the user's
/// home directory (without doing any other resolution — escape paths
/// may legitimately point at non-existent files for the audit log).
pub(crate) fn canonicalise_escape_path(raw: &str) -> Option<PathBuf> {
    if raw == "~" {
        dirs::home_dir()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir().map(|h| h.join(rest))
    } else if raw.starts_with('/') {
        Some(PathBuf::from(raw))
    } else {
        // Contains `..` or other relative components; resolve against CWD.
        Some(PathBuf::from(raw))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;
    use xaft_config::{EscapePolicy, MentionConfig};

    fn cfg() -> MentionConfig {
        MentionConfig {
            max_inline_lines: 10,
            max_inline_bytes: 200,
            image_max_bytes: 1024,
            resolver_max_file_bytes: 200,
            dedupe: false,
            escape_policy: EscapePolicy::Confirm,
            escape_allowlist: vec![],
        }
    }

    // ── Parser: find_mentions ────────────────────────────────────────────────

    #[test]
    fn find_no_mentions() {
        assert!(MentionResolver::find_mentions("hello world").is_empty());
        assert!(MentionResolver::find_mentions("").is_empty());
    }

    #[test]
    fn find_single_mention() {
        let toks = MentionResolver::find_mentions("see @src/lib.rs please");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].path, "src/lib.rs");
        assert!(toks[0].is_multi_component);
        assert!(!toks[0].is_escape);
    }

    #[test]
    fn find_multiple_mentions() {
        let toks = MentionResolver::find_mentions("@a.rs and @b/c.rs and @d");
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].path, "a.rs");
        assert_eq!(toks[1].path, "b/c.rs");
        assert_eq!(toks[2].path, "d");
    }

    #[test]
    fn find_mention_at_start() {
        let toks = MentionResolver::find_mentions("@x.rs tail");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].path, "x.rs");
    }

    #[test]
    fn find_mention_at_end() {
        let toks = MentionResolver::find_mentions("prefix @x.rs");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].path, "x.rs");
    }

    #[test]
    fn mention_stops_at_punctuation() {
        let toks = MentionResolver::find_mentions("see @a.rs, then @b.rs;");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].path, "a.rs");
        assert_eq!(toks[1].path, "b.rs");
    }

    #[test]
    fn mention_stops_at_whitespace() {
        let toks = MentionResolver::find_mentions("a @one two");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].path, "one");
    }

    #[test]
    fn mention_with_underscore_and_dash() {
        let toks = MentionResolver::find_mentions("@foo_bar-baz.qux");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].path, "foo_bar-baz.qux");
    }

    #[test]
    fn bare_at_does_not_match() {
        assert!(MentionResolver::find_mentions("just an @ symbol").is_empty());
    }

    #[test]
    fn mention_byte_offsets_are_correct() {
        let text = "see @src/lib.rs please";
        let toks = MentionResolver::find_mentions(text);
        assert_eq!(toks[0].start, 4);
        assert_eq!(toks[0].end, 15);
        assert_eq!(&text[toks[0].start..toks[0].end], "@src/lib.rs");
    }

    // ── Parser: classify_escape ─────────────────────────────────────────────

    #[test]
    fn classify_workspace_relative() {
        assert_eq!(classify_escape("a.rs"), (false, None, 0));
        assert_eq!(classify_escape("src/lib.rs"), (false, None, 0));
    }

    #[test]
    fn classify_absolute() {
        let (e, r, d) = classify_escape("/etc/passwd");
        assert!(e);
        assert_eq!(r, Some(EscapeReason::Absolute));
        assert_eq!(d, 0);
    }

    #[test]
    fn classify_parent_traversal() {
        let (e, r, d) = classify_escape("../sibling/foo.rs");
        assert!(e);
        assert_eq!(r, Some(EscapeReason::ParentTraversal));
        assert_eq!(d, 1);
        let (_, _, d2) = classify_escape("../../a.rs");
        assert_eq!(d2, 2);
    }

    #[test]
    fn classify_home_expansion() {
        let (e, r, _) = classify_escape("~/notes.md");
        assert!(e);
        assert_eq!(r, Some(EscapeReason::HomeExpansion));
    }

    #[test]
    fn classify_home_tilde_only() {
        let (e, r, _) = classify_escape("~");
        assert!(e);
        assert_eq!(r, Some(EscapeReason::HomeExpansion));
    }

    // ── Resolver: workspace-relative ────────────────────────────────────────

    #[tokio::test]
    async fn resolve_workspace_relative_text() {
        let store = InMemoryWorkspaceStore::with_files([(
            "src/lib.rs".to_string(),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
        )]);
        let expanded = MentionResolver::expand("see @src/lib.rs", &store, &cfg()).await;
        assert!(expanded.warnings.is_empty());
        assert_eq!(expanded.parts.len(), 2);
        assert!(matches!(expanded.parts[0], ContentBlock::Text { .. }));
        match &expanded.parts[1] {
            ContentBlock::FileRef {
                path,
                content,
                byte_size,
                line_count,
                sha256,
                escape,
                ..
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(escape.is_none());
                assert!(matches!(content, FileRefContent::Text(_)));
                assert!(*byte_size > 0);
                assert_eq!(*line_count, 1);
                assert_eq!(sha256.len(), 64);
            }
            _ => panic!("expected FileRef"),
        }
        assert!(expanded.escape_mentions.is_empty());
    }

    #[tokio::test]
    async fn resolve_single_component_name() {
        let store =
            InMemoryWorkspaceStore::with_files([("README.md".to_string(), "# hello".to_string())]);
        let expanded = MentionResolver::expand("look at @README.md", &store, &cfg()).await;
        assert!(expanded.warnings.is_empty());
        assert_eq!(expanded.parts.len(), 2);
        if let ContentBlock::FileRef { path, .. } = &expanded.parts[1] {
            assert_eq!(path, "README.md");
        } else {
            panic!("expected FileRef");
        }
    }

    #[tokio::test]
    async fn resolve_file_not_found() {
        let store = InMemoryWorkspaceStore::new();
        let expanded = MentionResolver::expand("see @nope.rs", &store, &cfg()).await;
        assert_eq!(expanded.warnings.len(), 1);
        assert!(matches!(
            &expanded.warnings[0],
            MentionError::FileNotFound { .. }
        ));
        // Literal text is inlined.
        let text_concat: String = expanded
            .parts
            .iter()
            .map(|p| match p {
                ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(text_concat.contains("@nope.rs"));
    }

    #[tokio::test]
    async fn resolve_text_truncation() {
        // 5 lines of "abc" (15 bytes total)
        let content = "abc\nabc\nabc\nabc\nabc".to_string();
        let store = InMemoryWorkspaceStore::with_files([("long.rs".to_string(), content.clone())]);
        let mut c = cfg();
        c.max_inline_lines = 2;
        c.max_inline_bytes = 1024;
        let expanded = MentionResolver::expand("@long.rs", &store, &c).await;
        if let ContentBlock::FileRef {
            content: FileRefContent::Text(t),
            truncation,
            ..
        } = &expanded.parts[0]
        {
            assert_eq!(t.lines().count(), 2);
            let tr = truncation.as_ref().expect("truncation recorded");
            assert_eq!(tr.shown_lines, 2);
            assert_eq!(tr.total_lines, 5);
        } else {
            panic!("expected truncated FileRef::Text");
        }
    }

    // ── Resolver: image detection ───────────────────────────────────────────

    #[tokio::test]
    async fn resolve_png_image() {
        // 1×1 transparent PNG
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01".to_vec();
        let store = InMemoryWorkspaceStore::with_files([("logo.png".to_string(), String::new())]);
        // InMemoryWorkspaceStore stores text, so head_bytes returns the
        // text round-trip. To test image detection, we have to stuff raw
        // bytes into a path; the in-memory store's read_bytes is lossy
        // UTF-8 re-encoding. We override the trait locally for the test
        // via a wrapper.
        struct ByteStore(Vec<u8>);
        #[async_trait::async_trait]
        impl WorkspaceStore for ByteStore {
            async fn write(&self, _: &str, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read(&self, _: &str) -> Result<String, AgtrsError> {
                Ok(String::new())
            }
            async fn exists(&self, _: &str) -> bool {
                true
            }
            async fn list(&self) -> Vec<String> {
                vec!["logo.png".into()]
            }
            async fn delete(&self, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read_all(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }
            fn root_display(&self) -> String {
                "<bytes>".into()
            }
            async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, AgtrsError> {
                Ok(self.0.clone())
            }
            async fn head_bytes(&self, _: &str, max: usize) -> Result<(Vec<u8>, bool), AgtrsError> {
                if self.0.len() > max {
                    Ok((self.0[..max].to_vec(), true))
                } else {
                    Ok((self.0.clone(), false))
                }
            }
        }
        let bs = ByteStore(png.clone());
        let expanded = MentionResolver::expand("@logo.png", &bs, &cfg()).await;
        if let ContentBlock::FileRef {
            content:
                FileRefContent::Image {
                    media_type,
                    data_base64,
                },
            ..
        } = &expanded.parts[0]
        {
            assert_eq!(media_type, "image/png");
            // Round-trip the base64 and confirm we got the bytes back.
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data_base64)
                .unwrap();
            assert_eq!(decoded, png);
        } else {
            panic!("expected FileRef::Image");
        }
    }

    // ── Resolver: escape classification (read skipped when policy=Never) ──

    #[tokio::test]
    async fn resolve_escape_policy_never_rejects() {
        let store = InMemoryWorkspaceStore::new();
        let mut c = cfg();
        c.escape_policy = EscapePolicy::Never;
        let expanded = MentionResolver::expand("see @../foo.rs", &store, &c).await;
        assert!(
            expanded
                .warnings
                .iter()
                .any(|w| matches!(w, MentionError::EscapeRejected { .. }))
        );
        assert!(expanded.escape_mentions.is_empty());
    }

    #[tokio::test]
    async fn resolve_absolute_path_records_escape_info() {
        // Build a ByteStore that returns a small text file for any path.
        struct AnyStore;
        #[async_trait::async_trait]
        impl WorkspaceStore for AnyStore {
            async fn write(&self, _: &str, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read(&self, _: &str) -> Result<String, AgtrsError> {
                Ok("hi".into())
            }
            async fn exists(&self, _: &str) -> bool {
                true
            }
            async fn list(&self) -> Vec<String> {
                vec![]
            }
            async fn delete(&self, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read_all(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }
            fn root_display(&self) -> String {
                "<any>".into()
            }
            async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, AgtrsError> {
                Ok(b"hi".to_vec())
            }
            async fn head_bytes(&self, _: &str, _: usize) -> Result<(Vec<u8>, bool), AgtrsError> {
                Ok((b"hi".to_vec(), false))
            }
        }
        let store = AnyStore;
        let expanded = MentionResolver::expand("see @/etc/hosts", &store, &cfg()).await;
        assert!(expanded.warnings.is_empty());
        assert_eq!(expanded.escape_mentions.len(), 1);
        assert_eq!(expanded.escape_mentions[0].reason, EscapeReason::Absolute);
        if let ContentBlock::FileRef { escape, .. } = &expanded.parts[1] {
            let esc = escape.as_ref().unwrap();
            assert_eq!(esc.reason, EscapeReason::Absolute);
            assert!(!esc.absolute_path.is_empty());
        } else {
            panic!("expected FileRef at parts[1], got {:#?}", expanded.parts);
        }
    }

    #[tokio::test]
    async fn resolve_parent_traversal_records_escape_info() {
        struct AnyStore;
        #[async_trait::async_trait]
        impl WorkspaceStore for AnyStore {
            async fn write(&self, _: &str, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read(&self, _: &str) -> Result<String, AgtrsError> {
                Ok("ok".into())
            }
            async fn exists(&self, _: &str) -> bool {
                true
            }
            async fn list(&self) -> Vec<String> {
                vec![]
            }
            async fn delete(&self, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read_all(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }
            fn root_display(&self) -> String {
                "<any>".into()
            }
            async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, AgtrsError> {
                Ok(b"ok".to_vec())
            }
            async fn head_bytes(&self, _: &str, _: usize) -> Result<(Vec<u8>, bool), AgtrsError> {
                Ok((b"ok".to_vec(), false))
            }
        }
        let store = AnyStore;
        let expanded = MentionResolver::expand("@../sibling/foo.rs", &store, &cfg()).await;
        assert!(expanded.warnings.is_empty());
        assert_eq!(expanded.escape_mentions.len(), 1);
        assert_eq!(
            expanded.escape_mentions[0].reason,
            EscapeReason::ParentTraversal
        );
        assert_eq!(expanded.escape_mentions[0].depth, 1);
    }

    #[tokio::test]
    async fn resolve_home_expansion_records_escape_info() {
        struct AnyStore;
        #[async_trait::async_trait]
        impl WorkspaceStore for AnyStore {
            async fn write(&self, _: &str, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read(&self, _: &str) -> Result<String, AgtrsError> {
                Ok("x".into())
            }
            async fn exists(&self, _: &str) -> bool {
                true
            }
            async fn list(&self) -> Vec<String> {
                vec![]
            }
            async fn delete(&self, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read_all(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }
            fn root_display(&self) -> String {
                "<any>".into()
            }
            async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, AgtrsError> {
                Ok(b"x".to_vec())
            }
            async fn head_bytes(&self, _: &str, _: usize) -> Result<(Vec<u8>, bool), AgtrsError> {
                Ok((b"x".to_vec(), false))
            }
        }
        let store = AnyStore;
        let expanded = MentionResolver::expand("@~/notes.md", &store, &cfg()).await;
        assert!(expanded.warnings.is_empty());
        assert_eq!(expanded.escape_mentions.len(), 1);
        assert_eq!(
            expanded.escape_mentions[0].reason,
            EscapeReason::HomeExpansion
        );
    }

    // ── Resolver: too-large ─────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_too_large() {
        struct BigStore;
        #[async_trait::async_trait]
        impl WorkspaceStore for BigStore {
            async fn write(&self, _: &str, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read(&self, _: &str) -> Result<String, AgtrsError> {
                Ok("x".into())
            }
            async fn exists(&self, _: &str) -> bool {
                true
            }
            async fn list(&self) -> Vec<String> {
                vec![]
            }
            async fn delete(&self, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read_all(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }
            fn root_display(&self) -> String {
                "<big>".into()
            }
            async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, AgtrsError> {
                Ok(vec![b'x'; 10_000])
            }
            async fn head_bytes(&self, _: &str, _: usize) -> Result<(Vec<u8>, bool), AgtrsError> {
                Ok((vec![b'x'; 10_000], false))
            }
        }
        let store = BigStore;
        let mut c = cfg();
        c.resolver_max_file_bytes = 100;
        let expanded = MentionResolver::expand("@huge.bin", &store, &c).await;
        assert!(
            expanded
                .warnings
                .iter()
                .any(|w| matches!(w, MentionError::TooLarge { .. }))
        );
    }

    // ── Resolver: binary file (not text or image) ───────────────────────────

    #[tokio::test]
    async fn resolve_binary_not_text_or_image() {
        struct BinStore;
        #[async_trait::async_trait]
        impl WorkspaceStore for BinStore {
            async fn write(&self, _: &str, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read(&self, _: &str) -> Result<String, AgtrsError> {
                Ok(String::new())
            }
            async fn exists(&self, _: &str) -> bool {
                true
            }
            async fn list(&self) -> Vec<String> {
                vec![]
            }
            async fn delete(&self, _: &str) -> Result<(), AgtrsError> {
                Ok(())
            }
            async fn read_all(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }
            fn root_display(&self) -> String {
                "<bin>".into()
            }
            async fn read_bytes(&self, _: &str) -> Result<Vec<u8>, AgtrsError> {
                // 16 random non-UTF-8 bytes that don't match any image magic.
                Ok(vec![
                    0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
                    0x0A, 0x0B, 0x0C,
                ])
            }
            async fn head_bytes(&self, _: &str, _: usize) -> Result<(Vec<u8>, bool), AgtrsError> {
                Ok((
                    vec![
                        0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
                        0x09, 0x0A, 0x0B, 0x0C,
                    ],
                    false,
                ))
            }
        }
        let store = BinStore;
        let expanded = MentionResolver::expand("@blob.bin", &store, &cfg()).await;
        assert!(
            expanded
                .warnings
                .iter()
                .any(|w| matches!(w, MentionError::NotTextOrImage { .. }))
        );
    }

    // ── Resolver: interleave text + mentions + warnings ────────────────────

    #[tokio::test]
    async fn expand_interleaves_text_and_mentions() {
        let store = InMemoryWorkspaceStore::with_files([("b.rs".to_string(), "B".to_string())]);
        let expanded =
            MentionResolver::expand("intro @a.rs (missing) middle @b.rs end", &store, &cfg()).await;
        // 4 parts: "intro ", "@a.rs" (inlined), " middle ", FileRef, " end"
        assert!(
            expanded
                .warnings
                .iter()
                .any(|w| matches!(w, MentionError::FileNotFound { .. }))
        );
        // Verify text order via lossy concat.
        let text = expanded
            .parts
            .iter()
            .map(|p| match p {
                ContentBlock::Text { text } => text.clone(),
                ContentBlock::FileRef { path, .. } => format!("<@{path}>"),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("intro"));
        assert!(text.contains("@a.rs"));
        assert!(text.contains("@b.rs"));
        assert!(text.contains("end"));
    }

    // ── SHA-256 stability ───────────────────────────────────────────────────

    #[tokio::test]
    async fn sha256_is_correct() {
        let store = InMemoryWorkspaceStore::with_files([("a.txt".to_string(), "abc".to_string())]);
        let expanded = MentionResolver::expand("@a.txt", &store, &cfg()).await;
        if let ContentBlock::FileRef { sha256, .. } = &expanded.parts[0] {
            // sha256("abc") = ba7816bf...f20015ad
            assert!(sha256.starts_with("ba7816bf8f01cfea"));
        } else {
            panic!("expected FileRef");
        }
    }

    // ── detect_image_mime coverage ──────────────────────────────────────────

    #[test]
    fn detect_jpeg_magic() {
        assert_eq!(
            detect_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Some("image/jpeg".to_string())
        );
    }

    #[test]
    fn detect_gif_magic() {
        assert_eq!(
            detect_image_mime(b"GIF89a..."),
            Some("image/gif".to_string())
        );
    }

    #[test]
    fn detect_webp_magic() {
        assert_eq!(
            detect_image_mime(b"RIFF\x00\x00\x00\x00WEBP"),
            Some("image/webp".to_string())
        );
    }

    #[test]
    fn detect_no_magic() {
        assert_eq!(detect_image_mime(b"hello world"), None);
    }

    // ── normalise_relative ─────────────────────────────────────────────────

    #[test]
    fn normalise_strips_current_dir() {
        assert_eq!(normalise_relative("./a.rs").unwrap(), "a.rs");
        assert_eq!(normalise_relative("a/./b.rs").unwrap(), "a/b.rs");
    }

    #[test]
    fn normalise_preserves_parent_traversal() {
        assert_eq!(normalise_relative("../a.rs").unwrap(), "../a.rs");
    }

    #[test]
    fn normalise_empty_returns_none() {
        assert!(normalise_relative(".").is_none());
    }

    // ── canonicalise_escape_path ───────────────────────────────────────────

    #[test]
    fn canonicalise_absolute() {
        assert_eq!(
            canonicalise_escape_path("/etc/hosts").unwrap(),
            PathBuf::from("/etc/hosts")
        );
    }

    #[test]
    fn canonicalise_traversal() {
        // Best-effort: we don't resolve against CWD here, but the function
        // should at least return *something* parseable as a PathBuf.
        let p = canonicalise_escape_path("../foo.rs").unwrap();
        assert!(p.to_string_lossy().contains("foo.rs"));
    }

    // ── truncate_text ──────────────────────────────────────────────────────

    #[test]
    fn truncate_text_no_truncation() {
        let s = "abc\ndef";
        let (out, t) = truncate_text(s, 10, 100);
        assert_eq!(out, s);
        assert!(t.is_none());
    }

    #[test]
    fn truncate_text_by_lines() {
        let s = "a\nb\nc\nd";
        let (out, t) = truncate_text(s, 2, 1000);
        assert_eq!(out, "a\nb\n");
        let t = t.unwrap();
        assert_eq!(t.shown_lines, 2);
        assert_eq!(t.total_lines, 4);
    }

    #[test]
    fn truncate_text_by_bytes() {
        let s = "abcdef";
        let (out, t) = truncate_text(s, 100, 3);
        assert_eq!(out, "abc");
        let t = t.unwrap();
        assert_eq!(t.shown_bytes, 3);
        assert_eq!(t.total_bytes, 6);
    }
}
