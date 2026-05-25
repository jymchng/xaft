//! `SqliteSessionStore` — SQLite-backed `SessionStore`.
//!
//! Stores `AgentSession` metadata in a SQLite database co-located with the
//! conversation history database.  Schema is created/migrated on first open.
//!
//! # Schema
//!
//! ```sql
//! sessions(id PK, created_at, updated_at, task, workspace_root,
//!          git_branch, total_cost_usd, total_tokens, turn_count,
//!          status_json, agent_preset, model)
//! ```

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use tracing::instrument;

use xaft_runtime::RuntimeError;
use xaft_runtime::session::{AgentSession, SessionId, SessionStatus};
use xaft_runtime::session_store::SessionStore;

use crate::error::SessionError;

// ── SqliteSessionStore ────────────────────────────────────────────────────────

/// SQLite-backed session metadata store.
///
/// Thread-safe — the underlying `SqlitePool` is `Clone + Send + Sync`.
#[derive(Clone, Debug)]
pub struct SqliteSessionStore {
    pool: Arc<SqlitePool>,
}

impl SqliteSessionStore {
    /// Open (or create) the SQLite database at `path`, running migrations.
    pub async fn open(path: &Path) -> Result<Self, SessionError> {
        let url = format!("sqlite:{}", path.display());
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePool::connect_with(opts)
            .await
            .map_err(SessionError::Database)?;

        let store = Self {
            pool: Arc::new(pool),
        };
        store.migrate().await?;
        Ok(store)
    }

    /// Open an in-memory database (ephemeral — for tests).
    pub async fn in_memory() -> Result<Self, SessionError> {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .map_err(SessionError::Database)?;
        let store = Self {
            pool: Arc::new(pool),
        };
        store.migrate().await?;
        Ok(store)
    }

    /// Run schema migrations.
    async fn migrate(&self) -> Result<(), SessionError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id             TEXT    PRIMARY KEY,
                created_at     TEXT    NOT NULL,
                updated_at     TEXT    NOT NULL,
                task           TEXT    NOT NULL,
                workspace_root TEXT    NOT NULL,
                git_branch     TEXT,
                total_cost_usd REAL    NOT NULL DEFAULT 0.0,
                total_tokens   INTEGER NOT NULL DEFAULT 0,
                turn_count     INTEGER NOT NULL DEFAULT 0,
                status_json    TEXT    NOT NULL,
                agent_preset   TEXT    NOT NULL,
                model          TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_workspace
                ON sessions(workspace_root);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated
                ON sessions(updated_at DESC);
            "#,
        )
        .execute(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;
        Ok(())
    }
}

// ── SessionStore impl ─────────────────────────────────────────────────────────

#[async_trait]
impl SessionStore for SqliteSessionStore {
    #[instrument(name = "sqlite_session_save", skip_all, fields(session_id = %session.id))]
    async fn save(&self, session: &AgentSession) -> Result<(), RuntimeError> {
        let status_json = serde_json::to_string(&session.status)
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO sessions
                (id, created_at, updated_at, task, workspace_root,
                 git_branch, total_cost_usd, total_tokens, turn_count,
                 status_json, agent_preset, model)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                updated_at     = excluded.updated_at,
                git_branch     = excluded.git_branch,
                total_cost_usd = excluded.total_cost_usd,
                total_tokens   = excluded.total_tokens,
                turn_count     = excluded.turn_count,
                status_json    = excluded.status_json
            "#,
        )
        .bind(session.id.as_str())
        .bind(session.created_at.to_rfc3339())
        .bind(session.updated_at.to_rfc3339())
        .bind(&session.task)
        .bind(session.workspace_root.to_string_lossy().as_ref())
        .bind(session.git_branch.as_deref())
        .bind(session.total_cost_usd)
        .bind(session.total_tokens as i64)
        .bind(session.turn_count as i64)
        .bind(&status_json)
        .bind(&session.agent_preset)
        .bind(&session.model)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| RuntimeError::Workspace(e.to_string()))?;

        tracing::debug!(session_id = %session.id, "saved session to SQLite");
        Ok(())
    }

    #[instrument(name = "sqlite_session_load", skip_all, fields(session_id = %id))]
    async fn load(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
        let row = sqlx::query(
            r#"
            SELECT id, created_at, updated_at, task, workspace_root,
                   git_branch, total_cost_usd, total_tokens, turn_count,
                   status_json, agent_preset, model
            FROM sessions WHERE id = ?1
            "#,
        )
        .bind(id.as_str())
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| RuntimeError::Workspace(e.to_string()))?;

        match row {
            None => Ok(None),
            Some(r) => Ok(Some(row_to_session(r)?)),
        }
    }

    #[instrument(name = "sqlite_session_list", skip_all)]
    async fn list(&self, working_dir: Option<&Path>) -> Result<Vec<AgentSession>, RuntimeError> {
        let rows = match working_dir {
            Some(dir) => sqlx::query(
                r#"
                    SELECT id, created_at, updated_at, task, workspace_root,
                           git_branch, total_cost_usd, total_tokens, turn_count,
                           status_json, agent_preset, model
                    FROM sessions
                    WHERE workspace_root = ?1
                    ORDER BY updated_at DESC
                    "#,
            )
            .bind(dir.to_string_lossy().as_ref())
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
            None => sqlx::query(
                r#"
                    SELECT id, created_at, updated_at, task, workspace_root,
                           git_branch, total_cost_usd, total_tokens, turn_count,
                           status_json, agent_preset, model
                    FROM sessions
                    ORDER BY updated_at DESC
                    "#,
            )
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
        };

        rows.into_iter()
            .map(row_to_session)
            .collect::<Result<Vec<_>, _>>()
    }

    #[instrument(name = "sqlite_session_delete", skip_all, fields(session_id = %id))]
    async fn delete(&self, id: &SessionId) -> Result<(), RuntimeError> {
        sqlx::query("DELETE FROM sessions WHERE id = ?1")
            .bind(id.as_str())
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
        Ok(())
    }
}

