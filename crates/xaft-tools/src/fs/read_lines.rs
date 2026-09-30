//! `ReadLinesTool` \u2014 read a 1-indexed, inclusive line range from a file.
//!
//! Parity row 20. This is genuinely distinct from `read_file` (whole file)
//! and from `batch_read` (many files): an agent asking for lines 40-80 of a
//! 3,000-line file should not pay for the other 2,920.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_u64, require_str, validate_path};

/// Read a specific range of lines from a file (1-indexed, inclusive).
pub struct ReadLinesTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl ReadLinesTool {
    /// Create a new `ReadLinesTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "read_lines";
}

impl std::fmt::Debug for ReadLinesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadLinesTool").finish()
    }
}

#[async_trait]
impl Tool for ReadLinesTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Read a specific range of lines from a file (1-indexed). \
         `start` defaults to 1 and `end` defaults to end-of-file. \
         Returns the selected lines plus the file's total line count."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path."
                },
                "start": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 1,
                    "description": "First line to read (1-indexed)."
                },
                "end": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to read inclusive. Omit for end of file."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "read_lines", skip(self, ctx), fields(path, start, end))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            });
        }

        let path = require_str(Self::TOOL_NAME, &input, "path").map_err(AgtrsError::from)?;
        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        // agenthicc types `start` as a plain int defaulting to 1; a 0 or
        // negative start would silently produce an empty range, which reads
        // like "the file is empty" rather than "you asked for nonsense".
        let start = opt_u64(&input, "start").unwrap_or(1);
        if start == 0 {
            return Ok(ToolResult::error(
                "'start' is 1-indexed and must be at least 1",
                &ctx.tool_use_id,
            ));
        }
        let end = opt_u64(&input, "end");

        tracing::Span::current().record("path", path);
        tracing::Span::current().record("start", start);
        tracing::Span::current().record("end", end.unwrap_or(0));

        let full = self.root.join(path);
        let text = match tokio::fs::read_to_string(&full).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolResult::error(
                    format!("File not found: '{path}'"),
                    &ctx.tool_use_id,
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Ok(ToolResult::error(
                    format!("'{path}' is not valid UTF-8 text"),
                    &ctx.tool_use_id,
                ));
            }
            Err(e) => {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("read failed: {e}"),
                });
            }
        };

        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();

        // Clamp rather than error: a range that runs off the end of a file is
        // an ordinary thing to ask for, and agenthicc's tail reads rely on it.
        let from = start.min(total as u64).max(1) as usize - 1;
        let to = match end {
            Some(e) if e > 0 => (e as usize).min(total),
            _ => total,
        };

        if from >= total {
            return Ok(ToolResult::error(
                format!("'{path}' has {total} line(s); start {start} is past the end"),
                &ctx.tool_use_id,
            ));
        }

        let selected: Vec<&str> = if to > from {
            lines[from..to].to_vec()
        } else {
            vec![lines[from]]
        };

        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": true,
                "path": path,
                "start": from + 1,
                "end": from + selected.len(),
                "total_lines": total,
                "content": selected.join("\n"),
            })
            .to_string(),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_store::FsWorkspaceStore;
    use tempfile::TempDir;
    use tokio::fs;

    fn tool(tmp: &TempDir) -> ReadLinesTool {
        ReadLinesTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    async fn three_lines(tmp: &TempDir) {
        fs::write(tmp.path().join("f.txt"), "one\ntwo\nthree\n")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reads_inclusive_range() {
        let tmp = TempDir::new().unwrap();
        three_lines(&tmp).await;
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "f.txt", "start": 2, "end": 3}),
                &ToolContext::new("rl1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["content"], "two\nthree");
        assert_eq!(v["total_lines"], 3);
    }

    #[tokio::test]
    async fn defaults_to_whole_file() {
        let tmp = TempDir::new().unwrap();
        three_lines(&tmp).await;
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "f.txt"}),
                &ToolContext::new("rl2"),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["content"], "one\ntwo\nthree");
    }

    #[tokio::test]
    async fn end_past_eof_is_clamped() {
        let tmp = TempDir::new().unwrap();
        three_lines(&tmp).await;
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "f.txt", "start": 2, "end": 999}),
                &ToolContext::new("rl3"),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["content"], "two\nthree");
        assert_eq!(v["total_lines"], 3);
    }

    #[tokio::test]
    async fn start_zero_is_rejected_rather_than_silently_empty() {
        let tmp = TempDir::new().unwrap();
        three_lines(&tmp).await;
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "f.txt", "start": 0}),
                &ToolContext::new("rl4"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn missing_file_errors() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "absent.txt"}),
                &ToolContext::new("rl5"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let res = tool(&tmp)
            .call(
                serde_json::json!({"path": "../escape.txt"}),
                &ToolContext::new("rl6"),
            )
            .await;
        assert!(res.is_err() || res.unwrap().is_error);
    }
}
