//! `SessionManager` — unified session lifecycle management.
//!
//! Coordinates two stores:
//!
//! - **`SessionStore`** — session metadata (`AgentSession`): task, model, cost, status
//! - **`SqliteConversationStore`** — conversation history (`Vec<Message>`) for agent memory
//!
//! The same SQLite file stores both via two schemas, enabling atomic backup,
//! easy inspection, and consistent cleanup.
//!
//! # Convention
//!
//! The **conversation key** for a session is the session's string ID. This
//! avoids a separate mapping table and makes debugging easy.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::transport::Message;
use agtrs_store::{ListOptions, PersistentConversationStore, SqliteConversationStore};
use tracing::instrument;

use xaft_runtime::session::{AgentSession, SessionId, SessionStatus};
use xaft_runtime::session_store::SessionStore;

use crate::error::SessionError;
use crate::store::SqliteSessionStore;

// ── SessionManager ────────────────────────────────────────────────────────────

/// Unified session manager combining metadata + conversation history.
///
/// # Thread safety
///
/// `SessionManager` is `Clone + Send + Sync`. All internal state is
/// held in `Arc<…>` so clones share the same underlying stores.
#[derive(Clone)]
pub struct SessionManager {
    session_store: Arc<dyn SessionStore>,
    conversation_store: Arc<SqliteConversationStore>,
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager").finish()
    }
}

// ── SessionWithHistory ────────────────────────────────────────────────────────

/// A loaded session with its conversation history.
#[derive(Debug, Clone)]
pub struct SessionWithHistory {
    /// Session metadata.
    pub session: AgentSession,
    /// Full conversation history (in ordinal order).
    pub messages: Vec<Message>,
    /// Conversation key used to look up / save history.
    pub conversation_key: String,
}

// ── Construction ──────────────────────────────────────────────────────────────

impl SessionManager {
    /// Open (or create) a `SessionManager` backed by two SQLite databases in
    /// `data_dir`:
    ///
    /// - `data_dir/sessions.db` — session metadata
    /// - `data_dir/conversations.db` — conversation history
    pub async fn new(data_dir: &Path) -> Result<Self, SessionError> {
        tokio::fs::create_dir_all(data_dir).await?;
        let sessions_db = data_dir.join("sessions.db");
        let conversations_db = data_dir.join("conversations.db");

        let session_store = Arc::new(SqliteSessionStore::open(&sessions_db).await?);
        let conversation_store = Arc::new(
            SqliteConversationStore::open(&format!("sqlite:{}", conversations_db.display()))
                .await?,
        );

        Ok(Self {
            session_store,
            conversation_store,
        })
    }

    /// Create an in-memory `SessionManager` (for tests — no persistence).
    pub async fn in_memory() -> Result<Self, SessionError> {
        let session_store = Arc::new(SqliteSessionStore::in_memory().await?);
        let conversation_store = Arc::new(SqliteConversationStore::in_memory().await?);
        Ok(Self {
            session_store,
            conversation_store,
        })
    }

    /// Build from pre-constructed stores (for dependency injection / tests).
    pub fn from_stores(
        session_store: Arc<dyn SessionStore>,
        conversation_store: Arc<SqliteConversationStore>,
    ) -> Self {
        Self {
            session_store,
            conversation_store,
        }
    }

    /// Return the underlying conversation store as a `ConversationStore` trait object.
    ///
    /// Pass this to agents / orchestrators so they write into the durable SQLite
    /// store instead of `InMemoryConversationStore`.
    pub fn conversation_store(&self) -> Arc<dyn ConversationStore> {
        Arc::clone(&self.conversation_store) as Arc<dyn ConversationStore>
    }

    /// Return the underlying session store.
    pub fn session_store(&self) -> Arc<dyn SessionStore> {
        Arc::clone(&self.session_store)
    }
}

// ── Session lifecycle ─────────────────────────────────────────────────────────

impl SessionManager {
    /// Save a session (metadata only).
    ///
    /// Call on first creation and after every status/stats update.
    #[instrument(name = "session_manager_save", skip(self, session), fields(id = %session.id))]
    pub async fn save(&self, session: &AgentSession) -> Result<(), SessionError> {
        self.session_store.save(session).await?;
        Ok(())
    }

    /// Load session metadata by ID.
    #[instrument(name = "session_manager_load", skip(self), fields(id = %id))]
    pub async fn load(&self, id: &SessionId) -> Result<Option<AgentSession>, SessionError> {
        Ok(self.session_store.load(id).await?)
    }

