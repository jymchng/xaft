//! Plugin trust decisions and the load audit journal.
//!
//! Parity with `agenthicc.plugins.trust.check_trust` and the
//! `PluginSettings` keys it consumes: `auto_trust`, `trust_file`, `audit_file`,
//! `disabled`, `allowed_modules` and `timeout_seconds`.
//!
//! The trust rule is content-addressed. A plugin is trusted for the *bytes* it
//! had when the user agreed, so editing a trusted plugin revokes its trust
//! rather than inheriting it -- otherwise "I reviewed this" would silently
//! extend to whatever the file was replaced with.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use xaft_config::PluginConfig;

/// Format version written into the trust file.
const TRUST_FILE_VERSION: u32 = 1;

/// What to do about one plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// Load it for this run; the hash already matched.
    TrustOnce,
    /// Load it and record the decision.
    AlwaysTrust,
    /// Do not load it.
    Skip,
}

impl TrustDecision {
    /// Whether this decision permits loading.
    pub const fn allows_load(self) -> bool {
        matches!(self, Self::TrustOnce | Self::AlwaysTrust)
    }
}

/// One entry of the trust file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustRecord {
    /// Hex SHA-256 of the file when it was trusted.
    #[serde(default)]
    pub sha256: String,
    /// When the decision was made, as an RFC 3339 timestamp.
    #[serde(default)]
    pub trusted_at: String,
    /// The decision that was taken.
    #[serde(default)]
    pub decision: String,
}

/// The trust file's contents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustManifest {
    /// Schema version.
    #[serde(default)]
    pub version: u32,
    /// Trusted paths, keyed by absolute path.
    #[serde(default)]
    pub trusted: std::collections::BTreeMap<String, TrustRecord>,
}

impl TrustManifest {
    /// Parse a trust file.
    ///
    /// A missing or unreadable file yields an empty manifest rather than an
    /// error: "nothing is trusted yet" is the correct state for both, and
    /// failing closed on a corrupt file would leave the user unable to load
    /// anything without hand-editing JSON.
    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }
}

/// One line of the audit journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// RFC 3339 timestamp.
    pub at: String,
    /// Plugin path, as written to the journal.
    pub plugin: String,
    /// The decision taken.
    pub decision: String,
    /// Hex SHA-256 of the plugin's contents at decision time.
    pub sha256: String,
}

/// Evaluates [`PluginConfig`] for one plugin file.
#[derive(Debug, Clone)]
pub struct PluginTrust {
    auto_trust: bool,
    disabled: Vec<String>,
    allowed_modules: Vec<String>,
    timeout_seconds: f64,
    trust_file: PathBuf,
    audit_file: PathBuf,
}

/// Per-plugin execution ceiling for auto-installed dependencies.
#[derive(Debug, Clone, PartialEq)]
pub struct InstallPlan {
    /// Where dependencies are installed.
    pub target: String,
    /// Whether to install them at all.
    pub auto_install: bool,
    /// Ceiling applied to the installer, seconds.
    pub timeout_seconds: f64,
    /// Whether a plugin shadowing a built-in command is an error.
    pub strict_cli_shadow: bool,
}

impl PluginTrust {
    /// Build an evaluator from the `[plugins]` section.
    pub fn new(config: &PluginConfig) -> Self {
        Self {
            auto_trust: config.auto_trust,
            disabled: config.disabled.clone(),
            allowed_modules: config.allowed_modules.clone(),
            timeout_seconds: config.timeout_seconds,
            trust_file: config.trust_file.clone(),
            audit_file: config.audit_file.clone(),
        }
    }

    /// The installer policy, read from the `[plugins]` section.
    ///
    /// Kept separate from [`Self`] because it answers a different question:
    /// not "may this plugin run" but "if it needs a dependency, may xaft fetch
    /// one, and where does it go".
    pub fn install_plan(config: &PluginConfig) -> InstallPlan {
        InstallPlan {
            target: config.install_target.clone(),
            auto_install: config.auto_install,
            timeout_seconds: config.timeout_seconds,
            strict_cli_shadow: config.strict_cli_shadow,
        }
    }

    /// Whether a shadowing plugin is an error rather than a warning.
    pub fn is_strict_cli_shadow(config: &PluginConfig) -> bool {
        config.strict_cli_shadow
    }

    /// Whether missing dependencies may be installed automatically.
    ///
    /// Off by default: this makes loading a config file run package
    /// installation, so the default has to be the one that does nothing.
    pub fn allows_auto_install(config: &PluginConfig) -> bool {
        config.auto_install
    }