// ── Row → AgentSession conversion ─────────────────────────────────────────────

fn row_to_session(row: sqlx::sqlite::SqliteRow) -> Result<AgentSession, RuntimeError> {
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;

    let created_at: DateTime<Utc> = {
        let s: String = row
            .try_get("created_at")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
        DateTime::parse_from_rfc3339(&s)
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?
            .with_timezone(&Utc)
    };
    let updated_at: DateTime<Utc> = {
        let s: String = row
            .try_get("updated_at")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
        DateTime::parse_from_rfc3339(&s)
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?
            .with_timezone(&Utc)
    };

    let status_json: String = row
        .try_get("status_json")
        .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
    let status: SessionStatus =
        serde_json::from_str(&status_json).map_err(|e| RuntimeError::Workspace(e.to_string()))?;

    let workspace_str: String = row
        .try_get("workspace_root")
        .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
    let id_str: String = row
        .try_get("id")
        .map_err(|e| RuntimeError::Workspace(e.to_string()))?;

    Ok(AgentSession {
        id: SessionId::from_string(id_str),
        created_at,
        updated_at,
        task: row
            .try_get("task")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
        workspace_root: PathBuf::from(workspace_str),
        git_branch: row
            .try_get("git_branch")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
        total_cost_usd: row
            .try_get("total_cost_usd")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
        total_tokens: {
            let v: i64 = row
                .try_get("total_tokens")
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
            v as u64
        },
        turn_count: {
            let v: i64 = row
                .try_get("turn_count")
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?;
            v as u32
        },
        status,
        agent_preset: row
            .try_get("agent_preset")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
        model: row
            .try_get("model")
            .map_err(|e| RuntimeError::Workspace(e.to_string()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use xaft_runtime::session::SessionStatus;

    fn make_session(task: &str) -> AgentSession {
        AgentSession::new(
            task,
            PathBuf::from("/work"),
            "default".into(),
            "claude".into(),
        )
    }

    #[tokio::test]
    async fn save_and_load_roundtrip() {
        let store = SqliteSessionStore::in_memory().await.unwrap();
        let s = make_session("test task");
        let id = s.id.clone();
        store.save(&s).await.unwrap();
        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded.task, "test task");
        assert_eq!(loaded.id, id);
    }

    #[tokio::test]
    async fn load_missing_returns_none() {
        let store = SqliteSessionStore::in_memory().await.unwrap();
        let id = SessionId::new();
        assert!(store.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_updates_existing() {
        let store = SqliteSessionStore::in_memory().await.unwrap();
        let mut s = make_session("task");
        let id = s.id.clone();
        store.save(&s).await.unwrap();

        s.total_tokens = 1234;
        s.turn_count = 5;
        s.total_cost_usd = 0.042;
        store.save(&s).await.unwrap();

        let loaded = store.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded.total_tokens, 1234);
        assert_eq!(loaded.turn_count, 5);
        assert!((loaded.total_cost_usd - 0.042).abs() < 1e-9);
    }

    #[tokio::test]
    async fn list_by_workspace() {
        let store = SqliteSessionStore::in_memory().await.unwrap();
        let mut s1 = make_session("t1");
        s1.workspace_root = PathBuf::from("/work/a");
        let mut s2 = make_session("t2");
        s2.workspace_root = PathBuf::from("/work/b");
        store.save(&s1).await.unwrap();
        store.save(&s2).await.unwrap();

        let all = store.list(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let filtered = store.list(Some(Path::new("/work/a"))).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].task, "t1");
    }

    #[tokio::test]
    async fn delete_session() {
        let store = SqliteSessionStore::in_memory().await.unwrap();
        let s = make_session("task");
        let id = s.id.clone();
        store.save(&s).await.unwrap();
        store.delete(&id).await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_and_load_completed_status() {
        let store = SqliteSessionStore::in_memory().await.unwrap();
        let mut s = make_session("task");
        s.status = SessionStatus::Completed {
            summary: "all done".into(),
        };
        let id = s.id.clone();
        store.save(&s).await.unwrap();
        let loaded = store.load(&id).await.unwrap().unwrap();
        match loaded.status {
            SessionStatus::Completed { summary } => assert_eq!(summary, "all done"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_saves_no_deadlock() {
        let store = Arc::new(SqliteSessionStore::in_memory().await.unwrap());
        let mut handles = Vec::new();
        for i in 0..10 {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                let session = make_session(&format!("task {i}"));
                s.save(&session).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let all = store.list(None).await.unwrap();
        assert_eq!(all.len(), 10);
    }
}
