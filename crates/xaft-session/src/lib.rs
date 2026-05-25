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
