//! Persistent MCP server configuration for the `xaft mcp` subcommands.
//!
//! This is the port of `agenthicc.cli.mcp_config`. It owns three things and
//! nothing else: which file a subcommand reads and writes, how a `[[mcp.client]]`
//! stanza is validated, and how stanzas are added and removed.
//!
//! # The file is edited surgically, never re-serialised
//!
//! agenthicc appends a stanza for `add` and excises one for `remove`, working
//! on the raw text. The obvious alternative — parse the file into
//! [`XaftConfig`](xaft_config::XaftConfig), push or pull from `mcp.client`, and
//! write it back with `toml::to_string_pretty` — is *wrong* for a user-edited
//! config: TOML comments, key ordering and blank lines do not survive a serde
//! round trip, so a single `mcp add` would silently rewrite the user's whole
//! file. `remove_mcp_server` carries the same warning in agenthicc
//! ("while preserving unrelated TOML text").
//!
//! The cost of the surgical approach is that `remove` can only delete a stanza
//! written as an explicit `[[mcp.client]]` header. A config that inlines the
//! array as `client = [{ name = "x" }]` is reported as
//! `could not locate MCP stanza` rather than being rewritten. That is
//! deliberate: leaving the file alone beats mangling it.
//!
//! # Two deliberate divergences from agenthicc
//!
//! * **Stanza location.** agenthicc writes `[[tools.mcp_servers]]`; xaft's
//!   config tree puts client connections under `[mcp]`, so this writes
//!   `[[mcp.client]]`.
//! * **The write is atomic and mode-preserving, but not fsynced.** agenthicc
//!   `fsync`s the temporary file before `os.replace`; Rust's
//!   [`std::fs::rename`] is atomic within a filesystem but this code does not
//!   open a handle to fsync, so a power loss between the write and the rename
//!   can in principle leave the old file. The rename itself is never torn.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use xaft_config::{MCP_TRANSPORTS, McpClientConfig};

/// The TOML header of a single client stanza.
pub const STANZA_HEADER: &str = "[[mcp.client]]";

/// The transports agenthicc accepts, lowercase.
const VALID_TRANSPORTS: &[&str] = &["stdio", "ws", "websocket", "streamable", "http"];

/// A failure from reading or writing MCP configuration.
///
/// agenthicc raises `McpConfigError` for all of these and the CLI prints
/// `error: {msg}` without a non-zero exit, so the message is the whole
/// user-visible contract.
#[derive(Debug)]
pub enum McpError {
    /// Both `--global` and `--project` were given.
    ConflictingScope,
    /// The file could not be read, parsed or written.
    Io(String),
    /// A supplied field was rejected before anything was written.
    Invalid(String),
    /// A server could not be found, or exists but does not parse.
    NotFound(String),
    /// The named stanza exists in the parsed document but has no editable text.
    StanzaNotFound(String),
    /// No user config directory could be determined for `--global`.
    NoHomeDir,
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingScope => write!(f, "choose only one target: --global or --project"),
            Self::Io(msg) => write!(f, "{msg}"),
            Self::Invalid(msg) => write!(f, "{msg}"),
            Self::NotFound(msg) => write!(f, "MCP server not found: {msg}"),
            Self::StanzaNotFound(name) => {
                write!(f, "could not locate MCP stanza for {name}")
            }
            Self::NoHomeDir => write!(f, "could not determine user config directory"),
        }
    }
}

impl std::error::Error for McpError {}

/// One `[[mcp.client]]` entry as read from disk.
///
/// agenthicc's `mcp list` degrades a stanza that fails to parse into
/// `{"name": ..., "status": "invalid"}` rather than aborting the whole
/// listing — one typo in one of five servers should not hide the other four.
#[derive(Debug, Clone)]
pub enum McpEntry {
    /// The stanza parsed into a full configuration.
    Valid(McpClientConfig),
    /// The stanza did not parse, but its `name` was recoverable.
    Invalid {
        /// The name from the raw table, or `"<invalid>"` if it had none.
        name: String,
    },
}

impl McpEntry {
    /// The server's name, however it was recovered.
    pub fn name(&self) -> &str {
        match self {
            Self::Valid(c) => &c.name,
            Self::Invalid { name } => name,
        }
    }

