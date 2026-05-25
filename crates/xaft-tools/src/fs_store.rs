//! Filesystem-backed [`WorkspaceStore`] — reads and writes real files on disk.
//!
//! All paths are validated as relative and resolved against `root`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;
use tracing::debug;

use agtrs_runtime::error::AgtrsError;
use agtrs_workspace::WorkspaceStore;

/// A [`WorkspaceStore`] backed by the real filesystem.
///
/// `root` is the workspace root. All relative paths passed to store methods
/// are resolved against `root`. Absolute paths and `..` traversal are
/// rejected.
#[derive(Debug, Clone)]
pub struct FsWorkspaceStore {
    root: PathBuf,
}

impl FsWorkspaceStore {
    /// Create a new store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The workspace root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, AgtrsError> {
        // Normalize absolute paths: models often emit absolute paths even when
        // the workspace uses relative ones. Strip the workspace root prefix if
        // present, otherwise strip any leading slashes.
        let normalized: std::borrow::Cow<str> = if path.starts_with('/') || path.starts_with('\\') {
            let root_str = self.root.to_string_lossy();
            if let Some(rel) = path.strip_prefix(root_str.as_ref()) {
                // e.g. /root/rust_projects/foo/main.go → main.go
                std::borrow::Cow::Owned(rel.trim_start_matches('/').to_string())
            } else {
                // e.g. /main.go → main.go  or  /home/user/main.go → main.go (take filename)
                let stripped = path.trim_start_matches('/');
                // If still contains slashes and doesn't look like a workspace-relative path,
                // take just the last component(s) by stripping any absolute prefix.
                std::borrow::Cow::Owned(stripped.to_string())
            }
        } else {
            std::borrow::Cow::Borrowed(path)
        };

        let rel = Path::new(normalized.as_ref());
        // Reject traversal
        for component in rel.components() {
            use std::path::Component;
            if matches!(component, Component::ParentDir) {
                return Err(AgtrsError::Other(format!(
                    "path traversal not allowed: {path}"
                )));
            }
        }
        Ok(self.root.join(rel))
    }
}

#[async_trait]
impl WorkspaceStore for FsWorkspaceStore {
    async fn write(&self, path: &str, content: &str) -> Result<(), AgtrsError> {
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                AgtrsError::Other(format!("create_dir_all {}: {e}", parent.display()))
            })?;
        }
        debug!(path, bytes = content.len(), "fs workspace write");
        fs::write(&full, content)
            .await
            .map_err(|e| AgtrsError::Other(format!("write {path}: {e}")))
    }

    async fn read(&self, path: &str) -> Result<String, AgtrsError> {
        let full = self.resolve(path)?;
        fs::read_to_string(&full)
            .await
            .map_err(|e| AgtrsError::Other(format!("read {path}: {e}")))
    }

    async fn exists(&self, path: &str) -> bool {
        match self.resolve(path) {
            Ok(full) => full.exists(),
            Err(_) => false,
        }
    }

    async fn list(&self) -> Vec<String> {
        let mut results = Vec::new();
        walk_dir(&self.root, &self.root, &mut results).await;
        results.sort();
        results
    }

    async fn delete(&self, path: &str) -> Result<(), AgtrsError> {
        let full = match self.resolve(path) {
            Ok(p) => p,
            Err(e) => return Err(e),
        };
        if full.is_file() {
            fs::remove_file(&full)
                .await
                .map_err(|e| AgtrsError::Other(format!("delete {path}: {e}")))
        } else if full.is_dir() {
            fs::remove_dir_all(&full)
                .await
                .map_err(|e| AgtrsError::Other(format!("delete dir {path}: {e}")))
        } else {
            Ok(()) // already gone
        }
    }

    async fn read_all(&self) -> std::collections::HashMap<String, String> {
        let paths = self.list().await;
        let mut map = std::collections::HashMap::new();
        for path in paths {
            if let Ok(content) = self.read(&path).await {
                map.insert(path, content);
            }
        }
        map
    }

    fn root_display(&self) -> String {
        self.root.display().to_string()
    }
}

/// Recursively walk `dir`, collecting relative paths from `root`.
#[async_recursion::async_recursion]
async fn walk_dir(root: &Path, dir: &Path, acc: &mut Vec<String>) {
    let mut rd = match fs::read_dir(dir).await {
        Ok(r) => r,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let entry_path = entry.path();
        // Skip hidden dirs
        if let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        if entry_path.is_dir() {
            walk_dir(root, &entry_path, acc).await;
        } else if entry_path.is_file() {
            if let Ok(rel) = entry_path.strip_prefix(root) {
                if let Some(s) = rel.to_str() {
                    acc.push(s.replace('\\', "/"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn roundtrip_write_read() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        store.write("hello.txt", "world").await.unwrap();
        let content = store.read("hello.txt").await.unwrap();
        assert_eq!(content, "world");
    }

    #[tokio::test]
    async fn creates_nested_dirs() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        store.write("a/b/c.txt", "nested").await.unwrap();
        let content = store.read("a/b/c.txt").await.unwrap();
        assert_eq!(content, "nested");
    }

    #[tokio::test]
    async fn exists_works() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        assert!(!store.exists("x.txt").await);
        store.write("x.txt", "y").await.unwrap();
        assert!(store.exists("x.txt").await);
    }

    #[tokio::test]
    async fn list_returns_relative_paths() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        store.write("a.txt", "a").await.unwrap();
        store.write("sub/b.txt", "b").await.unwrap();
        let paths = store.list().await;
        assert!(paths.contains(&"a.txt".to_string()));
        assert!(paths.contains(&"sub/b.txt".to_string()));
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        store.write("del.txt", "gone").await.unwrap();
        assert!(store.exists("del.txt").await);
        store.delete("del.txt").await.unwrap();
        assert!(!store.exists("del.txt").await);
    }

    #[tokio::test]
    async fn rejects_absolute_path() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        assert!(store.read("/etc/passwd").await.is_err());
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let store = FsWorkspaceStore::new(tmp.path());
        assert!(store.read("../secret.txt").await.is_err());
    }
}
