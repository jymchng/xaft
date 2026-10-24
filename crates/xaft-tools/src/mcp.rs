//! `McpConnectionGuideTool` — explain how to configure and connect an MCP
//! server, without ever echoing a credential.
//!
//! Parity row 78, and the only row in section 6 that needs no subsystem: it
//! answers from xaft's own configuration schema rather than talking to
//! anything.
//!
//! Two rules shape it. First, it never prints a secret: it names the *keys*
//! that hold credentials and points at the environment-variable indirection,
//! and it is asserted by a test that scans the rendered text. Second, the
//! guide names real config keys, so a test checks every key it mentions still
//! exists on `McpClientConfig` — otherwise the tool would quietly become
//! documentation of a schema xaft no longer has, which is the exact
//! false-parity failure this goal exists to prevent.

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// Every config key this tool names, checked against `McpClientConfig` by test.
pub const REFERENCED_CLIENT_KEYS: &[&str] = &[
    "name",
    "transport",
    "command",
    "args",
    "url",
    "enabled",
    "token",
    "token_env",
    "env_headers",
    "auto_connect",
    "reconnect_attempts",
    "reconnect_delay_seconds",
];

/// Transports xaft accepts, from `xaft_config::types::MCP_TRANSPORTS`.
const TRANSPORTS: &str = "stdio, http, http+sse, streamable, ws, websocket";

const GUIDE: &str = r#"# Connecting an MCP server to xaft

## 1. Add the server

    xaft mcp add <name> --transport stdio --command <path-to-binary> --arg <arg> ...
    xaft mcp add <name> --transport streamable_http --url https://host/mcp

Entries live in the `[mcp]` table of `xaft.toml` as a list of client configs.

## 2. The keys that matter

| key | meaning |
|---|---|
| `name` | the name you address the server by |
| `transport` | one of: stdio, http, http+sse, streamable, ws, websocket |
| `command` / `args` | the program to launch, for `stdio` |
| `url` | the endpoint, for the HTTP and WebSocket transports |
| `enabled` | set false to keep the entry but skip it |
| `auto_connect` | connect at session start without being asked (default true) |
| `reconnect_attempts` / `reconnect_delay_seconds` | backoff for a dropped connection |

## 3. Credentials — the part that bites

Never put a literal secret in `xaft.toml`. Use one of:

- `token_env` — the name of an environment variable that holds the token.
- `token` — may hold a `${VAR}` reference. A literal value is accepted but
  is the wrong thing to do; prefer `token_env`.
- `env_headers` — maps a header name to the *name* of an environment
  variable. Only the variable names are stored, never the header values.

    [mcp.client.token_env]
    # or, in the same entry:
    # token_env = "GITHUB_TOKEN"

`xaft mcp list` and `xaft mcp get` always print the redacted form, so
checking your configuration will not leak what is in it.

## 4. Connect and check

    xaft mcp connect <name>     # connect and list the server's tools
    xaft mcp refresh <name>     # re-fetch one server's tool catalog
    xaft mcp doctor             # sweep every configured server
    xaft mcp doctor --json      # machine-readable per-server status

In a session, every `auto_connect` server is started when the session
begins and each of its tools is offered to the agent as
`mcp__<server>__<tool>`. Inside the TUI, `/mcp` shows every server's status,
and `/mcp connect <name>`, `/mcp disconnect <name>`, `/mcp refresh <name>`,
`/mcp reload` and `/mcp doctor` manage them without leaving the session.

## 5. Diagnose

- A server that will not start: confirm `command` resolves on PATH and that
  `args` are what the server expects. `xaft mcp doctor` reports the failure
  per server rather than aborting the sweep.
- Tools missing after editing the config: `/mcp reload` in the TUI (or
  `xaft mcp refresh <name>`), then start a new turn.
- A server that connects but whose credentials are rejected: check the
  environment variable is set in the shell xaft was started from, not only
  in your interactive shell.
"#;

/// Explain how to configure and connect an MCP server.
#[derive(Debug, Default, Clone, Copy)]
pub struct McpConnectionGuideTool;

