//! The live MCP client behind `xaft mcp connect`, `refresh` and `doctor`
//! (parity matrix rows 36, 40 and 41).
//!
//! agenthicc's three commands build a short-lived `McpSessionManager`
//! (`cli/commands/mcp.py::_live_cli_manager`) from the configured servers,
//! act on one or all of them, print JSON, and shut the manager down. The
//! manager and everything under it now live in the `xaft-mcp` crate, because
//! a session uses the very same client: the runtime connects the configured
//! servers when a session starts and gives every agent turn their tools, and
//! the TUI's `/mcp` manages them. This module keeps the CLI's paths:
//!
//! | module | agenthicc | role |
//! | --- | --- | --- |
//! | [`spec`] | `McpServerConfig` (connect-time half) | validation, credentials, redaction |
//! | [`transport`] | `lauren_mcp` transports | stdio, Streamable HTTP, legacy HTTP+SSE, WebSocket |
//! | [`client`] | the bridge's handshake and listings | `initialize`, `tools/list`, `prompts/list`, `resources/list` |
//! | [`manager`] | `McpSessionManager` | status lifecycle, catalog snapshots, `doctor`, network guard |

pub use xaft_mcp::{client, manager, spec, transport};

pub use xaft_mcp::{CatalogSnapshot, McpSessionManager, NetworkGuard, ServerState, ServerStatus};
pub use xaft_mcp::{McpError, ServerSpec, Transport};
