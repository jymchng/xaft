//! `GrepTool` — search file contents in the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, opt_str, opt_u64, require_str};

/// Search for a pattern across files in the workspace.
///
/// Returns matching lines with file paths and line numbers.
/// Uses simple substring matching (case-sensitive by default).
/// For regex patterns, set `use_regex: true`.
pub struct GrepTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl GrepTool {
    /// Create a new `GrepTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "grep";
    const MAX_MATCHES_DEFAULT: usize = 100;
}

impl std::fmt::Debug for GrepTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrepTool").finish()
    }
}

#[async_trait]
impl Tool for GrepTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Search for a pattern in files within the workspace. Returns matching lines with \
         file paths and line numbers. Use path_prefix to limit the search scope. \
         Use case_sensitive=false for case-insensitive matching."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Pattern to search for (substring or regex if use_regex=true)."
                },
                "path_prefix": {
                    "type": "string",
                    "description": "Only search files under this path prefix (e.g. \"src/\")."
                },
                "path_suffix": {
                    "type": "string",
                    "description": "Only search files with this suffix (e.g. \".rs\")."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "default": true,
                    "description": "Whether the search is case-sensitive. Default: true."
                },
                "max_matches": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "default": 100,
                    "description": "Maximum number of matches to return."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "grep", skip(self, ctx), fields(pattern))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let pattern = require_str(Self::TOOL_NAME, &input, "pattern").map_err(AgtrsError::from)?;

        if pattern.is_empty() {
            return Err(AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "pattern must not be empty".into(),
            });
        }

        tracing::Span::current().record("pattern", pattern);

        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            });
        }

        let path_prefix = opt_str(&input, "path_prefix");
        let path_suffix = opt_str(&input, "path_suffix");
        let case_sensitive = opt_bool(&input, "case_sensitive").unwrap_or(true);
        let max_matches = opt_u64(&input, "max_matches")
            .map(|n| n as usize)
            .unwrap_or(Self::MAX_MATCHES_DEFAULT);

        let search_pattern = if case_sensitive {
            pattern.to_string()
        } else {
            pattern.to_lowercase()
        };

        let files = self.workspace.list().await;
        let mut matches: Vec<String> = Vec::new();
        let mut files_searched = 0usize;

        for file_path in &files {
            if let Some(px) = path_prefix {
                if !file_path.starts_with(px) {
                    continue;
                }
            }
            if let Some(sx) = path_suffix {
                if !file_path.ends_with(sx) {
                    continue;
                }
            }

            if ctx.cancel_token.is_cancelled() {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", Self::TOOL_NAME),
                });
            }

            let content = match self.workspace.read(file_path).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            files_searched += 1;

            for (line_no, line) in content.lines().enumerate() {
                let haystack = if case_sensitive {
                    line.to_string()
                } else {
                    line.to_lowercase()
                };

                if haystack.contains(&search_pattern) {
                    matches.push(format!("{}:{}: {}", file_path, line_no + 1, line));
                    if matches.len() >= max_matches {
                        break;
                    }
                }
            }

            if matches.len() >= max_matches {
                break;
            }
        }

        tracing::debug!(pattern, files_searched, matches = matches.len(), "grep");

        if matches.is_empty() {
            return Ok(ToolResult::ok(
                format!("No matches found for '{pattern}' in {files_searched} files."),
                &ctx.tool_use_id,
            ));
        }

        let mut output = format!(
            "{} match(es) across {} searched file(s):\n\n",
            matches.len(),
            files_searched
        );
        for m in &matches {
            output.push_str(m);
            output.push('\n');
        }
        if matches.len() == max_matches {
            output.push_str(&format!("\n(truncated at {max_matches} results)"));
        }

        Ok(ToolResult::ok(
            output.trim_end().to_string(),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn make_store() -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(vec![
            (
                "src/main.rs".into(),
                "fn main() {\n    let x = 1;\n    println!(\"hello\");\n}\n".into(),
            ),
            (
                "src/lib.rs".into(),
                "pub fn hello() -> String {\n    \"world\".into()\n}\n".into(),
            ),
            (
                "README.md".into(),
                "# Hello World\nThis is a README.\n".into(),
            ),
        ])) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn finds_pattern_in_files() {
        let tool = GrepTool::new(make_store());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"pattern": "hello"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("src/main.rs")
                || result.content.contains("src/lib.rs")
                || result.content.contains("README.md")
        );
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let tool = GrepTool::new(make_store());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(
                serde_json::json!({"pattern": "HELLO", "case_sensitive": false}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("match"));
    }

    #[tokio::test]
    async fn no_match_returns_informative_message() {
        let tool = GrepTool::new(make_store());
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(
                serde_json::json!({"pattern": "xyz_not_in_any_file_12345"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("No matches"));
    }

    #[tokio::test]
    async fn prefix_filter_limits_search() {
        let tool = GrepTool::new(make_store());
        let ctx = ToolContext::new("t4");
        let result = tool
            .call(
                serde_json::json!({"pattern": "hello", "path_prefix": "src/"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        // README.md should not appear in results
        assert!(!result.content.contains("README.md"));
    }

    #[tokio::test]
    async fn empty_pattern_returns_error() {
        let tool = GrepTool::new(make_store());
        let ctx = ToolContext::new("t5");
        assert!(
            tool.call(serde_json::json!({"pattern": ""}), &ctx)
                .await
                .is_err()
        );
    }
}