    /// The JSON a CLI command prints for this entry.
    ///
    /// Invalid entries get the same two keys agenthicc gives them, so a
    /// consumer parsing `--json` output handles one shape, not two.
    pub fn redacted(&self) -> serde_json::Value {
        match self {
            Self::Valid(c) => c.redacted(),
            Self::Invalid { name } => {
                let mut map = serde_json::Map::new();
                map.insert("name".into(), name.clone().into());
                map.insert("status".into(), "invalid".into());
                serde_json::Value::Object(map)
            }
        }
    }
}

/// Where a subcommand should read and write MCP configuration.
///
/// Not `Copy`: `explicit` is an owned `PathBuf`, and taking a copy of a scope
/// per subcommand is not worth a `clone()`.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Write the user-global config (`~/.config/xaft/xaft.toml`).
    pub global: bool,
    /// Write the nearest project config, walking up from the cwd.
    pub project: bool,
    /// An explicit path from `--config`, which wins over both flags.
    pub explicit: Option<PathBuf>,
}

/// A validated `mcp add` request.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// The server name.
    pub name: String,
    /// The transport.
    pub transport: String,
    /// The command (stdio) or URL (everything else).
    pub target: String,
    /// Arguments for a stdio command.
    pub args: Vec<String>,
    /// Environment variable holding the bearer token, if any.
    pub token_env: Option<String>,
    /// Whether to connect at session start.
    pub auto_connect: bool,
    /// Reconnect attempts.
    pub reconnect_attempts: u32,
    /// Reconnect delay, in seconds.
    pub reconnect_delay_seconds: f64,
}

impl ServerSpec {
    /// Validate the request, normalising the fields agenthicc normalises.
    ///
    /// Every rule here is transcribed from agenthicc's `_validate`, including
    /// its error strings, because those strings are what a user sees when
    /// `mcp add` rejects a typo.
    pub fn validated(mut self) -> Result<Self, McpError> {
        self.name = self.name.trim().to_string();
        if !valid_server_name(&self.name) {
            return Err(McpError::Invalid(
                "server name must start with a letter or number and contain only \
                 letters, numbers, '.', '_' or '-'"
                    .into(),
            ));
        }

        self.target = self.target.trim().to_string();
        if self.target.is_empty() {
            return Err(McpError::Invalid(
                "server URL or stdio command cannot be empty".into(),
            ));
        }

        self.transport = self.transport.trim().to_ascii_lowercase();
        if !VALID_TRANSPORTS.contains(&self.transport.as_str())
            && !MCP_TRANSPORTS.contains(&self.transport.as_str())
        {
            let mut choices: Vec<&str> = VALID_TRANSPORTS.to_vec();
            for extra in MCP_TRANSPORTS {
                if !choices.contains(extra) {
                    choices.push(extra);
                }
            }
            choices.sort_unstable();
            return Err(McpError::Invalid(format!(
                "unsupported transport '{}'; choose from {}",
                self.transport,
                choices.join(", ")
            )));
        }

        if let Some(env) = self.token_env.as_deref() {
            if !valid_env_name(env) {
                return Err(McpError::Invalid(
                    "--token-env must be an uppercase environment variable name".into(),
                ));
            }
        }

        if self.reconnect_delay_seconds.is_nan()
            || self.reconnect_delay_seconds.is_infinite()
            || self.reconnect_delay_seconds < 0.0
        {
            return Err(McpError::Invalid(
                "reconnect delay must be a finite, non-negative number".into(),
            ));
        }

        Ok(self)
    }

    /// Render the stanza body, omitting every field left at its default.
    ///
    /// agenthicc's `_server_block` does the same, and for the same reason: a
    /// minimal stanza is easier to read, and the defaults are recoverable by
    /// anyone who knows them.
    fn render(&self) -> String {
        let mut lines = vec![
            STANZA_HEADER.to_string(),
            format!("name = {}", toml_string(&self.name)),
        ];
        if self.transport == "stdio" {
            lines.push(format!("command = {}", toml_string(&self.target)));
            if !self.args.is_empty() {
                let rendered: Vec<String> = self.args.iter().map(|a| toml_string(a)).collect();
                lines.push(format!("args = [{}]", rendered.join(", ")));
            }
        } else {
            lines.push(format!("url = {}", toml_string(&self.target)));
        }
        lines.push(format!("transport = {}", toml_string(&self.transport)));
        if let Some(env) = &self.token_env {
            // agenthicc stores `${ENV}` in the token field. xaft keeps the
            // reference verbatim in `token` *and* records the variable name in
            // `token_env`, so the two agree and a tool that wants the name does
            // not have to parse the reference back apart.
            lines.push(format!("token = {}", toml_string(&format!("${{{env}}}"))));
            lines.push(format!("token_env = {}", toml_string(env)));
        }
        if !self.auto_connect {
            lines.push("auto_connect = false".into());
        }
        if self.reconnect_attempts != 3 {
            lines.push(format!("reconnect_attempts = {}", self.reconnect_attempts));
        }
        if self.reconnect_delay_seconds != 1.0 {
            lines.push(format!(
                "reconnect_delay_seconds = {}",
                self.reconnect_delay_seconds
            ));
        }
        lines.join("\n") + "\n"
    }
}

