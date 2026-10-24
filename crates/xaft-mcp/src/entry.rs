//! Configured MCP servers, as the manager receives them.
//!
//! Two readers produce [`McpEntry`] lists. `xaft mcp list|get|connect|…` read
//! one config *file* leniently (`xaft_cli::mcp::read_mcp_servers`), so a
//! malformed stanza degrades to [`McpEntry::Invalid`] instead of hiding its
//! neighbours. A session reads the *effective* configuration the loader
//! already merged and validated ([`entries_from_config`]) — agenthicc's
//! `cfg.tools.mcp_servers` at session start.

use xaft_config::{McpClientConfig, XaftConfig};

/// The name a stanza without a `name` is reported under (agenthicc's
/// `raw.get("name", "<invalid>")`).
pub const INVALID_NAME: &str = "<invalid>";

/// One `[[mcp.client]]` / `[[tools.mcp_servers]]` entry.
///
/// agenthicc's `mcp list` degrades a stanza that fails to parse into
/// `{"name": ..., "status": "invalid"}` rather than aborting the whole
/// listing — one typo in one of five servers should not hide the other four.
#[derive(Debug, Clone)]
pub enum McpEntry {
    /// The stanza parsed into a full configuration.
    Valid(McpClientConfig),
    /// The stanza did not parse, but its `name` was recoverable.
    Invalid {
        /// The name from the raw table, or `"<invalid>"` if it had none.
        name: String,
    },
}

impl McpEntry {
    /// The server's name, however it was recovered.
    pub fn name(&self) -> &str {
        match self {
            Self::Valid(c) => &c.name,
            Self::Invalid { name } => name,
        }
    }

    /// The JSON a CLI command prints for this entry.
    ///
    /// Invalid entries get the same two keys agenthicc gives them, so a
    /// consumer parsing `--json` output handles one shape, not two.
    pub fn redacted(&self) -> serde_json::Value {
        match self {
            Self::Valid(c) => c.redacted(),
            Self::Invalid { name } => {
                let mut map = serde_json::Map::new();
                map.insert("name".into(), name.clone().into());
                map.insert("status".into(), "invalid".into());
                serde_json::Value::Object(map)
            }
        }
    }
}

/// Every MCP server the effective configuration declares: `[[mcp.client]]`
/// first, then agenthicc's `[[tools.mcp_servers]]` spelling.
///
/// The typed loader gives a stanza without a `name` the empty name (so one
/// partial stanza cannot reject the whole file); such a stanza is reported as
/// [`McpEntry::Invalid`] under `<invalid>`, exactly as `mcp list` reports it,
/// and is never started. A name declared twice reaches the manager twice and
/// is failed there as a duplicate rather than silently resolved.
pub fn entries_from_config(config: &XaftConfig) -> Vec<McpEntry> {
    config
        .mcp
        .client
        .iter()
        .chain(config.tools.mcp_servers.iter())
        .map(|server| {
            if server.name.is_empty() {
                McpEntry::Invalid {
                    name: INVALID_NAME.into(),
                }
            } else {
                McpEntry::Valid(server.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_spellings_are_read_and_a_nameless_stanza_is_invalid() {
        let config: XaftConfig = toml::from_str(
            r#"
[[mcp.client]]
name = "a"
command = "srv"

[[mcp.client]]
command = "nameless"

[[tools.mcp_servers]]
name = "b"
transport = "http"
url = "https://h/mcp"
"#,
        )
        .unwrap();
        let entries = entries_from_config(&config);
        let names: Vec<&str> = entries.iter().map(McpEntry::name).collect();
        assert_eq!(names, ["a", INVALID_NAME, "b"]);
        assert!(matches!(entries[1], McpEntry::Invalid { .. }));
        assert_eq!(entries[1].redacted()["status"], "invalid");
    }

    #[test]
    fn no_servers_is_an_empty_list() {
        assert!(entries_from_config(&XaftConfig::default()).is_empty());
    }
}
