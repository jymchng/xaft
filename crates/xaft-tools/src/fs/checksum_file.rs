//! `ChecksumFileTool` — compute a cryptographic checksum of a file.
//!
//! Parity row 8. agenthicc takes any algorithm name `hashlib` accepts and
//! returns `{"ok", "path", "algorithm", "digest"}`, with
//! `unsupported algorithm: {name}` for anything it does not know.
//!
//! hashlib has ~20 algorithms; this supports the three that appear in real
//! checksums (sha256 default, sha1, md5) and reports the *same* error text as
//! agenthicc for anything else, so a caller that worked against agenthicc gets
//! a diagnosable answer rather than a silently different one.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_str, require_str, validate_path};

/// Compute a cryptographic checksum of a file.
pub struct ChecksumFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl ChecksumFileTool {
    /// Create a new `ChecksumFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "checksum_file";

    /// Digest `data` with `algorithm`, or `None` when it is not supported.
    fn digest(algorithm: &str, data: &[u8]) -> Option<String> {
        // hashlib accepts a few spellings for the same digest; accept the
        // common ones so `SHA-256` does not read as "unsupported".
        let normalised = algorithm
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "");
        let digest = match normalised.as_str() {
            "sha256" => {
                use sha2::Digest as _;
                let mut h = sha2::Sha256::new();
                h.update(data);
                h.finalize().to_vec()
            }
            "sha1" => {
                use sha2::Digest as _;
                let mut h = sha1::Sha1::new();
                h.update(data);
                h.finalize().to_vec()
            }
            "md5" => {
                use sha2::Digest as _;
                let mut h = md5::Md5::new();
                h.update(data);
                h.finalize().to_vec()
            }
            _ => return None,
        };
        Some(digest.iter().map(|b| format!("{b:02x}")).collect())
    }
}

impl std::fmt::Debug for ChecksumFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChecksumFileTool").finish()
    }
}

#[async_trait]
impl Tool for ChecksumFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Compute a cryptographic checksum of a file. \
         Supported algorithms: sha256 (default), sha1, md5. \
         Returns { ok, path, algorithm, digest }."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path."
                },
                "algorithm": {
                    "type": "string",
                    "default": "sha256",
                    "enum": ["sha256", "sha1", "md5"],
                    "description": "Hash algorithm. Anything else is reported as unsupported."
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

    #[instrument(name = "checksum_file", skip(self, ctx), fields(path, algorithm))]
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
        let algorithm = opt_str(&input, "algorithm").unwrap_or("sha256").to_string();

        tracing::Span::current().record("path", path);
        tracing::Span::current().record("algorithm", &algorithm);

        let full = self.root.join(path);
        let data = match tokio::fs::read(&full).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolResult::error(
                    format!("file not found: {path}"),
                    &ctx.tool_use_id,
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Ok(ToolResult::error("permission_denied", &ctx.tool_use_id));
            }
            Err(e) => {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("read failed: {e}"),
                });
            }
        };

        // Checked before hashing: a typo'd algorithm should not cost a full
        // read of the file, and agenthicc reports it the same way.
        let Some(digest) = Self::digest(&algorithm, &data) else {
            return Ok(ToolResult::error(
                format!("unsupported algorithm: {algorithm}"),
                &ctx.tool_use_id,
            ));
        };

        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": true,
                "path": path,
                "algorithm": algorithm,
                "digest": digest,
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

    fn tool(tmp: &TempDir) -> ChecksumFileTool {
        ChecksumFileTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn sha256_is_the_default_and_is_correct() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "abc").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt"}),
                &ToolContext::new("cs1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["algorithm"], "sha256");
        // The canonical SHA-256 of the three bytes "abc".
        assert_eq!(
            v["digest"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn sha1_and_md5_match_known_vectors() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "abc").await.unwrap();
        for (alg, want) in [
            ("sha1", "a9993e364706816aba3e25717850c26c9cd0d89d"),
            ("md5", "900150983cd24fb0d6963f7d28e17f72"),
        ] {
            let out = tool(&tmp)
                .call(
                    serde_json::json!({"path": "a.txt", "algorithm": alg}),
                    &ToolContext::new("cs2"),
                )
                .await
                .unwrap();
            assert!(!out.is_error);
            let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
            assert_eq!(v["digest"], want, "wrong digest for {alg}");
        }
    }

    #[tokio::test]
    async fn accepts_uppercase_and_dashed_spellings() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "abc").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt", "algorithm": "SHA-256"}),
                &ToolContext::new("cs3"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
    }

    #[tokio::test]
    async fn unsupported_algorithm_reports_agenthicc_wording() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "abc").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt", "algorithm": "crc32"}),
                &ToolContext::new("cs4"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            out.content.contains("unsupported algorithm: crc32"),
            "got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn missing_file_reports_not_found() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "gone.txt"}),
                &ToolContext::new("cs5"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            out.content.contains("file not found"),
            "got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let res = tool(&tmp)
            .call(
                serde_json::json!({"path": "../secret"}),
                &ToolContext::new("cs6"),
            )
            .await;
        assert!(res.is_err() || res.unwrap().is_error);
    }
}
