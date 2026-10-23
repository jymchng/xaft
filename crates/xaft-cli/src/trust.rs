//! Digest-bound trust for project extensions (`xaft trust`, parity row 47).
//!
//! Port of `agenthicc.plugins.trust.ExtensionTrust` (PRD-250 S-02). A
//! repository can ship files that xaft *executes*: today that is
//! `.xaft/cli.toml`, whose `[[command]]` tables run programs (see
//! [`crate::cli_plugins`]). Running one acts with the user's permissions, so a
//! loader may use an extension file only when it is trusted:
//!
//! * a file under a **trusted root** is trusted by location — the user's own
//!   xaft home (`$XAFT_HOME`, default `~/.xaft`) and user config directory
//!   (`~/.config/xaft`). Nothing a repository ships lives there;
//! * any other file needs a **trust record** binding its resolved path to the
//!   SHA-256 of its current bytes. Editing the file invalidates the record.
//!
//! Records live in `$XAFT_HOME/trust.json`, never in the repository — a store
//! inside the checkout could forge its own trust. That is also why this store
//! deliberately does **not** honour `[plugins] trust_file`: that key is
//! project-overridable and defaults to a path *inside* the repository
//! (`.agenthicc/trusted_plugins.json`), which is exactly the forgeable
//! location agenthicc's PRD-250 moved away from.
//!
//! # Wire format
//!
//! Identical to agenthicc's `trust.json`:
//!
//! ```json
//! {"version": 1, "records": [{"path": "/abs/x", "sha256": "…", "kind": "cli", "trusted_at": "…"}]}
//! ```
//!
//! # One deliberate divergence
//!
//! agenthicc logs and ignores an unreadable store, and its next `trust add`
//! then overwrites it with only the new record — silently dropping every
//! other record the file held. Here an unreadable store still trusts nothing,
//! but `trust`/`revoke` refuse to write over it and say why.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Version written into `trust.json`.
const STORE_VERSION: u32 = 1;

/// The project extension files xaft can execute, by location.
///
/// agenthicc's table maps six `.agenthicc/<dir>` directories of Python modules
/// to kinds. xaft loads no Python, so the only executable extension a
/// repository can ship is the CLI-plugin table.
pub const PROJECT_CLI_PLUGINS: &str = ".xaft/cli.toml";

/// One trusted extension file, bound to the content that was reviewed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// Resolved absolute path.
    pub path: String,
    /// Hex SHA-256 of the file when it was trusted.
    pub sha256: String,
    /// Extension kind, e.g. `cli`.
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

/// Decides whether an extension file may be used, and records trust.
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

    /// Trust the current content of `path`.
    pub fn trust(&self, path: &Path, kind: &str) -> std::io::Result<TrustRecord> {
        let resolved = resolve(path);
        let bytes = std::fs::read(&resolved)?;
        let record = TrustRecord {
            path: resolved.display().to_string(),
            sha256: sha256_hex(&bytes),
            kind: kind.to_string(),
            trusted_at: chrono::Utc::now().to_rfc3339(),
        };
        let mut records = self.read_records_for_write()?;
        records.retain(|r| r.path != record.path);
        records.push(record.clone());
        self.write_records(records)?;
        Ok(record)
    }

    /// Forget the record for `path`; whether one existed.
    pub fn revoke(&self, path: &Path) -> std::io::Result<bool> {
        let key = resolve(path).display().to_string();
        let mut records = self.read_records_for_write()?;
        let before = records.len();
        records.retain(|r| r.path != key);
        if records.len() == before {
            return Ok(false);
        }
        self.write_records(records)?;
        Ok(true)
    }

    /// Every record, sorted by path.
    pub fn records(&self) -> Vec<TrustRecord> {
        let mut records = self.read_records().unwrap_or_default();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        records
    }

    /// Read the store. A missing file is empty; an unreadable one is an error.
    fn read_records(&self) -> Result<Vec<TrustRecord>, String> {
        let text = match std::fs::read_to_string(&self.store_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let file: StoreFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        // Malformed entries are skipped one by one, as agenthicc does, so one
        // bad record cannot revoke every other.
        Ok(file
            .records
            .into_iter()
            .filter_map(|value| serde_json::from_value::<TrustRecord>(value).ok())
            .collect())
    }

    fn read_records_for_write(&self) -> std::io::Result<Vec<TrustRecord>> {
        self.read_records().map_err(|e| {
            std::io::Error::other(format!(
                "refusing to overwrite unreadable trust store {}: {e}",
                self.store_path.display()
            ))
        })
    }

    fn write_records(&self, mut records: Vec<TrustRecord>) -> std::io::Result<()> {
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let payload = serde_json::json!({ "version": STORE_VERSION, "records": records });
        let mut text = serde_json::to_string_pretty(&payload).map_err(std::io::Error::other)?;
        text.push('\n');
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::project_init::write_atomic(&self.store_path, text.as_bytes())?;
        restrict_mode(&self.store_path)
    }
}

