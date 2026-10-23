//! Outlook mail and calendar tools over Microsoft Graph (parity rows 51-59).
//!
//! agenthicc's `tools/outlook/agent_tools.py` registers nine `@tool()`
//! functions — `list_emails`, `read_email`, `send_email`, `reply_email`,
//! `search_emails`, `move_email`, `list_folders`, `calendar_events`,
//! `create_event` — over a backend chosen per call: Outlook COM
//! (`Win32OutlookBackend`, pywin32) on Windows, Microsoft Graph everywhere
//! else, with the token in `MSGRAPH_TOKEN`. Every call goes through
//! `_safe_call`, which turns a network error into
//! `{"ok": false, "error": "...", "recoverable": true}` so a flaky connection
//! does not end the agent's turn.
//!
//! xaft ports the nine tools with agenthicc's names, parameters, defaults and
//! result shapes over the Graph backend ([`GraphOutlookBackend`]), keeps the
//! recoverable-error envelope, and classifies them as `network` in
//! [`crate::capabilities`]. The four that change a mailbox or calendar —
//! `send_email`, `reply_email`, `move_email`, `create_event` — require
//! approval, the way agenthicc tags them `tool_network_write`.
//!
//! **Not ported: the Windows COM backend.** It drives a locally installed
//! Outlook through pywin32; a Rust equivalent needs a COM binding crate the
//! workspace does not carry, and Graph works on Windows too. On every
//! platform xaft uses Graph.

pub mod graph;
mod tools;

pub use graph::{GRAPH_BASE, GraphOutlookBackend, OutlookError, TOKEN_ENV, TokenSource};
pub use tools::{
    OUTLOOK_TOOL_NAMES, OutlookTool, OutlookVerb, outlook_tools, read_only_outlook_tools,
};
