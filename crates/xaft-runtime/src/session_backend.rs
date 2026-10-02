//! `SessionBackend` — the history-reading half of session store access.
//!
//! # Why this trait exists separately from [`RuntimeDispatch`]
//!
//! Every `xaft session` subcommand needs the same durable store, but they need
//! different things from it:
//!
//! | subcommand | needs |
//! |---|---|
//! | `list` | the cwd-filtered session list |
//! | `show`, `cancel` | load / save one `AgentSession` by ID |
//! | `resume` | a full run restart |
//! | `export`, `inspect` | one `AgentSession` **plus its whole `Vec<Message>` history** |
//!
//! [`RuntimeDispatch`](crate::dispatch::RuntimeDispatch) covers the first three.
//! It does not cover the fourth, and it cannot be made to: the concrete store
//! (`xaft_session::SessionManager`) lives in `xaft-session`, which already
//! depends on **this** crate for `AgentSession` and `SessionId`. Adding the
//! reverse dependency would invert the crate graph.
//!
//! So the history-reading capability is declared here, as a trait, and
//! implemented on the other side of the boundary in `xaft-session`. The CLI
//! receives an `Arc<dyn SessionBackend>` — literally the same `SessionManager`
//! whose `session_store()` the runtime was given — so `export`/`inspect` read
//! the same SQLite file as `list`/`show`/`resume`/`cancel` by construction
//! rather than by two independent `core.data_dir` resolutions agreeing.
//!
//! # Deliberately narrow
//!
//! One method. `load_with_history` is a **global** lookup by ID: there is no
//! working-directory filter, because `xaft session inspect <id>` must find a
//! session started in any directory, exactly as `xaft session show <id>` does
//! (it resolves the exact ID through
//! [`RuntimeDispatch::load_session`](crate::dispatch::RuntimeDispatch::load_session)
//! before falling back to the cwd-scoped prefix match).

use agtrs_runtime::transport::Message;
use async_trait::async_trait;

use crate::error::RuntimeError;
use crate::session::{AgentSession, SessionId};

/// One session plus its full conversation history.
///
/// Returned by [`SessionBackend::load_with_history`]. Field-for-field the same
/// shape as `xaft_session::SessionWithHistory`; it is redeclared here rather
/// than re-used because `xaft-runtime` cannot name a type from `xaft-session`
/// without the dependency inversion this module exists to avoid.
#[derive(Debug, Clone)]
pub struct SessionHistory {
    /// The session's durable metadata.
    pub session: AgentSession,
    /// The full conversation, in ordinal order.
    ///
    /// Empty for a session that has not exchanged any messages yet — an empty
    /// history is not an error.
    pub messages: Vec<Message>,
    /// The key the history is stored under.
    pub conversation_key: String,
}

/// Read access to a durable session store that keeps message history.
///
/// Implemented by `xaft_session::SessionManager` and injected into
/// `xaft-cli` alongside
/// [`RuntimeDispatch`](crate::dispatch::RuntimeDispatch).
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Load one session and its conversation history by ID, across the whole
    /// store.
    ///
    /// Returns `Ok(None)` — not an error — when the ID is not in the store;
    /// the caller decides how to report "not found". A session that exists but
    /// has no messages yet returns `Ok(Some(..))` with an empty `messages`.
    async fn load_with_history(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionHistory>, RuntimeError>;
}
