//! Where the account's tokens live: `$XAFT_HOME/auth.json`, owner-only.
//!
//! agenthicc keeps tokens in the OS keyring and falls back to
//! `~/.agenthicc/tokens.json` (`chmod 600`). No keyring crate is in xaft's
//! dependency graph, and a keyring backend that needs D-Bus at start-up was
//! itself a source of agenthicc stalls (its AGX-02 note), so xaft always uses
//! the file, and makes the file path its contract: written atomically
//! (temporary file + rename) with mode `0600` from the moment it exists, in a
//! `0700` directory, and readable by nothing else.

use std::path::{Path, PathBuf};

use super::{AuthError, TokenBundle};

/// The token file's name inside the xaft home.
pub const TOKEN_FILE: &str = "auth.json";

/// `$XAFT_HOME`, else `~/.xaft` — the home `jobs`, `runs`, `trust` and
/// `worktrees` share.
pub fn xaft_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XAFT_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".xaft"))
}

/// The account's token store.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// A store at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store at `$XAFT_HOME/auth.json`.
    pub fn default_location() -> Result<Self, AuthError> {
        xaft_home()
            .map(|home| Self::new(home.join(TOKEN_FILE)))
            .ok_or_else(|| {
                AuthError::Store("cannot determine the home directory; set XAFT_HOME".into())
            })
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stored tokens, or `None` when there are none or the file is not a
    /// valid token document (agenthicc's `TokenStore.load`).
    pub fn load(&self) -> Option<TokenBundle> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str::<TokenBundle>(&text).ok()
    }

    /// Store `bundle`, replacing whatever was there.
    pub fn save(&self, bundle: &TokenBundle) -> Result<(), AuthError> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        create_private_dir(parent)?;
        let body = serde_json::to_vec_pretty(bundle)
            .map_err(|e| AuthError::Store(format!("cannot encode tokens: {e}")))?;
        let tmp = parent.join(format!(
            ".{TOKEN_FILE}.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let result = write_private(&tmp, &body).and_then(|()| {
            std::fs::rename(&tmp, &self.path).map_err(|e| {
                AuthError::Store(format!("cannot replace {}: {e}", self.path.display()))
            })
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// Delete the stored tokens. Absent tokens are not an error.
    pub fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::Store(format!(
                "cannot delete {}: {e}",
                self.path.display()
            ))),
        }
    }
}

fn create_private_dir(dir: &Path) -> Result<(), AuthError> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| AuthError::Store(format!("cannot create {}: {e}", dir.display())))
}

/// Write `body` to a new file that is `0600` from creation — never briefly
/// world-readable, as a write-then-chmod would leave it.
fn write_private(path: &Path, body: &[u8]) -> Result<(), AuthError> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| AuthError::Store(format!("cannot create {}: {e}", path.display())))?;
    file.write_all(body)
        .and_then(|()| file.sync_all())
        .map_err(|e| AuthError::Store(format!("cannot write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> TokenBundle {
        TokenBundle {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            expires_at: 1_900_000_000.0,
            plan: "pro".into(),
            email: "a@b.c".into(),
            user_id: "u1".into(),
        }
    }

    #[test]
    fn round_trips_and_clears() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = TokenStore::new(tmp.path().join("nested/home/auth.json"));
        assert!(store.load().is_none(), "nothing stored yet");
        store.save(&bundle()).unwrap();
        assert_eq!(store.load(), Some(bundle()));
        store.clear().unwrap();
        assert!(store.load().is_none());
        store.clear().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join("nested/home"))
            .unwrap()
            .collect();
        assert!(leftovers.is_empty(), "no temporary files survive a save");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_and_its_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = TokenStore::new(home.join("auth.json"));
        store.save(&bundle()).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(store.path()), 0o600);
        assert_eq!(mode(&home), 0o700);
        // A second save replaces the file and keeps the mode.
        store.save(&bundle()).unwrap();
        assert_eq!(mode(store.path()), 0o600);
    }

    #[test]
    fn a_corrupt_or_partial_document_reads_as_logged_out() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("auth.json");
        let store = TokenStore::new(&path);
        for body in [
            "not json",
            "[]",
            "{\"access_token\": \"a\"}",
            "{\"access_token\": 1, \"refresh_token\": \"r\", \"expires_at\": 1, \"plan\": \"p\", \"email\": \"e\", \"user_id\": \"u\"}",
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(store.load().is_none(), "{body}");
        }
    }
}
