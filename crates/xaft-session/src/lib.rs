//! `xaft-session` — durable SQLite-backed session and conversation persistence.
//!
//! Replaces the ephemeral `InMemoryConversationStore` with a persistent SQLite
//! store so agent conversation history survives process restarts, enabling
//! true session resume.
//!
//! # Components
//!
//! - [`SqliteSessionStore`] — stores `AgentSession` metadata in SQLite
//!   (replaces JSON-file `FsSessionStore` for production use)
//! - [`SessionManager`] — unified API coordinating both session metadata and
//!   conversation history
//!
//! # Quick start
//!
//! ```rust,no_run
//! use xaft_session::SessionManager;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mgr = SessionManager::new(Path::new("~/.xaft")).await?;
//!
//! // Wire into XaftRuntime
//! let conv_store = mgr.conversation_store(); // Arc<dyn ConversationStore>
//! let session_store = mgr.session_store();   // Arc<dyn SessionStore>
//! # Ok(())
//! # }
//! ```
//!
//! # Session resume flow
//!
//! ```text
//! SessionManager::validate_resumable(session_id)
//!   → load AgentSession metadata (SQLite sessions.db)
//!   → load Vec<Message> history (SQLite conversations.db)
//!   → inject into AgentContext via ConversationStore
//!   → agent resumes from where it left off
//! ```

#![warn(missing_docs)]

pub mod error;
pub mod manager;
pub mod store;

pub use error::SessionError;
pub use manager::{SessionManager, SessionWithHistory, conversation_key_for};
pub use store::SqliteSessionStore;

/// Re-exported so callers can name the element type of
/// `SessionWithHistory::messages` and of `SessionManager::save_with_history`.
///
/// That field is public, but `Message` lives in `agtrs_runtime::transport`.
/// Without this re-export a consumer of `xaft-session` cannot construct the
/// `Vec<Message>` the save API takes, so the store could only be written from
/// inside this workspace.
pub use agtrs_runtime::transport::Message;
