//! `xaft-mcp` — xaft's live Model Context Protocol client.
//!
//! One crate, two consumers:
//!
//! * **Sessions.** `xaft-runtime` and the TUI build one
//!   [`McpSessionManager`] per session from the configured servers
//!   ([`entries_from_config`]), start the `auto_connect` ones (in the
//!   background in the TUI), and hand every published tool to each agent
//!   turn and workflow phase as `mcp__<server>__<tool>` ([`McpTool`]). `/mcp`
//!   reports and manages the same manager.
//! * **`xaft mcp connect|refresh|doctor`** build a short-lived manager over
//!   one config file, act, print JSON and shut it down.
//!
//! | module | agenthicc | role |
//! | --- | --- | --- |
//! | [`spec`] | `McpServerConfig` (connect-time half) | validation, credentials, redaction |
//! | [`transport`] | `lauren_mcp` transports | stdio, Streamable HTTP, legacy HTTP+SSE, WebSocket |
//! | [`client`] | the bridge's handshake, listings and `call_tool` | `initialize`, `tools/list`, `tools/call`, … |
//! | [`manager`] | `McpSessionManager` | lifecycle, reconnects, catalog snapshots, `doctor` |
//! | [`tool`] | `AgenthiccMcpTool` | the agent-facing tools and result extraction |
//! | [`entry`] | `ToolSettings.mcp_servers` | the configured servers |
//! | [`pyjson`] | `json.dumps` | byte-identical Python JSON for printed output and the catalog hash |
//! | [`testing`] | — | stand-in servers for tests (no network) |

pub mod client;
pub mod entry;
pub mod manager;
pub mod pyjson;
pub mod spec;
pub mod testing;
pub mod tool;
pub mod transport;

pub use entry::{McpEntry, entries_from_config};
pub use manager::{
    CatalogSnapshot, FailureListener, McpServerFailure, McpSessionManager, McpTimeouts,
    NetworkGuard, ServerState, ServerStatus,
};
pub use spec::{ServerSpec, shell_split};
pub use tool::{McpTool, McpToolBinding, provider_safe_tool_name};
pub use transport::{McpError, Transport};