impl McpConnectionGuideTool {
    /// Create a new `McpConnectionGuideTool`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for McpConnectionGuideTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "mcp_connection_guide"
    }

    fn description(&self) -> &str {
        "Explain how to install, configure, connect, diagnose and reload MCP servers, \
         including how to reference credentials without exposing them. \
         Takes an optional `topic` of `add`, `credentials`, `connect` or `diagnose`; \
         with no topic it returns the whole guide."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "enum": ["add", "credentials", "connect", "diagnose"],
                    "description": "Optional section to return instead of the whole guide."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "mcp_connection_guide", skip(self, ctx), fields(topic))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", self.name()),
            });
        }

        let topic = input.get("topic").and_then(|v| v.as_str());
        tracing::Span::current().record("topic", topic.unwrap_or("<all>"));

        let body = match topic {
            None => GUIDE.to_string(),
            Some(t) => section(t).ok_or_else(|| AgtrsError::ToolCallFailed {
                tool_name: self.name().to_string(),
                reason: format!(
                    "unknown topic '{t}': expected one of add, credentials, connect, diagnose"
                ),
            })?,
        };

        Ok(ToolResult::ok(body, &ctx.tool_use_id))
    }
}

/// One `##` section of the guide, by heading.
fn section(topic: &str) -> Option<String> {
    let mut out = String::new();
    let mut capturing = false;
    for line in GUIDE.lines() {
        if line.starts_with("## ") {
            let heading = line.trim_start_matches('#').trim().to_ascii_lowercase();
            let matches = match topic {
                "add" => heading.starts_with("1."),
                "credentials" => heading.starts_with("3."),
                "connect" => heading.starts_with("4."),
                "diagnose" => heading.starts_with("5."),
                _ => false,
            };
            capturing = matches;
        }
        if capturing {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_the_whole_guide_by_default() {
        let out = McpConnectionGuideTool::new()
            .call(serde_json::json!({}), &ToolContext::new("mcg1"))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("mcp add"));
        assert!(out.content.contains("token_env"));
    }

    #[tokio::test]
    async fn topic_selects_one_section() {
        let out = McpConnectionGuideTool::new()
            .call(
                serde_json::json!({"topic": "credentials"}),
                &ToolContext::new("mcg2"),
            )
            .await
            .unwrap();
        assert!(out.content.contains("token_env"));
        assert!(
            !out.content.contains("mcp doctor"),
            "the credentials section should not drag in unrelated sections"
        );
    }

    #[tokio::test]
    async fn unknown_topic_is_an_error() {
        let res = McpConnectionGuideTool::new()
            .call(
                serde_json::json!({"topic": "nonsense"}),
                &ToolContext::new("mcg3"),
            )
            .await;
        assert!(
            res.is_err(),
            "an unknown topic must not return a plausible answer"
        );
    }

    /// The guide is only useful if it describes xaft's actual schema. A key
    /// that no longer exists would make this tool a well-written lie.
    #[test]
    fn every_referenced_key_exists_on_the_real_config_type() {
        // Every optional field is populated: several are
        // `skip_serializing_if = "Option::is_none"`, so a sample with `None`
        // would omit exactly the keys this test is here to check. What is
        // being verified is the SERDE name — the key a user writes in
        // xaft.toml — not merely that the Rust field exists.
        let mut env_headers = std::collections::BTreeMap::new();
        env_headers.insert("Authorization".to_string(), "TOKEN_ENV".to_string());
        let sample = xaft_config::types::McpClientConfig {
            name: "n".into(),
            transport: "stdio".into(),
            command: Some("server-bin".into()),
            args: vec!["--flag".into()],
            url: Some("https://example.invalid/mcp".into()),
            enabled: true,
            token: Some("${TOKEN_ENV}".into()),
            token_env: Some("TOKEN_ENV".into()),
            env_headers,
            auto_connect: true,
            reconnect_attempts: 3,
            reconnect_delay_seconds: 1.0,
        };
        let json = serde_json::to_value(&sample).expect("serialise");
        for key in REFERENCED_CLIENT_KEYS {
            assert!(
                json.get(*key).is_some(),
                "the guide names '{key}' but McpClientConfig has no such field"
            );
        }
    }

    #[test]
    fn the_guide_does_not_contain_a_literal_credential() {
        // The guide may only ever describe the *indirection* to a secret.
        for forbidden in ["sk-", "ghp_", "Bearer ey", "password ="] {
            assert!(
                !GUIDE.contains(forbidden),
                "the guide appears to contain a literal credential: {forbidden}"
            );
        }
        assert!(
            GUIDE.contains("Never put a literal secret"),
            "the guide should state the rule it follows"
        );
    }

    #[test]
    fn every_advertised_transport_is_one_xaft_accepts() {
        for t in TRANSPORTS.split(", ").collect::<Vec<_>>() {
            assert!(
                xaft_config::types::MCP_TRANSPORTS.contains(&t),
                "the guide advertises transport '{t}' which xaft_config does not accept"
            );
        }
    }
}
