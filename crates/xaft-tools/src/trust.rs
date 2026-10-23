//! Digest-bound trust for project extensions — the read side.
//!
//! Port of `agenthicc.plugins.trust.ExtensionTrust` (PRD-250 S-02). A
//! repository can ship files that xaft *executes*: `.xaft/cli.toml`, whose
//! `[[command]]` tables run programs, and `.xaft/tools/<name>.toml`, whose
//! custom tools do (see [`crate::custom_tools`]). Running one acts with the
//! user's permissions, so a loader may use an extension file only when it is
//! trusted:
//!
//! * a file under a **trusted root** is trusted by location — the user's own
//!   xaft home (`$XAFT_HOME`, default `~/.xaft`) and user config directory
//!   (`~/.config/xaft`). Nothing a repository ships lives there;
//! * any other file needs a **trust record** binding its resolved path to the
//!   SHA-256 of its current bytes. Editing the file invalidates the record.
//!
//! Records live in `$XAFT_HOME/trust.json`, never in the repository — a store
//! inside the checkout could forge its own trust.
//!
//! This module is what loaders need: reading the store and deciding. Writing
//! it (`xaft trust add|revoke`) stays with the CLI, which re-exports these
//! types from `xaft_cli::trust` and adds the write side there.
//!
//! # Wire format
//!
//! Identical to agenthicc's `trust.json`:
//!
//! ```json
//! {"version": 1, "records": [{"path": "/abs/x", "sha256": "…", "kind": "cli", "trusted_at": "…"}]}
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Version written into `trust.json`.
pub const STORE_VERSION: u32 = 1;

/// One trusted extension file, bound to the content that was reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// Resolved absolute path.
    pub path: String,
    /// Hex SHA-256 of the file when it was trusted.
    pub sha256: String,
    /// Extension kind, e.g. `cli` or `tool`.
    pub kind: String,
    /// RFC 3339 timestamp of the decision.
    pub trusted_at: String,
}

/// Why a loader may not use an extension file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrustedExtension {
    /// Resolved path.
    pub path: PathBuf,
    /// Extension kind.
    pub kind: String,
    /// `not trusted`, `changed since it was trusted`, or `unreadable: …`.
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    records: Vec<serde_json::Value>,
}

/// Decides whether an extension file may be used.
#[derive(Debug, Clone)]
pub struct ExtensionTrust {
    store_path: PathBuf,
    trusted_roots: Vec<PathBuf>,
}

/// `$XAFT_HOME`, else `~/.xaft` — the same home `jobs`, `runs` and
/// `worktrees` keep their stores in.
pub fn xaft_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XAFT_HOME") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|home| home.join(".xaft"))
}

/// The user's xaft config directory (`~/.config/xaft` on Linux).
pub fn user_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("xaft"))
}

impl ExtensionTrust {
    /// A policy over an explicit store and explicit trusted roots.
    pub fn new(store_path: impl Into<PathBuf>, trusted_roots: Vec<PathBuf>) -> Self {
        let trusted_roots = trusted_roots
            .into_iter()
            .map(|root| resolve(&root))
            .collect();
        Self {
            store_path: store_path.into(),
            trusted_roots,
        }
    }

    /// The user's policy: `$XAFT_HOME/trust.json`, trusting the user's own
    /// xaft home and config directory by location.
    pub fn for_user() -> Self {
        let home = xaft_home().unwrap_or_else(|| PathBuf::from(".xaft-home-unavailable"));
        let mut roots = vec![home.clone()];
        roots.extend(user_config_dir());
        Self::new(home.join("trust.json"), roots)
    }

    /// Where records are stored.
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    /// Directories whose files are trusted by location.
    pub fn trusted_roots(&self) -> &[PathBuf] {
        &self.trusted_roots
    }

