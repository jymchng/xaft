//! `mcp__<server>__<tool>` — one connected server's tool, as an agent tool.
//!
//! Port of agenthicc's `AgenthiccMcpTool` (`tools/mcp.py`). The manager binds
//! every published tool to a provider-safe name ([`provider_safe_tool_name`])
//! and hands the bindings to a run as [`McpTool`]s; a call goes back through
//! the manager, which owns the live connection and refuses a binding whose
//! catalog has been replaced.
//!
//! # Naming
//!
//! agenthicc keeps the canonical identity `mcp:<server>:<tool>` and offers the
//! provider `mcp_<server>_<tool>` (its `_provider_safe_tool_name` turns every
//! character outside `[A-Za-z0-9_-]` into `_`). xaft offers
//! `mcp__<server>__<tool>` instead — the spelling `xaft_tools::capabilities`
//! classifies as `network` by its prefix, so every mode, phase and approval
//! filter treats MCP tools as outbound calls without a table of their names —
//! through the same sanitiser, 64-character cap and digest suffix. The
//! canonical identity is unchanged.
//!
//! # Results
//!
//! [`extract_tool_content`] is agenthicc's `_extract_tool_content`:
//! `structuredContent.result` when present; else one text block (decoded when
//! it is JSON), one non-text block's `data`, or a list for several blocks. A
//! result with `isError: true` is a failed tool call the model sees as an
//! error, never a transport fault.

use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::manager::{McpSessionManager, sha256_hex};

/// Provider tool names are at most this long (agenthicc's
/// `_PROVIDER_TOOL_NAME_MAX_LENGTH`).
pub const PROVIDER_TOOL_NAME_MAX_LENGTH: usize = 64;

/// agenthicc's `_provider_safe_tool_name`: every character outside
/// `[A-Za-z0-9_-]` becomes `_`, and a name over 64 characters is cut to 55
/// and given `_` plus the first 8 hex digits of its SHA-256.
pub fn provider_safe_tool_name(name: &str) -> String {
    let mut safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        safe = "mcp_tool".into();
    }
    if safe.len() > PROVIDER_TOOL_NAME_MAX_LENGTH {
        let digest = sha256_hex(name);
        safe.truncate(PROVIDER_TOOL_NAME_MAX_LENGTH - 9);
        safe = format!("{safe}_{}", &digest[..8]);
    }
    safe
}

/// One published tool, bound to the name agents call it by.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolBinding {
    /// The provider-safe name: `mcp__<server>__<tool>`.
    pub provider_name: String,
    /// agenthicc's canonical identity: `mcp:<server>:<tool>`.
    pub canonical_name: String,
    /// The server.
    pub server: String,
    /// The tool's own name on that server.
    pub tool: String,
    /// The server's description of the tool.
    pub description: String,
    /// The server's `inputSchema`.
    pub input_schema: Value,
    /// The catalog revision the binding belongs to.
    pub catalog_revision: u64,
}

/// An MCP server's tool, callable by an agent.
///
/// The capability is `network` (by name, in `xaft_tools::capabilities`), so
/// Plan mode never offers it, Safe mode asks before every call, and a phase
/// sees it only when its capabilities allow outbound calls. It asks for no
/// confirmation of its own, exactly like agenthicc's MCP tools.
pub struct McpTool {
    manager: Arc<McpSessionManager>,
    binding: McpToolBinding,
    description: String,
    schema: Value,
}

impl std::fmt::Debug for McpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTool")
            .field("name", &self.binding.provider_name)
            .field("revision", &self.binding.catalog_revision)
            .finish()
    }
}

impl McpTool {
    /// Bind `binding` to `manager`.
    pub fn new(manager: Arc<McpSessionManager>, binding: McpToolBinding) -> Self {
        let description = if binding.description.trim().is_empty() {
            format!(
                "Tool '{}' from MCP server '{}'.",
                binding.tool, binding.server
            )
        } else {
            binding.description.clone()
        };
        Self {
            schema: provider_schema(&binding.input_schema),
            description,
            manager,
            binding,
        }
    }

    /// The binding this tool calls through.
    pub fn binding(&self) -> &McpToolBinding {
        &self.binding
    }
}

/// The server's input schema, as a provider accepts it: always an object
/// schema, `{"type": "object", "properties": {}}` when the server sent none.
fn provider_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) if !map.is_empty() => {
            let mut map = map.clone();
            map.entry("type").or_insert_with(|| json!("object"));
            if map.get("type") == Some(&json!("object")) {
                map.entry("properties").or_insert_with(|| json!({}));
            }
            Value::Object(map)
        }
        _ => json!({"type": "object", "properties": {}}),
    }
}

