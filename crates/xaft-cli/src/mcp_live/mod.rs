//! A live MCP client for `xaft mcp connect`, `refresh` and `doctor`
//! (parity matrix rows 36, 40 and 41).
//!
//! agenthicc's three commands build a short-lived `McpSessionManager`
//! (`cli/commands/mcp.py::_live_cli_manager`) from the configured servers,
//! act on one or all of them, print JSON, and shut the manager down. This
//! module is that manager and everything under it:
//!
//! | module | agenthicc | role |
//! | --- | --- | --- |
//! | [`spec`] | `McpServerConfig` (connect-time half) | validation, credentials, redaction |
//! | [`transport`] | `lauren_mcp` transports | stdio, Streamable HTTP, legacy HTTP+SSE, WebSocket |
//! | [`client`] | the bridge's handshake and listings | `initialize`, `tools/list`, `prompts/list`, `resources/list` |
//! | [`manager`] | `McpSessionManager` | status lifecycle, catalog snapshots, `doctor`, network guard |
//!
//! Nothing here registers tools with an agent; the commands only connect,
//! inspect and disconnect. The pieces are self-contained (config types in,
//! JSON out) so a session-time MCP integration can reuse them.

pub mod client;
pub mod manager;
pub mod spec;
pub mod transport;

pub use manager::{CatalogSnapshot, McpSessionManager, NetworkGuard, ServerState, ServerStatus};
pub use spec::ServerSpec;
pub use transport::{McpError, Transport};
