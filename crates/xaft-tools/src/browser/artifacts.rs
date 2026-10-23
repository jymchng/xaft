//! Bounded screenshot persistence inside the workspace.
//!
//! Port of agenthicc's `tools/cloakbrowser/artifacts.py`. Screenshots are
//! written below `.xaft/browser-artifacts/<session>/` only — agenthicc's
//! `.agenthicc/browser-artifacts`, under xaft's project directory. Every path
//! component is either a validated opaque id or a fresh UUID, and the final
//! directory is re-checked to resolve inside the workspace, so neither a
//! hostile session id nor a planted symlink can move a write elsewhere.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The workspace-relative artifact root.
pub const ARTIFACT_ROOT: &str = ".xaft/browser-artifacts";

/// Whether `id` is safe as a single path component: agenthicc's
/// `^[A-Za-z0-9_-]{1,128}$`.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Metadata returned after a screenshot is stored (agenthicc `BrowserArtifact`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserArtifact {
    /// Opaque artifact id.
    pub artifact_id: String,
    /// Workspace-relative path, `/`-separated.
    pub relative_path: String,
    /// `image/png` or `image/jpeg`.
    pub mime_type: String,
    /// Bytes written.
    pub byte_count: usize,
}

impl BrowserArtifact {
    /// agenthicc `BrowserArtifact.to_dict`.
    pub fn to_json(&self) -> Value {
        json!({
            "artifact_id": self.artifact_id,
            "path": self.relative_path,
            "mime_type": self.mime_type,
            "byte_count": self.byte_count,
        })
    }
}

/// Why a screenshot could not be stored. Logged, never shown verbatim.
#[derive(Debug)]
pub enum ArtifactError {
    /// The session id is not a safe path component.
    UnsafeSessionId,
    /// The MIME type is not PNG or JPEG.
    UnsupportedMime,
    /// The directory resolved outside the workspace.
    EscapesWorkspace,
    /// A filesystem error.
    Io(std::io::Error),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeSessionId => {
                f.write_str("browser session id is not safe for artifact storage")
            }
            Self::UnsupportedMime => f.write_str("unsupported screenshot MIME type"),
            Self::EscapesWorkspace => {
                f.write_str("artifact directory resolves outside the workspace")
            }
            Self::Io(e) => write!(f, "artifact write failed: {e}"),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// Writes screenshots below [`ARTIFACT_ROOT`] (agenthicc `BrowserArtifactStore`).
#[derive(Debug, Clone)]
pub struct BrowserArtifactStore {
    workspace_root: PathBuf,
}

impl BrowserArtifactStore {
    /// A store rooted at `workspace_root`.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    /// Store one screenshot for `session_id`.
    pub fn write_screenshot(
        &self,
        session_id: &str,
        content: &[u8],
        mime_type: &str,
    ) -> Result<BrowserArtifact, ArtifactError> {
        if !is_safe_id(session_id) {
            return Err(ArtifactError::UnsafeSessionId);
        }
        let suffix = match mime_type {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => return Err(ArtifactError::UnsupportedMime),
        };
        let artifact_id = uuid::Uuid::new_v4().simple().to_string();
        let relative = format!("{ARTIFACT_ROOT}/{session_id}/{artifact_id}.{suffix}");
        // Check the shared root before creating anything below it, then the
        // session directory itself: a symlink at either level is refused
        // before a single directory is created outside the workspace.
        let root = self.workspace_root.join(ARTIFACT_ROOT);
        std::fs::create_dir_all(&root).map_err(ArtifactError::Io)?;
        ensure_inside(&self.workspace_root, &root)?;
        let dir = root.join(session_id);
        std::fs::create_dir_all(&dir).map_err(ArtifactError::Io)?;
        ensure_inside(&self.workspace_root, &dir)?;
        let path = dir.join(format!("{artifact_id}.{suffix}"));
        std::fs::write(&path, content).map_err(ArtifactError::Io)?;
        Ok(BrowserArtifact {
            artifact_id,
            relative_path: relative,
            mime_type: mime_type.to_string(),
            byte_count: content.len(),
        })
    }
}

/// Refuse a directory that canonicalises outside the canonical workspace.
fn ensure_inside(root: &Path, dir: &Path) -> Result<(), ArtifactError> {
    let root = root.canonicalize().map_err(ArtifactError::Io)?;
    let dir = dir.canonicalize().map_err(ArtifactError::Io)?;
    if dir.starts_with(&root) {
        Ok(())
    } else {
        Err(ArtifactError::EscapesWorkspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn screenshots_land_inside_the_workspace_artifact_root() {
        let tmp = TempDir::new().unwrap();
        let store = BrowserArtifactStore::new(tmp.path());
        let artifact = store
            .write_screenshot("session-1", b"png", "image/png")
            .unwrap();
        assert_eq!(artifact.byte_count, 3);
        assert!(
            artifact
                .relative_path
                .starts_with(".xaft/browser-artifacts/session-1/")
        );
        assert!(artifact.relative_path.ends_with(".png"));
        assert_eq!(
            std::fs::read(tmp.path().join(&artifact.relative_path)).unwrap(),
            b"png"
        );
        let jpeg = store
            .write_screenshot("session-1", b"jpg", "image/jpeg")
            .unwrap();
        assert!(jpeg.relative_path.ends_with(".jpg"));
    }

    #[test]
    fn unsafe_ids_and_mime_types_are_refused() {
        let tmp = TempDir::new().unwrap();
        let store = BrowserArtifactStore::new(tmp.path());
        for id in ["../escape", "", "a/b", &"x".repeat(129)] {
            assert!(matches!(
                store.write_screenshot(id, b"x", "image/png"),
                Err(ArtifactError::UnsafeSessionId)
            ));
        }
        assert!(matches!(
            store.write_screenshot("s", b"x", "image/gif"),
            Err(ArtifactError::UnsupportedMime)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_artifact_root_cannot_redirect_the_write() {
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".xaft")).unwrap();
        std::os::unix::fs::symlink(outside.path(), tmp.path().join(ARTIFACT_ROOT)).unwrap();
        let store = BrowserArtifactStore::new(tmp.path());
        assert!(matches!(
            store.write_screenshot("s", b"x", "image/png"),
            Err(ArtifactError::EscapesWorkspace)
        ));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