#[async_trait]
impl Tool for McpTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        &self.binding.provider_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", self.binding.provider_name),
            });
        }
        let arguments = match input {
            Value::Null => json!({}),
            Value::Object(_) => input,
            other => {
                return Ok(ToolResult::error(
                    format!(
                        "MCP tool '{}' takes a JSON object of arguments, not {other}",
                        self.binding.canonical_name
                    ),
                    &ctx.tool_use_id,
                ));
            }
        };
        let outcome = tokio::select! {
            biased;
            _ = ctx.cancel_token.cancelled() => {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", self.binding.provider_name),
                });
            }
            outcome = self.manager.call_tool(&self.binding, arguments) => outcome,
        };
        Ok(match outcome {
            Ok(value) => ToolResult::ok(render(&value), &ctx.tool_use_id),
            Err(error) => ToolResult::error(error, &ctx.tool_use_id),
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        // A remote tool may have side effects whose order matters.
        false
    }
}

/// The text a tool result carries: a string as itself, anything else as
/// compact JSON (what agenthicc's runner sends for a non-string return).
pub fn render(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// agenthicc's `_extract_tool_content`: the usable payload of a
/// `CallToolResult`.
pub fn extract_tool_content(result: &Value) -> Value {
    if let Some(structured) = result.get("structuredContent").and_then(Value::as_object) {
        if let Some(inner) = structured.get("result") {
            return inner.clone();
        }
    }
    let content = match result.get("content") {
        None | Some(Value::Null) => return Value::Null,
        Some(Value::Array(blocks)) if blocks.is_empty() => return Value::Null,
        Some(Value::Array(blocks)) => blocks,
        Some(other) => return other.clone(),
    };
    if content.len() == 1 {
        let block = &content[0];
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            return serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()));
        }
        return block
            .get("data")
            .filter(|d| truthy(d))
            .cloned()
            .unwrap_or_else(|| Value::String(block.to_string()));
    }
    Value::Array(
        content
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .filter(|t| truthy(t))
                    .or_else(|| block.get("data").filter(|d| truthy(d)))
                    .cloned()
                    .unwrap_or_else(|| Value::String(block.to_string()))
            })
            .collect(),
    )
}

/// agenthicc's `_extract_text_content`: the text of an error result.
pub fn extract_text_content(result: &Value) -> String {
    match result.get("content") {
        Some(Value::Array(blocks)) if !blocks.is_empty() => blocks
            .iter()
            .map(|block| match block.get("text") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => block.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::String(text)) if !text.is_empty() => text.clone(),
        _ => result.to_string(),
    }
}

/// Python truthiness of a JSON value, for agenthicc's `a or b` chains.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_names_are_sanitised_and_capped_like_agenthiccs() {
        assert_eq!(
            provider_safe_tool_name("mcp__github__create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(
            provider_safe_tool_name("mcp__docs.v2__search pages"),
            "mcp__docs_v2__search_pages"
        );
        assert_eq!(provider_safe_tool_name(""), "mcp_tool");
        let long = format!("mcp__srv__{}", "x".repeat(80));
        let safe = provider_safe_tool_name(&long);
        assert_eq!(safe.len(), PROVIDER_TOOL_NAME_MAX_LENGTH);
        assert!(safe.starts_with("mcp__srv__xxx"));
        assert_eq!(&safe[55..56], "_");
        assert_eq!(&safe[56..], &sha256_hex(&long)[..8]);
    }

    #[test]
    fn content_extraction_follows_agenthicc() {
        assert_eq!(
            extract_tool_content(&json!({"structuredContent": {"result": [1, 2]}})),
            json!([1, 2])
        );
        assert_eq!(
            extract_tool_content(&json!({"content": [{"type": "text", "text": "plain"}]})),
            json!("plain")
        );
        assert_eq!(
            extract_tool_content(&json!({"content": [{"type": "text", "text": "{\"a\": 1}"}]})),
            json!({"a": 1}),
            "JSON text is decoded"
        );
        assert_eq!(
            extract_tool_content(&json!({"content": [{"type": "image", "data": "aGk="}]})),
            json!("aGk=")
        );
        assert_eq!(
            extract_tool_content(&json!({"content": [
                {"type": "text", "text": "a"},
                {"type": "image", "data": "b"}
            ]})),
            json!(["a", "b"])
        );
        assert_eq!(extract_tool_content(&json!({"content": []})), Value::Null);
    }

    #[test]
    fn error_text_joins_the_blocks() {
        assert_eq!(
            extract_text_content(&json!({"isError": true, "content": [
                {"type": "text", "text": "bad"},
                {"type": "text", "text": "input"}
            ]})),
            "bad input"
        );
    }

    #[test]
    fn schemas_are_always_objects() {
        assert_eq!(
            provider_schema(&json!({})),
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            provider_schema(&json!({"properties": {"a": {"type": "string"}}})),
            json!({"type": "object", "properties": {"a": {"type": "string"}}})
        );
    }

    #[test]
    fn rendering_keeps_text_and_serialises_the_rest() {
        assert_eq!(render(&json!("hi")), "hi");
        assert_eq!(render(&json!({"a": 1})), "{\"a\":1}");
        assert_eq!(render(&Value::Null), "null");
    }
}