    /// Why `path` may not be used, or `None` if it may.
    pub fn check(&self, path: &Path, kind: &str) -> Option<UntrustedExtension> {
        let resolved = resolve(path);
        if self
            .trusted_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            return None;
        }
        let refusal = |reason: String| UntrustedExtension {
            path: resolved.clone(),
            kind: kind.to_string(),
            reason,
        };
        let records = self.read_records().unwrap_or_default();
        let key = resolved.display().to_string();
        let Some(record) = records.iter().find(|r| r.path == key) else {
            return Some(refusal("not trusted".into()));
        };
        match std::fs::read(&resolved) {
            Ok(bytes) if sha256_hex(&bytes) == record.sha256 => None,
            Ok(_) => Some(refusal("changed since it was trusted".into())),
            Err(e) => Some(refusal(format!("unreadable: {e}"))),
        }
    }

    /// Whether `path` may be used; the refusal is returned for reporting.
    pub fn permits(&self, path: &Path, kind: &str) -> Result<(), UntrustedExtension> {
        match self.check(path, kind) {
            None => Ok(()),
            Some(refusal) => Err(refusal),
        }
    }

    /// Every record, sorted by path.
    pub fn records(&self) -> Vec<TrustRecord> {
        let mut records = self.read_records().unwrap_or_default();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        records
    }

    /// Read the store. A missing file is empty; an unreadable one is an error.
    ///
    /// Malformed entries are skipped one by one, as agenthicc does, so one bad
    /// record cannot revoke every other.
    pub fn read_records(&self) -> Result<Vec<TrustRecord>, String> {
        let text = match std::fs::read_to_string(&self.store_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let file: StoreFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(file
            .records
            .into_iter()
            .filter_map(|value| serde_json::from_value::<TrustRecord>(value).ok())
            .collect())
    }
}

/// Resolve symlinks where the path exists; otherwise make it absolute.
pub fn resolve(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path) -> ExtensionTrust {
        ExtensionTrust::new(dir.join("home/trust.json"), vec![dir.join("home")])
    }

    fn write_store(trust: &ExtensionTrust, records: serde_json::Value) {
        std::fs::create_dir_all(trust.store_path().parent().unwrap()).unwrap();
        std::fs::write(
            trust.store_path(),
            serde_json::json!({"version": STORE_VERSION, "records": records}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn an_unrecorded_file_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        let refusal = policy(dir.path()).check(&file, "cli").unwrap();
        assert_eq!(refusal.reason, "not trusted");
        assert_eq!(refusal.kind, "cli");
    }

    #[test]
    fn a_record_binds_the_digest_and_an_edit_revokes_it() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("tool.toml");
        std::fs::write(&file, "v1").unwrap();
        write_store(
            &trust,
            serde_json::json!([{
                "path": resolve(&file).display().to_string(),
                "sha256": sha256_hex(b"v1"),
                "kind": "tool",
                "trusted_at": "t",
            }]),
        );
        assert!(trust.permits(&file, "tool").is_ok());
        std::fs::write(&file, "v2").unwrap();
        assert_eq!(
            trust.check(&file, "tool").unwrap().reason,
            "changed since it was trusted"
        );
    }

    #[test]
    fn files_under_a_trusted_root_need_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        assert!(policy(dir.path()).check(&file, "cli").is_none());
    }

    #[test]
    fn an_unreadable_store_trusts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        std::fs::create_dir_all(trust.store_path().parent().unwrap()).unwrap();
        std::fs::write(trust.store_path(), "{ not json").unwrap();
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(trust.check(&file, "cli").unwrap().reason, "not trusted");
        assert!(trust.read_records().is_err());
    }

    #[test]
    fn one_malformed_record_does_not_hide_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        write_store(
            &trust,
            serde_json::json!([{"path": 3}, {
                "path": resolve(&file).display().to_string(),
                "sha256": sha256_hex(b"x"),
                "kind": "cli",
                "trusted_at": "t",
            }]),
        );
        assert!(trust.check(&file, "cli").is_none());
        assert_eq!(trust.records().len(), 1);
    }
}