/// Whether `name` satisfies agenthicc's `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`.
///
/// Hand-rolled rather than pulled from a regex crate: the class is small, the
/// length cap matters (a 64-character name is rejected), and a new dependency
/// is not worth it for one character class.
fn valid_server_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    if name.len() > 64 {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Whether `name` satisfies agenthicc's `^[A-Z_][A-Z0-9_]*$`.
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Render a string as a TOML basic string, escaping what TOML requires.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Resolve the config file a subcommand should use, without touching it.
///
/// Mirrors agenthicc's `mcp_config_path`: an explicit `--config` wins outright,
/// `--global` and `--project` are mutually exclusive, and the project form
/// walks up from `cwd` looking for an existing file before falling back to the
/// conventional `.xaft/xaft.toml`.
pub fn config_path(scope: Scope, cwd: &Path, home: Option<&Path>) -> Result<PathBuf, McpError> {
    if scope.global && scope.project {
        return Err(McpError::ConflictingScope);
    }
    if let Some(explicit) = scope.explicit {
        return Ok(explicit);
    }

    if scope.global {
        let home = home.ok_or(McpError::NoHomeDir)?;
        return Ok(home.join(".config/xaft/xaft.toml"));
    }

    // Project scope: the same flat-then-nested precedence the config loader
    // uses, so `mcp add` and `xaft config show` agree on which file is "the"
    // project config.
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let flat = dir.join(".xaft.toml");
        if flat.is_file() {
            return Ok(flat);
        }
        let nested = dir.join(".xaft/xaft.toml");
        if nested.is_file() {
            return Ok(nested);
        }
        current = dir.parent();
    }
    Ok(cwd.join(".xaft/xaft.toml"))
}

/// Read every `[[mcp.client]]` stanza from `path`.
///
/// A missing file is not an error — it is an empty list, which is what makes
/// `mcp list` on a fresh checkout print its "No MCP servers configured" line
/// rather than complain.
pub fn read_mcp_servers(path: &Path) -> Result<Vec<McpEntry>, McpError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(McpError::Io(format!(
                "could not read {}: {e}",
                path.display()
            )));
        }
    };
    parse_servers(&text)
}

/// Parse MCP entries out of TOML text.
fn parse_servers(text: &str) -> Result<Vec<McpEntry>, McpError> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let document: toml::Value = toml::from_str(text)
        .map_err(|e| McpError::Io(format!("could not read configuration: {e}")))?;

    let Some(mcp) = document.get("mcp") else {
        return Ok(Vec::new());
    };
    if !mcp.is_table() {
        return Err(McpError::Invalid("[mcp] must be a TOML table".into()));
    }
    let Some(client) = mcp.get("client") else {
        return Ok(Vec::new());
    };
    let Some(servers) = client.as_array() else {
        return Err(McpError::Invalid(
            "mcp.client must be an array of tables".into(),
        ));
    };

    Ok(servers
        .iter()
        .map(|item| {
            // Read `name` off the *raw* table before the typed parse, because
            // `McpClientConfig::name` carries a serde default of `""` (so the
            // typed loader tolerates a partially-specified stanza instead of
            // rejecting the whole file). That default means the typed parse
            // can no longer distinguish "no name at all" from "name is the
            // empty string" — only the raw key can.
            //
            // agenthicc draws the same line in `_redacted_servers`, which
            // reports `{"name": raw.get("name", "<invalid>"), ...}`: a stanza
            // with no `name` key is invalid and is printed under the literal
            // `<invalid>`, while an explicit `name = ""` is whatever the user
            // wrote.
            match item.get("name").and_then(|n| n.as_str()) {
                None => McpEntry::Invalid {
                    name: "<invalid>".into(),
                },
                Some(name) => match McpClientConfig::deserialize(item.clone()) {
                    Ok(config) => McpEntry::Valid(config),
                    // One malformed stanza must not hide the servers around it,
                    // so the name is recovered from the raw table.
                    Err(_) => McpEntry::Invalid {
                        name: name.to_string(),
                    },
                },
            }
        })
        .collect())
}