    /// Load session metadata + conversation history together.
    ///
    /// Returns `None` if the session does not exist. History may be empty for
    /// a brand-new session.
    #[instrument(name = "session_manager_load_with_history", skip(self), fields(id = %id))]
    pub async fn load_with_history(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionWithHistory>, SessionError> {
        let session = match self.session_store.load(id).await? {
            Some(s) => s,
            None => return Ok(None),
        };

        let conversation_key = conversation_key_for(id);
        let messages = self
            .conversation_store
            .load(&conversation_key)
            .await
            .unwrap_or_default();

        tracing::debug!(
            session_id = %id,
            message_count = messages.len(),
            "loaded session with history"
        );

        Ok(Some(SessionWithHistory {
            session,
            messages,
            conversation_key,
        }))
    }

    /// Save both session metadata and conversation history atomically.
    ///
    /// The conversation is identified by the session's own ID.
    #[instrument(name = "session_manager_save_with_history", skip(self, session, messages),
                 fields(id = %session.id, messages = messages.len()))]
    pub async fn save_with_history(
        &self,
        session: &AgentSession,
        messages: &[Message],
    ) -> Result<(), SessionError> {
        // Save metadata
        self.session_store.save(session).await?;

        // Save conversation history (replace-all semantics via ConversationStore::save)
        let key = conversation_key_for(&session.id);
        self.conversation_store.save(&key, messages).await?;

        tracing::debug!(
            session_id = %session.id,
            message_count = messages.len(),
            "saved session with history"
        );
        Ok(())
    }

    /// Append a single message to the conversation without rewriting the whole
    /// history. More efficient for long-running sessions.
    #[instrument(name = "session_manager_append", skip(self, message),
                 fields(id = %session_id))]
    pub async fn append_message(
        &self,
        session_id: &SessionId,
        message: &Message,
    ) -> Result<(), SessionError> {
        let key = conversation_key_for(session_id);
        let id = agtrs_store::ConversationId::from_string(&key);
        self.conversation_store.append_message(&id, message).await?;
        Ok(())
    }

    /// List sessions, optionally filtered by working directory.
    #[instrument(name = "session_manager_list", skip(self))]
    pub async fn list(
        &self,
        working_dir: Option<&Path>,
    ) -> Result<Vec<AgentSession>, SessionError> {
        Ok(self.session_store.list(working_dir).await?)
    }

    /// Delete a session and ALL its conversation history (idempotent).
    #[instrument(name = "session_manager_delete", skip(self), fields(id = %id))]
    pub async fn delete(&self, id: &SessionId) -> Result<(), SessionError> {
        // Delete conversation history first (FK-safe order)
        let key = conversation_key_for(id);
        let conv_id = agtrs_store::ConversationId::from_string(&key);
        let _ = self.conversation_store.delete_conversation(&conv_id).await; // idempotent

        // Delete session metadata
        self.session_store.delete(id).await?;

        tracing::info!(session_id = %id, "deleted session and conversation history");
        Ok(())
    }

    /// Validate that a session exists and is resumable.
    ///
    /// Returns the `SessionWithHistory` if the session is `Active`, `Suspended`,
    /// or `Completed`.  Only `Failed` and `Cancelled` sessions are rejected.
    pub async fn validate_resumable(
        &self,
        id: &SessionId,
    ) -> Result<SessionWithHistory, SessionError> {
        match self.load_with_history(id).await? {
            None => Err(SessionError::NotFound(id.as_str().to_string())),
            Some(swh) => match &swh.session.status {
                SessionStatus::Active
                | SessionStatus::Suspended
                | SessionStatus::Completed { .. } => Ok(swh),
                SessionStatus::Failed { error } => Err(SessionError::NotResumable {
                    id: id.as_str().to_string(),
                    status: format!("failed: {}", error),
                }),
                SessionStatus::Cancelled => Err(SessionError::NotResumable {
                    id: id.as_str().to_string(),
                    status: "cancelled".to_string(),
                }),
            },
        }
    }

    /// Count the total number of stored sessions.
    pub async fn count(&self) -> Result<usize, SessionError> {
        let all = self.session_store.list(None).await?;
        Ok(all.len())
    }

    /// Count the number of messages in a session's conversation.
    pub async fn message_count(&self, id: &SessionId) -> Result<usize, SessionError> {
        let key = conversation_key_for(id);
        let msgs = self.conversation_store.load(&key).await.unwrap_or_default();
        Ok(msgs.len())
    }

    /// Purge sessions older than `max_age` (hard-delete metadata + history).
    ///
    /// Returns the number of sessions deleted.
    pub async fn purge_old_sessions(
        &self,
        max_age: std::time::Duration,
    ) -> Result<usize, SessionError> {
        let all = self.session_store.list(None).await?;
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let mut deleted = 0;
        for session in all {
            if session.updated_at < cutoff {
                self.delete(&session.id).await?;
                deleted += 1;
            }
        }
        if deleted > 0 {
            tracing::info!(deleted, "purged old sessions");
        }
        Ok(deleted)
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Derive the conversation store key from a `SessionId`.
///
/// We use the session ID directly as the conversation key — this keeps the
/// mapping trivial and avoids a separate lookup table.
pub fn conversation_key_for(id: &SessionId) -> String {
    id.as_str().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::transport::{Message, Role};
    use std::path::PathBuf;

    fn make_session(task: &str) -> AgentSession {
        AgentSession::new(
            task,
            PathBuf::from("/work"),
            "default".into(),
            "claude".into(),
        )
    }

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant_msg(text: &str) -> Message {
        Message::assistant(text)
    }

    #[tokio::test]
    async fn create_and_load_session() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("fix the bug");
        mgr.save(&s).await.unwrap();
        let loaded = mgr.load(&s.id).await.unwrap().unwrap();
        assert_eq!(loaded.task, "fix the bug");
    }

    #[tokio::test]
    async fn load_missing_session_returns_none() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let id = SessionId::new();
        assert!(mgr.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn load_with_history_returns_messages() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("write tests");
        let messages = vec![
            user_msg("Write tests"),
            assistant_msg("Sure, here are the tests"),
        ];
        mgr.save_with_history(&s, &messages).await.unwrap();

        let swh = mgr.load_with_history(&s.id).await.unwrap().unwrap();
        assert_eq!(swh.messages.len(), 2);
        assert_eq!(swh.session.task, "write tests");
    }

    #[tokio::test]
    async fn load_with_history_empty_for_new_session() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("new session");
        mgr.save(&s).await.unwrap();

        let swh = mgr.load_with_history(&s.id).await.unwrap().unwrap();
        assert!(swh.messages.is_empty());
    }

    #[tokio::test]
    async fn save_with_history_replaces_messages() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("task");
        mgr.save_with_history(&s, &[user_msg("hello")])
            .await
            .unwrap();
        mgr.save_with_history(&s, &[user_msg("world"), assistant_msg("!")])
            .await
            .unwrap();

        let swh = mgr.load_with_history(&s.id).await.unwrap().unwrap();
        assert_eq!(swh.messages.len(), 2);
    }

    #[tokio::test]
    async fn delete_removes_session_and_history() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("delete me");
        mgr.save_with_history(&s, &[user_msg("hi")]).await.unwrap();

        mgr.delete(&s.id).await.unwrap();

        assert!(mgr.load(&s.id).await.unwrap().is_none());
        // After delete, load_with_history returns None (session gone)
        assert!(mgr.load_with_history(&s.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn validate_resumable_active_session() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("active task");
        mgr.save(&s).await.unwrap();

        let swh = mgr.validate_resumable(&s.id).await.unwrap();
        assert_eq!(swh.session.id, s.id);
    }

    #[tokio::test]
    async fn validate_resumable_completed_session_succeeds() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let mut s = make_session("done");
        s.status = SessionStatus::Completed {
            summary: "done".into(),
        };
        mgr.save(&s).await.unwrap();

        let swh = mgr.validate_resumable(&s.id).await.unwrap();
        assert_eq!(swh.session.id, s.id);
    }

    #[tokio::test]
    async fn validate_resumable_missing_session_fails() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let id = SessionId::new();
        let err = mgr.validate_resumable(&id).await.unwrap_err();
        assert!(matches!(err, SessionError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_sessions_by_dir() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let mut s1 = make_session("t1");
        s1.workspace_root = PathBuf::from("/proj/a");
        let mut s2 = make_session("t2");
        s2.workspace_root = PathBuf::from("/proj/b");
        mgr.save(&s1).await.unwrap();
        mgr.save(&s2).await.unwrap();

        let all = mgr.list(None).await.unwrap();
        assert_eq!(all.len(), 2);
        let filtered = mgr.list(Some(Path::new("/proj/a"))).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].task, "t1");
    }

    #[tokio::test]
    async fn count_sessions() {
        let mgr = SessionManager::in_memory().await.unwrap();
        assert_eq!(mgr.count().await.unwrap(), 0);
        mgr.save(&make_session("a")).await.unwrap();
        mgr.save(&make_session("b")).await.unwrap();
        assert_eq!(mgr.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn message_count() {
        let mgr = SessionManager::in_memory().await.unwrap();
        let s = make_session("task");
        let msgs = vec![user_msg("hi"), assistant_msg("hello"), user_msg("bye")];
        mgr.save_with_history(&s, &msgs).await.unwrap();
        assert_eq!(mgr.message_count(&s.id).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn conversation_key_is_session_id() {
        let id = SessionId::new();
        assert_eq!(conversation_key_for(&id), id.as_str());
    }

    #[tokio::test]
    async fn purge_old_sessions() {
        let mgr = SessionManager::in_memory().await.unwrap();
        // Create a session and manually age it
        let mut s = make_session("old");
        s.updated_at = chrono::Utc::now() - chrono::Duration::hours(25);
        mgr.save(&s).await.unwrap();
        mgr.save(&make_session("new")).await.unwrap();

        let deleted = mgr
            .purge_old_sessions(std::time::Duration::from_secs(3600 * 24))
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(mgr.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn concurrent_session_operations() {
        let mgr = Arc::new(SessionManager::in_memory().await.unwrap());
        let mut handles = Vec::new();
        for i in 0..20 {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move {
                let s = make_session(&format!("task {i}"));
                m.save_with_history(&s, &[user_msg(&format!("msg {i}"))])
                    .await
                    .unwrap();
                let swh = m.load_with_history(&s.id).await.unwrap().unwrap();
                assert_eq!(swh.messages.len(), 1);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(mgr.count().await.unwrap(), 20);
    }
}