#[cfg(unix)]
fn restrict_mode(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_mode(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Resolve symlinks where the path exists; otherwise make it absolute.
fn resolve(path: &Path) -> PathBuf {
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

/// Every project extension file under `project`, with its kind.
///
/// agenthicc enumerates its six extension directories; xaft has one
/// executable extension file, so this is that file when it exists.
pub fn project_extension_files(project: &Path) -> Vec<(PathBuf, &'static str)> {
    let cli = project.join(PROJECT_CLI_PLUGINS);
    if cli.is_file() {
        vec![(cli, "cli")]
    } else {
        Vec::new()
    }
}

/// The extension kind of `path`, from where it lives.
pub fn kind_for(path: &Path) -> &'static str {
    if path.file_name().is_some_and(|n| n == "cli.toml") {
        "cli"
    } else {
        "extension"
    }
}

/// The files `trust add`/`revoke` act on for a path argument.
///
/// A directory means every CLI-plugin table beneath it (agenthicc: every
/// Python file beneath it); anything else is taken as the one file named.
pub fn targets(path: &Path) -> Vec<PathBuf> {
    if !path.is_dir() {
        return vec![path.to_path_buf()];
    }
    let mut found = Vec::new();
    collect_cli_tables(path, &mut found);
    found.sort();
    found
}

fn collect_cli_tables(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_cli_tables(&path, out);
        } else if file_type.is_file() && path.file_name().is_some_and(|n| n == "cli.toml") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path) -> ExtensionTrust {
        ExtensionTrust::new(dir.join("home/trust.json"), vec![dir.join("home")])
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
    fn trusting_records_the_digest_and_editing_revokes_it() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "v1").unwrap();

        let record = trust.trust(&file, "cli").unwrap();
        assert_eq!(record.sha256, sha256_hex(b"v1"));
        assert!(trust.check(&file, "cli").is_none());

        std::fs::write(&file, "v2").unwrap();
        assert_eq!(
            trust.check(&file, "cli").unwrap().reason,
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
    fn the_store_round_trips_in_agenthicc_wire_format() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        trust.trust(&file, "cli").unwrap();

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(trust.store_path()).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        let record = &raw["records"][0];
        for key in ["path", "sha256", "kind", "trusted_at"] {
            assert!(record[key].is_string(), "record lacks {key}: {record}");
        }
        assert_eq!(trust.records().len(), 1);
    }

    #[test]
    fn revoke_reports_whether_a_record_existed() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        assert!(!trust.revoke(&file).unwrap());
        trust.trust(&file, "cli").unwrap();
        assert!(trust.revoke(&file).unwrap());
        assert!(trust.records().is_empty());
        assert_eq!(trust.check(&file, "cli").unwrap().reason, "not trusted");
    }

    #[test]
    fn an_unreadable_store_trusts_nothing_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        std::fs::create_dir_all(trust.store_path().parent().unwrap()).unwrap();
        std::fs::write(trust.store_path(), "{ not json").unwrap();
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();

        assert_eq!(trust.check(&file, "cli").unwrap().reason, "not trusted");
        let err = trust.trust(&file, "cli").unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"));
        assert_eq!(
            std::fs::read_to_string(trust.store_path()).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn one_malformed_record_does_not_hide_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        let good = serde_json::json!({
            "path": resolve(&file).display().to_string(),
            "sha256": sha256_hex(b"x"),
            "kind": "cli",
            "trusted_at": "t",
        });
        std::fs::create_dir_all(trust.store_path().parent().unwrap()).unwrap();
        std::fs::write(
            trust.store_path(),
            serde_json::json!({"version": 1, "records": [{"path": 3}, good]}).to_string(),
        )
        .unwrap();
        assert!(trust.check(&file, "cli").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let file = dir.path().join("cli.toml");
        std::fs::write(&file, "x").unwrap();
        trust.trust(&file, "cli").unwrap();
        let mode = std::fs::metadata(trust.store_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn project_extensions_are_the_cli_table_when_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(project_extension_files(dir.path()).is_empty());
        std::fs::create_dir_all(dir.path().join(".xaft")).unwrap();
        std::fs::write(dir.path().join(PROJECT_CLI_PLUGINS), "").unwrap();
        let files = project_extension_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "cli");
    }

    #[test]
    fn a_directory_target_means_every_cli_table_beneath_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/cli.toml"), "").unwrap();
        std::fs::write(dir.path().join("a/b/cli.toml"), "").unwrap();
        std::fs::write(dir.path().join("a/other.toml"), "").unwrap();
        let found = targets(&dir.path().join("a"));
        assert_eq!(found.len(), 2);
        assert_eq!(targets(Path::new("/no/such/file")).len(), 1);
    }
}