    /// Where trust decisions are recorded.
    pub fn trust_file(&self) -> &Path {
        &self.trust_file
    }

    /// Where loads are journaled.
    pub fn audit_file(&self) -> &Path {
        &self.audit_file
    }

    /// The per-plugin execution ceiling, seconds.
    pub fn timeout_seconds(&self) -> f64 {
        self.timeout_seconds
    }

    /// Whether a plugin name is on the disabled list.
    ///
    /// Matched on the file stem as well as the full file name, so `disabled =
    /// ["myplugin"]` catches `myplugin.xaft-plugin`.
    pub fn is_disabled(&self, plugin_path: &Path) -> bool {
        let file = plugin_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = plugin_path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.disabled
            .iter()
            .any(|name| *name == file || *name == stem)
    }

    /// Whether `module` may be imported by a plugin.
    ///
    /// Empty means **no** module. That is the fail-closed reading, and it is
    /// the safe one: a plugin runs with the user's permissions, so an empty
    /// allow-list that meant "everything" would hand every plugin the standard
    /// library and the network stack.
    pub fn is_module_allowed(&self, module: &str) -> bool {
        self.allowed_modules.iter().any(|allowed| allowed == module)
    }

    /// Decide whether `plugin_path` may load, given the recorded trust.
    ///
    /// Order matters and matches agenthicc: `disabled` first, then an
    /// already-recorded matching hash, then `auto_trust`. Checking `disabled`
    /// first is what stops `auto_trust` from re-enabling a plugin the user
    /// turned off.
    pub fn check(
        &self,
        plugin_path: &Path,
        contents: &[u8],
        manifest: &TrustManifest,
    ) -> TrustDecision {
        if self.is_disabled(plugin_path) {
            return TrustDecision::Skip;
        }

        let digest = sha256_hex(contents);
        let key = plugin_key(plugin_path);
        if manifest
            .trusted
            .get(&key)
            .is_some_and(|r| r.sha256 == digest)
        {
            return TrustDecision::TrustOnce;
        }

        if self.auto_trust {
            return TrustDecision::AlwaysTrust;
        }
        TrustDecision::Skip
    }

    /// The manifest entry to write for a decision.
    ///
    /// `None` for a decision that is not recorded, so a plain skip never
    /// rewrites the trust file.
    ///
    /// Takes no path: the entry records the *hash* of what was reviewed, and
    /// the path it applies to is supplied separately to [`Self::apply`].
    /// Passing it here would suggest the path influenced the hash, which it
    /// does not -- the whole point of content addressing is that it does not.
    pub fn record_for(
        &self,
        contents: &[u8],
        decision: TrustDecision,
        trusted_at: &str,
    ) -> Option<TrustRecord> {
        if decision != TrustDecision::AlwaysTrust {
            return None;
        }
        Some(TrustRecord {
            sha256: sha256_hex(contents),
            trusted_at: trusted_at.to_string(),
            decision: "always_trust".to_string(),
        })
    }

    /// Apply a decision to `manifest`, returning whether it changed.
    pub fn apply(
        &self,
        manifest: &mut TrustManifest,
        plugin_path: &Path,
        record: TrustRecord,
    ) -> bool {
        let key = plugin_key(plugin_path);
        let entry = manifest.trusted.entry(key).or_default();
        let changed = entry.sha256 != record.sha256 || entry.trusted_at != record.trusted_at;
        entry.sha256 = record.sha256;
        entry.trusted_at = record.trusted_at;
        entry.decision = record.decision;
        if manifest.version == 0 {
            manifest.version = TRUST_FILE_VERSION;
        }
        changed
    }

    /// One audit line for a decision.
    pub fn audit_record(
        &self,
        plugin_path: &Path,
        contents: &[u8],
        decision: TrustDecision,
        at: &str,
    ) -> AuditRecord {
        AuditRecord {
            at: at.to_string(),
            plugin: plugin_path.display().to_string(),
            decision: decision.as_str().to_string(),
            sha256: sha256_hex(contents),
        }
    }
}

impl TrustDecision {
    /// The wire spelling used in the trust file and audit journal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrustOnce => "trust_once",
            Self::AlwaysTrust => "always_trust",
            Self::Skip => "skip",
        }
    }
}

/// Hex SHA-256 of `contents`.
pub fn sha256_hex(contents: &[u8]) -> String {
    let digest = Sha256::digest(contents);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The key a plugin is recorded under: its absolute path when it has one.
fn plugin_key(path: &Path) -> String {
    path.display().to_string()
}
