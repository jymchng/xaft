//! Session persistence — in-memory (tests) and filesystem (production).

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;

use crate::error::RuntimeError;
use crate::session::{AgentSession, SessionId};

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Persists `AgentSession` records across process restarts.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Save (create or update) a session.
    async fn save(&self, session: &AgentSession) -> Result<(), RuntimeError>;

    /// Load a session by ID. Returns `None` if not found.
    async fn load(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError>;

    /// List all sessions, optionally filtered by working directory.
    async fn list(&self, working_dir: Option<&Path>) -> Result<Vec<AgentSession>, RuntimeError>;

    /// Delete a session.
    async fn delete(&self, id: &SessionId) -> Result<(), RuntimeError>;
}

// ── InMemorySessionStore ──────────────────────────────────────────────────────

/// Thread-safe in-memory session store.
///
/// Sessions are lost when the process exits. Suitable for tests and ephemeral
/// runs.
#[derive(Debug, Default)]
pub struct InMemorySessionStore {
    sessions: RwLock<HashMap<String, AgentSession>>,
}

impl InMemorySessionStore {
    /// Create an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the number of stored sessions.
    pub async fn len(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// True if no sessions are stored.
    pub async fn is_empty(&self) -> bool {
        self.sessions.read().await.is_empty()
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn save(&self, session: &AgentSession) -> Result<(), RuntimeError> {
        self.sessions
            .write()
            .await
            .insert(session.id.to_string(), session.clone());
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
        Ok(self.sessions.read().await.get(id.as_str()).cloned())
    }

    async fn list(&self, working_dir: Option<&Path>) -> Result<Vec<AgentSession>, RuntimeError> {
        let guard = self.sessions.read().await;
        let mut sessions: Vec<AgentSession> = guard
            .values()
            .filter(|s| working_dir.map(|d| s.workspace_root == d).unwrap_or(true))
            .cloned()
            .collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    async fn delete(&self, id: &SessionId) -> Result<(), RuntimeError> {
        self.sessions.write().await.remove(id.as_str());
        Ok(())
    }
}

// ── FsSessionStore ────────────────────────────────────────────────────────────

/// Filesystem-backed session store.
///
/// Persists sessions as JSON files in `data_dir/sessions/{id}.json`.
/// Suitable for production use.
pub struct FsSessionStore {
    dir: PathBuf,
}

impl FsSessionStore {
    /// Create (or open) a store rooted at `data_dir/sessions/`.
    pub async fn new(data_dir: &Path) -> Result<Self, RuntimeError> {
        let dir = data_dir.join("sessions");
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self { dir })
    }

    fn path_for(&self, id: &SessionId) -> PathBuf {
        self.dir.join(format!("{}.json", id.as_str()))
    }
}

#[async_trait]
impl SessionStore for FsSessionStore {
    async fn save(&self, session: &AgentSession) -> Result<(), RuntimeError> {
        let path = self.path_for(&session.id);
        let json = serde_json::to_string_pretty(session)
            .map_err(|e| RuntimeError::Workspace(format!("session serialize failed: {e}")))?;
        tokio::fs::write(&path, json).await?;
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
        let path = self.path_for(id);
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => {
                let session = serde_json::from_str(&json).map_err(|e| {
                    RuntimeError::Workspace(format!("session deserialize failed: {e}"))
                })?;
                Ok(Some(session))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(RuntimeError::Io(e)),
        }
    }

    async fn list(&self, working_dir: Option<&Path>) -> Result<Vec<AgentSession>, RuntimeError> {
        let mut sessions = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.dir).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(sessions),
            Err(e) => return Err(RuntimeError::Io(e)),
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(json) = tokio::fs::read_to_string(&path).await {
                if let Ok(session) = serde_json::from_str::<AgentSession>(&json) {
                    if working_dir
                        .map(|d| session.workspace_root == d)
                        .unwrap_or(true)
                    {
                        sessions.push(session);
                    }
                }
            }
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    async fn delete(&self, id: &SessionId) -> Result<(), RuntimeError> {
        let path = self.path_for(id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(RuntimeError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(task: &str) -> AgentSession {
        AgentSession::new(
            task,
            PathBuf::from("/work"),
            "default".into(),
            "claude-3-5-sonnet".into(),
        )
    }

    #[tokio::test]
    async fn in_memory_save_load_roundtrip() {
        let store = InMemorySessionStore::new();
        let session = make_session("test task");
        let id = session.id.clone();
        store.save(&session).await.unwrap();
        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded.task, "test task");
    }

    #[tokio::test]
    async fn in_memory_load_missing_returns_none() {
        let store = InMemorySessionStore::new();
        let id = SessionId::new();
        let result = store.load(&id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn in_memory_list_by_working_dir() {
        let store = InMemorySessionStore::new();
        let mut s1 = make_session("task1");
        s1.workspace_root = PathBuf::from("/work/a");
        let mut s2 = make_session("task2");
        s2.workspace_root = PathBuf::from("/work/b");
        store.save(&s1).await.unwrap();
        store.save(&s2).await.unwrap();

        let all = store.list(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let filtered = store.list(Some(Path::new("/work/a"))).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].task, "task1");
    }

    #[tokio::test]
    async fn in_memory_delete() {
        let store = InMemorySessionStore::new();
        let session = make_session("deleteme");
        let id = session.id.clone();
        store.save(&session).await.unwrap();
        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
        assert_eq!(store.len().await, 0);
    }

    #[tokio::test]
    async fn fs_store_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FsSessionStore::new(tmp.path()).await.unwrap();
        let session = make_session("fs task");
        let id = session.id.clone();
        store.save(&session).await.unwrap();
        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded.task, "fs task");
    }

    #[tokio::test]
    async fn fs_store_load_missing_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FsSessionStore::new(tmp.path()).await.unwrap();
        let id = SessionId::new();
        assert!(store.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fs_store_delete() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FsSessionStore::new(tmp.path()).await.unwrap();
        let session = make_session("delete me");
        let id = session.id.clone();
        store.save(&session).await.unwrap();
        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
    }
}
