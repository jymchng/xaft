//! TUI layout state persistence across sessions.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::types::TuiLayoutState;

/// Manages persistence of TUI layout state for a session.
pub struct TuiLayoutPersistence {
    path: PathBuf,
    state: TuiLayoutState,
    dirty: bool,
}

impl TuiLayoutPersistence {
    /// Load the layout state for `session_id`, or use defaults if not found.
    pub fn load_or_default(data_dir: &Path, session_id: &str) -> Self {
        let path = data_dir
            .join("sessions")
            .join(session_id)
            .join("tui-layout.toml");

        let state = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
                    warn!("Failed to deserialize TUI layout from {}: {}", path.display(), e);
                    TuiLayoutState::default()
                }),
                Err(e) => {
                    warn!("Failed to read TUI layout from {}: {}", path.display(), e);
                    TuiLayoutState::default()
                }
            }
        } else {
            TuiLayoutState::default()
        };

        Self {
            path,
            state,
            dirty: false,
        }
    }

    /// Return a reference to the current layout state.
    pub fn state(&self) -> &TuiLayoutState {
        &self.state
    }

    /// Mutate the layout state. Marks as dirty (needs save).
    pub fn update<F>(&mut self, f: F)
    where
        F: FnOnce(&mut TuiLayoutState),
    {
        f(&mut self.state);
        self.dirty = true;
    }

    /// Write layout state to disk if dirty. No-op if clean.
    pub fn persist(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }

        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(&self.state)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // Atomic write: write to temp file then rename
        let tmp_path = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &self.path)?;

        self.dirty = false;
        debug!("TUI layout persisted to {}", self.path.display());
        Ok(())
    }

    /// Return `true` if there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// Spawn a background task that debounce-saves layout state changes.
///
/// `debounce` controls the minimum interval between saves.
/// The task exits when the sender is dropped.
pub fn spawn_layout_saver(
    persistence: std::sync::Arc<tokio::sync::Mutex<TuiLayoutPersistence>>,
    mut rx: watch::Receiver<TuiLayoutState>,
    debounce: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_save = Instant::now() - debounce; // allow immediate first save

        loop {
            match rx.changed().await {
                Err(_) => break, // sender dropped
                Ok(()) => {}
            }

            let elapsed = last_save.elapsed();
            if elapsed < debounce {
                let wait = debounce - elapsed;
                tokio::time::sleep(wait).await;
                // Drain any additional changes that arrived during sleep
                while rx.has_changed().unwrap_or(false) {
                    rx.borrow_and_update();
                }
            }

            let new_state = rx.borrow().clone();
            let mut p = persistence.lock().await;
            p.update(|s| *s = new_state);
            if let Err(e) = p.persist() {
                warn!("Failed to auto-save TUI layout: {}", e);
            }
            last_save = Instant::now();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_default_uses_default_when_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = TuiLayoutPersistence::load_or_default(tmp.path(), "test-session");
        assert!(!p.is_dirty());
    }

    #[test]
    fn persist_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = TuiLayoutPersistence::load_or_default(tmp.path(), "test-session");
        p.update(|s| s.conversation_width = 70);
        assert!(p.is_dirty());
        p.persist().unwrap();
        assert!(!p.is_dirty());

        // Reload and verify
        let p2 = TuiLayoutPersistence::load_or_default(tmp.path(), "test-session");
        assert_eq!(p2.state().conversation_width, 70);
    }

    #[test]
    fn persist_no_op_when_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = TuiLayoutPersistence::load_or_default(tmp.path(), "test-session");
        p.persist().unwrap(); // should not create file
        let expected_path = tmp.path().join("sessions/test-session/tui-layout.toml");
        assert!(!expected_path.exists());
    }

    #[tokio::test]
    async fn layout_saver_task_saves_on_change() {
        let tmp = tempfile::tempdir().unwrap();
        let persistence = std::sync::Arc::new(tokio::sync::Mutex::new(
            TuiLayoutPersistence::load_or_default(tmp.path(), "saver-test"),
        ));

        let initial = TuiLayoutState::default();
        let (tx, rx) = watch::channel(initial);

        let saver = spawn_layout_saver(
            std::sync::Arc::clone(&persistence),
            rx,
            Duration::from_millis(10),
        );

        let mut new_state = TuiLayoutState::default();
        new_state.conversation_width = 75;
        tx.send(new_state).unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(tx);
        let _ = saver.await;

        let layout_path = tmp
            .path()
            .join("sessions/saver-test/tui-layout.toml");
        assert!(layout_path.exists(), "layout file should have been saved");
    }
}