/// Append a stanza to `path`.
///
/// Returns the server name on success. Refuses to write when the name is
/// already present, so a repeated `mcp add` is an error rather than a
/// duplicate the user has to clean up by hand.
pub fn add_mcp_server(path: &Path, spec: &ServerSpec) -> Result<String, McpError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(McpError::Io(format!(
                "cannot safely update {}: {e}",
                path.display()
            )));
        }
    };
    // Parse before writing: appending to a file that does not parse would
    // produce a file that still does not parse, having told the user it
    // succeeded.
    if !existing.trim().is_empty() {
        toml::from_str::<toml::Value>(&existing)
            .map_err(|e| McpError::Io(format!("cannot safely update {}: {e}", path.display())))?;
    }

    let names: Vec<String> = parse_servers(&existing)?
        .into_iter()
        .map(|e| e.name().to_string())
        .collect();
    if names.contains(&spec.name) {
        return Err(McpError::Invalid(format!(
            "MCP server already exists: {}",
            spec.name
        )));
    }

    let separator = if existing.is_empty() || existing.ends_with('\n') {
        if existing.is_empty() { "" } else { "\n" }
    } else {
        "\n\n"
    };
    let updated = format!("{existing}{separator}{}", spec.render());
    write_atomically(path, &updated)?;
    Ok(spec.name.clone())
}

/// Remove the stanza for `name`, preserving every other byte of the file.
pub fn remove_mcp_server(path: &Path, name: &str) -> Result<(), McpError> {
    let clean = name.trim();
    if !valid_server_name(clean) {
        return Err(McpError::Invalid("invalid MCP server name".into()));
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if !parse_servers(&existing)?.iter().any(|e| e.name() == clean) {
        return Err(McpError::NotFound(clean.to_string()));
    }

    let lines: Vec<&str> = existing.split_inclusive('\n').collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == STANZA_HEADER)
        .map(|(i, _)| i)
        .collect();

    let mut removed = None;
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(lines.len());
        let block = lines[start..end].concat();
        let Ok(parsed) = toml::from_str::<toml::Value>(&block) else {
            continue;
        };
        let item = parsed
            .get("mcp")
            .and_then(|m| m.get("client"))
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        if item.and_then(|i| i.get("name")).and_then(|n| n.as_str()) == Some(clean) {
            removed = Some((start, end));
            break;
        }
    }

    let (start, end) = removed.ok_or_else(|| McpError::StanzaNotFound(clean.to_string()))?;
    let mut updated = String::with_capacity(existing.len());
    updated.push_str(&lines[..start].concat());
    updated.push_str(&lines[end..].concat());
    write_atomically(path, &updated)?;
    Ok(())
}

/// Write `contents` to `path` without ever leaving a half-written config.
///
/// The sequence is agenthicc's: write a sibling temporary file, give it the
/// mode of the file it replaces, then rename over the target. A crash between
/// write and rename leaves the original intact; a crash during the rename
/// leaves either the original or the new file, never a mixture.
fn write_atomically(path: &Path, contents: &str) -> Result<(), McpError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)
            .map_err(|e| McpError::Io(format!("could not write {}: {e}", path.display())))?;
    }

    // agenthicc creates new config files 0o600: an MCP stanza can carry a
    // token, and this file should not be world-readable just because the
    // command that made it was convenient.
    let mode = std::fs::metadata(path)
        .map(|m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o777
            }
            #[cfg(not(unix))]
            {
                let _ = m;
                0o600
            }
        })
        .unwrap_or(0o600);

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "xaft.toml".to_string());
    let temp = path.with_file_name(format!(".{file_name}.tmp{}", std::process::id()));

    std::fs::write(&temp, contents)
        .map_err(|e| McpError::Io(format!("could not write {}: {e}", path.display())))?;
    set_mode(&temp, mode)?;
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(McpError::Io(format!(
            "could not write {}: {e}",
            path.display()
        )));
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), McpError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| McpError::Io(format!("could not write {}: {e}", path.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
