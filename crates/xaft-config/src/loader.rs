//! Configuration loader — full hierarchical merge pipeline.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::error::ConfigError;
use crate::interpolate::interpolate_strings;
use crate::merge::deep_merge;
use crate::types::{CliOverrides, LogLevel, XaftConfig};
use crate::validate::validate;

/// Loads and merges `XaftConfig` from all sources in the correct precedence order.
///
/// Precedence (highest wins):
/// 1. CLI flags (`CliOverrides`)
/// 2. Environment variables (`XAFT_*`)
/// 3. Session overrides (`~/.xaft/sessions/<id>/config.toml`)
/// 4. Project config (`.xaft/xaft.toml` found by walking up from cwd)
/// 5. User global config (`~/.config/xaft/xaft.toml`)
/// 6. Built-in defaults
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load configuration applying the full precedence chain.
    ///
    /// Every file layer is merged as a **raw table** — only the keys that file
    /// actually writes — and the result is deserialized once. Merging fully
    /// deserialized layers instead (the previous behaviour) let a later file's
    /// *defaults* overwrite an earlier file's explicit values: a project
    /// `.xaft.toml` that never mentioned `[core] log_level` reset the user's
    /// global `log_level = "debug"` to `"info"`. agenthicc merges raw dicts
    /// and converts once, and so does this.
    ///
    /// An explicit config file — `--config`, else the `XAFT_CONFIG`
    /// environment variable (agenthicc's `AGENTHICC_CONFIG`) — replaces
    /// project discovery. `--set` / `--set-secret` overrides apply last.
    pub fn load(cli: &CliOverrides) -> Result<XaftConfig, ConfigError> {
        // Step 1: Built-in defaults
        let mut merged = serde_json::to_value(XaftConfig::default())?;
        debug!("Config: built-in defaults loaded");

        // Step 2: User global config
        if let Some(ref path) = global_config_path() {
            if path.exists() {
                deep_merge(&mut merged, Self::load_raw(path)?);
                debug!("Config: merged user global config from {}", path.display());
            }
        }

        // Step 3: Explicit config (--config, else XAFT_CONFIG), which replaces
        // project discovery; otherwise the project config found walking up
        // from cwd / project_dir.
        let explicit = explicit_config_path(cli, std::env::var("XAFT_CONFIG").ok());
        match explicit {
            Some(ref path) => {
                deep_merge(&mut merged, Self::load_raw(path)?);
                debug!("Config: merged explicit config from {}", path.display());
            }
            None => {
                let project_dir = cli
                    .project_dir
                    .clone()
                    .or_else(|| std::env::current_dir().ok());
                if let Some(ref path) = find_project_config(project_dir.as_deref()) {
                    deep_merge(&mut merged, Self::load_raw(path)?);
                    debug!("Config: merged project config from {}", path.display());
                }
            }
        }

        // Step 4: Session overrides
        if let Some(ref session_id) = cli.session_id {
            let session_path = session_config_path(session_id);
            if session_path.exists() {
                deep_merge(&mut merged, Self::load_raw(&session_path)?);
                debug!("Config: merged session config for {}", session_id);
            }
        }

        let mut config: XaftConfig =
            serde_json::from_value(merged).map_err(|e| ConfigError::Validation {
                section: "config".into(),
                message: e.to_string(),
            })?;

        // Step 5: Environment variables
        apply_env_overrides(&mut config)?;
        debug!("Config: env var overrides applied");

        // Step 6: CLI flags, then --set / --set-secret (highest precedence)
        apply_cli_overrides(&mut config, cli);
        apply_set_overrides(&mut config, &cli.set, &cli.set_secret)?;
        debug!("Config: CLI overrides applied");

        // Step 7: Validate final config
        validate(&config)?;

        Ok(config)
    }

    /// Load a single TOML config file and parse it.
    ///
    /// Keys the file does not write keep their built-in defaults; `extends`
    /// chains are resolved first.
    pub fn load_file(path: &Path) -> Result<XaftConfig, ConfigError> {
        let mut base = serde_json::to_value(XaftConfig::default())?;
        deep_merge(&mut base, Self::load_raw(path)?);
        serde_json::from_value(base).map_err(|e| ConfigError::Validation {
            section: format!("{}", path.display()),
            message: e.to_string(),
        })
    }

    /// Read one config file as a raw table: only the keys it writes, with
    /// `${ENV}` references expanded and its `extends` chain merged underneath.
    ///
    /// `extends = "base.toml"` (or a list, merged left to right) names parent
    /// files relative to this file's directory — agenthicc's PRD-113
    /// inheritance. A cycle is an error naming the chain.
    pub fn load_raw(path: &Path) -> Result<serde_json::Value, ConfigError> {
        let mut chain: Vec<PathBuf> = Vec::new();
        Self::load_raw_inner(path, &mut chain)
    }

    fn load_raw_inner(
        path: &Path,
        chain: &mut Vec<PathBuf>,
    ) -> Result<serde_json::Value, ConfigError> {
        let canonical = std::fs::canonicalize(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        if chain.contains(&canonical) {
            let mut names: Vec<String> = chain.iter().map(|p| p.display().to_string()).collect();
            names.push(canonical.display().to_string());
            return Err(ConfigError::Validation {
                section: "extends".into(),
                message: format!("circular extends: {}", names.join(" -> ")),
            });
        }
        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let raw: toml::Value = toml::from_str(&content).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })?;
        let mut own: serde_json::Value = serde_json::to_value(&raw)?;
        interpolate_strings(&mut own);

        let parents: Vec<String> = match own.as_object_mut().and_then(|m| m.remove("extends")) {
            None => Vec::new(),
            Some(serde_json::Value::String(s)) => vec![s],
            Some(serde_json::Value::Array(items)) => {
                let mut out = Vec::new();
                for item in items {
                    match item {
                        serde_json::Value::String(s) => out.push(s),
                        other => {
                            return Err(ConfigError::Validation {
                                section: "extends".into(),
                                message: format!(
                                    "{}: extends entries must be paths, got {other}",
                                    path.display()
                                ),
                            });
                        }
                    }
                }
                out
            }
            Some(other) => {
                return Err(ConfigError::Validation {
                    section: "extends".into(),
                    message: format!(
                        "{}: extends must be a path or a list of paths, got {other}",
                        path.display()
                    ),
                });
            }
        };
        if parents.is_empty() {
            return Ok(own);
        }

        chain.push(canonical);
        let base_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut merged = serde_json::Value::Object(serde_json::Map::new());
        for parent in parents {
            let parent_path = base_dir.join(expand_home(&parent));
            if !parent_path.is_file() {
                return Err(ConfigError::Validation {
                    section: "extends".into(),
                    message: format!(
                        "{} extends '{parent}', which does not exist ({})",
                        path.display(),
                        parent_path.display()
                    ),
                });
            }
            deep_merge(&mut merged, Self::load_raw_inner(&parent_path, chain)?);
        }
        chain.pop();
        deep_merge(&mut merged, own);
        Ok(merged)
    }

    /// Merge `layer` into `base` using deep merge semantics.
    fn merge_into(base: &mut XaftConfig, layer: XaftConfig) -> Result<(), ConfigError> {
        let mut base_val = serde_json::to_value(&*base)?;
        let layer_val = serde_json::to_value(&layer)?;
        deep_merge(&mut base_val, layer_val);
        *base = serde_json::from_value(base_val)?;
        Ok(())
    }
}

/// The explicit config file: `--config`, else a non-blank `XAFT_CONFIG`
/// (agenthicc's precedence: explicit argument > environment > discovery).
fn explicit_config_path(cli: &CliOverrides, env_value: Option<String>) -> Option<PathBuf> {
    cli.config_file.clone().or_else(|| {
        env_value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

/// `~/x` → `$HOME/x`; anything else unchanged.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

// ── --set / --set-secret ──────────────────────────────────────────────────────

/// The raw right-hand side assigned to `key` by the last matching flag.
fn raw_value_of<'a>(sets: &'a [String], secrets: &'a [String], key: &str) -> &'a str {
    sets.iter()
        .chain(secrets.iter())
        .rev()
        .filter_map(|raw| raw.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v)
        .unwrap_or("x")
}

/// Parse a `--set` value the way a TOML file would (numbers, booleans,
/// arrays, quoted strings, inline tables); anything else is a plain string,
/// so `--set core.log_level=debug` needs no quoting.
fn parse_set_value(raw: &str) -> serde_json::Value {
    let raw = raw.trim();
    toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|t| t.get("v").cloned())
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or_else(|| serde_json::Value::String(raw.to_string()))
}

fn split_assignment<'a>(raw: &'a str, flag: &str) -> Result<(&'a str, &'a str), ConfigError> {
    raw.split_once('=')
        .map(|(k, v)| (k.trim(), v))
        .filter(|(k, _)| !k.is_empty() && !k.split('.').any(str::is_empty))
        .ok_or_else(|| ConfigError::Validation {
            section: flag.to_string(),
            message: format!("expected section.key=value, got '{raw}'"),
        })
}

fn set_path(root: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let mut node = root;
    let parts: Vec<&str> = path.split('.').collect();
    for (i, part) in parts.iter().enumerate() {
        if !node.is_object() {
            *node = serde_json::Value::Object(serde_json::Map::new());
        }
        let map = node.as_object_mut().expect("object");
        if i == parts.len() - 1 {
            map.insert((*part).to_string(), value);
            return;
        }
        node = map
            .entry((*part).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
}

fn get_path<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(root, |node, part| node.get(part))
}

/// Apply agenthicc's `--set section.key=value` and
/// `--set-secret section.key=ENV_VAR` overrides, highest precedence.
///
/// A key the config does not know is an error rather than a silent no-op:
/// serde drops unknown fields, so each assignment is checked to survive the
/// round trip.
pub fn apply_set_overrides(
    config: &mut XaftConfig,
    sets: &[String],
    secrets: &[String],
) -> Result<(), ConfigError> {
    if sets.is_empty() && secrets.is_empty() {
        return Ok(());
    }
    let mut value = serde_json::to_value(&*config)?;
    let mut assigned: Vec<(String, &'static str)> = Vec::new();
    for raw in sets {
        let (key, val) = split_assignment(raw, "--set")?;
        set_path(&mut value, key, parse_set_value(val));
        assigned.push((key.to_string(), "--set"));
    }
    for raw in secrets {
        let (key, env) = split_assignment(raw, "--set-secret")?;
        let env = env.trim();
        let secret = std::env::var(env).map_err(|_| ConfigError::Validation {
            section: "--set-secret".into(),
            message: format!("environment variable {env} (for {key}) is not set"),
        })?;
        set_path(&mut value, key, serde_json::Value::String(secret));
        assigned.push((key.to_string(), "--set-secret"));
    }
    let updated: XaftConfig =
        serde_json::from_value(value).map_err(|e| ConfigError::Validation {
            section: "--set".into(),
            message: e.to_string(),
        })?;
    let check = serde_json::to_value(&updated)?;
    for (key, flag) in assigned {
        // Fields skipped when serialized at their default (`false`, empty)
        // vanish from the round trip even though they exist, so only a value
        // that did not survive *and* is not default-like is unknown.
        let set_value = parse_set_value(raw_value_of(sets, secrets, &key));
        let default_like = matches!(
            &set_value,
            serde_json::Value::Bool(false) | serde_json::Value::Null
        ) || set_value.as_str() == Some("")
            || set_value.as_array().is_some_and(Vec::is_empty)
            || set_value.as_object().is_some_and(serde_json::Map::is_empty);
        if get_path(&check, &key).is_none() && !default_like {
            return Err(ConfigError::Validation {
                section: flag.into(),
                message: format!("unknown config key '{key}'"),
            });
        }
    }
    *config = updated;
    Ok(())
}

// ── Env var overrides ─────────────────────────────────────────────────────────

fn apply_env_overrides(config: &mut XaftConfig) -> Result<(), ConfigError> {
    // Core overrides: XAFT_CORE__<KEY>
    if let Ok(val) = std::env::var("XAFT_CORE__LOG_LEVEL") {
        config.core.log_level = val.parse::<LogLevel>().map_err(|_| ConfigError::EnvParse {
            var: "XAFT_CORE__LOG_LEVEL".to_string(),
            expected: "trace|debug|info|warn|error",
        })?;
    }
    if let Ok(val) = std::env::var("XAFT_CORE__DATA_DIR") {
        config.core.data_dir = PathBuf::from(val);
    }
    if let Ok(val) = std::env::var("XAFT_CORE__TELEMETRY") {
        config.core.telemetry = val.parse().map_err(|_| ConfigError::EnvParse {
            var: "XAFT_CORE__TELEMETRY".to_string(),
            expected: "boolean (true/false)",
        })?;
    }

    // Provider overrides: XAFT_PROVIDER_<NAME>__<KEY>
    let provider_names: Vec<String> = config.provider.keys().cloned().collect();
    for name in &provider_names {
        let prefix = format!("XAFT_PROVIDER_{}__", name.to_uppercase().replace('-', "_"));
        if let Some(provider) = config.provider.get_mut(name) {
            if let Ok(val) = std::env::var(format!("{prefix}API_KEY")) {
                provider.api_key = val;
            }
            if let Ok(val) = std::env::var(format!("{prefix}BASE_URL")) {
                provider.base_url = val;
            }
        }
    }

    // Agent overrides: XAFT_AGENT_<NAME>__<KEY>
    let agent_names: Vec<String> = config.agent.keys().cloned().collect();
    for name in &agent_names {
        let prefix = format!("XAFT_AGENT_{}__", name.to_uppercase().replace('-', "_"));
        if let Some(agent) = config.agent.get_mut(name) {
            if let Ok(val) = std::env::var(format!("{prefix}MODEL")) {
                agent.model = val;
            }
            if let Ok(val) = std::env::var(format!("{prefix}PROVIDER")) {
                agent.provider = val;
            }
            if let Ok(val) = std::env::var(format!("{prefix}MAX_TURNS")) {
                agent.max_turns = val.parse().map_err(|_| ConfigError::EnvParse {
                    var: format!("{prefix}MAX_TURNS"),
                    expected: "positive integer",
                })?;
            }
            if let Ok(val) = std::env::var(format!("{prefix}TEMPERATURE")) {
                agent.temperature = val.parse().map_err(|_| ConfigError::EnvParse {
                    var: format!("{prefix}TEMPERATURE"),
                    expected: "float in [0.0, 2.0]",
                })?;
            }
        }
    }

    // Shorthands
    if let Ok(val) = std::env::var("XAFT_MODEL") {
        if let Some(agent) = config.agent.get_mut("default") {
            agent.model = val;
        }
    }
    if let Ok(val) = std::env::var("XAFT_PROVIDER") {
        if let Some(agent) = config.agent.get_mut("default") {
            agent.provider = val;
        }
    }
    if let Ok(val) = std::env::var("XAFT_LOG_LEVEL") {
        config.core.log_level = val.parse::<LogLevel>().unwrap_or(LogLevel::Info);
    }

    // Conventional API key shorthands
    for name in &["anthropic", "openai", "gemini"] {
        let env_var = format!("XAFT_{}_API_KEY", name.to_uppercase());
        if let Ok(val) = std::env::var(&env_var) {
            if let Some(provider) = config.provider.get_mut(*name) {
                provider.api_key = val;
            }
        }
    }

    Ok(())
}

// ── CLI flag overrides ────────────────────────────────────────────────────────

fn apply_cli_overrides(config: &mut XaftConfig, cli: &CliOverrides) {
    let agent_name = cli.agent_preset.as_deref().unwrap_or("default");

    if let Some(agent) = config.agent.get_mut(agent_name) {
        if let Some(ref model) = cli.model {
            agent.model = model.clone();
        }
        if let Some(ref provider) = cli.provider {
            agent.provider = provider.clone();
        }
        if let Some(max_turns) = cli.max_turns {
            agent.max_turns = max_turns;
        }
        if let Some(temperature) = cli.temperature {
            agent.temperature = temperature;
        }
    }

    if let Some(ref level) = cli.log_level {
        config.core.log_level = level.clone();
    }

    if cli.no_telemetry {
        config.core.telemetry = false;
    }

    if cli.auto_approve {
        config.guardrail.command_approval = false;
        config.guardrail.file_destruction = false;
        if let Some(fe) = config.tool.get_mut("file-edit") {
            fe.extra
                .insert("confirm_on_write".to_string(), serde_json::json!(false));
        }
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn global_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("xaft/xaft.toml"))
}

fn session_config_path(session_id: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!("xaft/sessions/{session_id}/config.toml"))
}

fn find_project_config(start: Option<&Path>) -> Option<PathBuf> {
    let start = start?;
    let mut current = start.to_path_buf();

    loop {
        // Flat `.xaft.toml` takes precedence over directory `.xaft/xaft.toml`
        let flat = current.join(".xaft.toml");
        if flat.exists() {
            return Some(flat);
        }
        let nested = current.join(".xaft/xaft.toml");
        if nested.exists() {
            return Some(nested);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config_toml() -> &'static str {
        r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#
    }

    #[test]
    fn load_defaults_with_no_files() {
        let cli = CliOverrides::default();
        // Use a temp dir as config_file to avoid picking up any real files
        let tmp = tempfile::tempdir().unwrap();
        let fake_config = tmp.path().join("xaft.toml");
        std::fs::write(&fake_config, minimal_config_toml()).unwrap();

        let cli_with_file = CliOverrides {
            config_file: Some(fake_config),
            ..Default::default()
        };

        let config = ConfigLoader::load(&cli_with_file).expect("load should succeed");
        assert!(!config.agent.is_empty());
    }

    #[test]
    fn load_file_parses_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, minimal_config_toml()).unwrap();

        let config = ConfigLoader::load_file(&path).unwrap();
        assert_eq!(config.core.log_level, LogLevel::Info);
        assert!(!config.core.telemetry);
    }

    #[test]
    fn load_file_interpolates_env_vars() {
        unsafe { std::env::set_var("XAFT_TEST_API", "test-key-123") }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(
            &path,
            r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "${XAFT_TEST_API}"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();

        let config = ConfigLoader::load_file(&path).unwrap();
        assert_eq!(
            config.provider.get("anthropic").unwrap().api_key,
            "test-key-123"
        );
        unsafe { std::env::remove_var("XAFT_TEST_API") }
    }

    #[test]
    fn cli_model_override_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, minimal_config_toml()).unwrap();

        let cli = CliOverrides {
            model: Some("claude-3-opus-20240229".to_string()),
            config_file: Some(path),
            ..Default::default()
        };

        let config = ConfigLoader::load(&cli).unwrap();
        assert_eq!(
            config.agent.get("default").unwrap().model,
            "claude-3-opus-20240229"
        );
    }

    #[test]
    fn env_model_override_applied() {
        unsafe { std::env::set_var("XAFT_MODEL", "test-model-env") }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, minimal_config_toml()).unwrap();

        let cli = CliOverrides {
            config_file: Some(path),
            ..Default::default()
        };
        let config = ConfigLoader::load(&cli).unwrap();
        assert_eq!(config.agent.get("default").unwrap().model, "test-model-env");
        unsafe { std::env::remove_var("XAFT_MODEL") }
    }

    #[test]
    fn auto_approve_disables_guardrails() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, minimal_config_toml()).unwrap();

        let cli = CliOverrides {
            auto_approve: true,
            config_file: Some(path),
            ..Default::default()
        };
        let config = ConfigLoader::load(&cli).unwrap();
        assert!(!config.guardrail.command_approval);
        assert!(!config.guardrail.file_destruction);
    }

    #[test]
    fn missing_file_returns_io_error() {
        let path = PathBuf::from("/nonexistent/path/xaft.toml");
        let err = ConfigLoader::load_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, "not valid toml }{[").unwrap();
        let err = ConfigLoader::load_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn project_config_merged_over_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(
            &path,
            r#"
[core]
log_level = "debug"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 5
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();

        let cli = CliOverrides {
            config_file: Some(path),
            ..Default::default()
        };
        let config = ConfigLoader::load(&cli).unwrap();
        assert_eq!(config.core.log_level, LogLevel::Debug);
        assert_eq!(config.agent.get("default").unwrap().max_turns, 5);
    }

    #[test]
    fn hierarchical_merge_project_over_global() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global.toml");
        let project = tmp.path().join("project.toml");

        std::fs::write(
            &global,
            r#"
[core]
log_level = "warn"
telemetry = true

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "global-model"
provider = "anthropic"
max_turns = 10
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();

        std::fs::write(
            &project,
            r#"
[core]
log_level = "debug"

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 20
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();

        // Load global first
        let global_cfg = ConfigLoader::load_file(&global).unwrap();
        let project_cfg = ConfigLoader::load_file(&project).unwrap();

        let mut merged = global_cfg;
        ConfigLoader::merge_layers(&mut merged, project_cfg).unwrap();

        // Project wins on log_level and max_turns
        assert_eq!(merged.core.log_level, LogLevel::Debug);
        assert_eq!(merged.agent.get("default").unwrap().max_turns, 20);
        // Global value preserved where project didn't override
        assert!(merged.core.telemetry);
    }
}

impl ConfigLoader {
    /// Merge `layer` into `base` (public for testing and external use).
    ///
    /// Uses deep merge semantics: objects merge recursively, scalars/arrays replace.
    pub fn merge_layers(base: &mut XaftConfig, layer: XaftConfig) -> Result<(), ConfigError> {
        Self::merge_into(base, layer)
    }
}

#[cfg(test)]
mod layering_tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The regression: merging fully-deserialized layers let a later file's
    /// defaults overwrite an earlier file's explicit values.
    #[test]
    fn a_later_layer_never_resets_a_key_it_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let global = write(tmp.path(), "global.toml", "[core]\nlog_level = \"debug\"\n");
        let project = write(
            tmp.path(),
            "project.toml",
            "[workflow]\ndefault_workflow = \"code_plan\"\n",
        );
        let mut merged = serde_json::to_value(XaftConfig::default()).unwrap();
        deep_merge(&mut merged, ConfigLoader::load_raw(&global).unwrap());
        deep_merge(&mut merged, ConfigLoader::load_raw(&project).unwrap());
        let config: XaftConfig = serde_json::from_value(merged).unwrap();
        assert_eq!(config.core.log_level, LogLevel::Debug);
        assert_eq!(
            config.workflow.default_workflow.as_deref(),
            Some("code_plan")
        );
    }

    #[test]
    fn extends_merges_parents_first_and_the_file_on_top() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "base.toml",
            "[core]\nlog_level = \"debug\"\n[workflow]\ndefault_workflow = \"standard\"\n",
        );
        write(
            tmp.path(),
            "team.toml",
            "[workflow]\ndefault_workflow = \"meta\"\n",
        );
        let child = write(
            tmp.path(),
            "child.toml",
            "extends = [\"base.toml\", \"team.toml\"]\n[workflow]\nmeta_max_parallel = 2\n",
        );
        let config = ConfigLoader::load_file(&child).unwrap();
        assert_eq!(config.core.log_level, LogLevel::Debug, "from base");
        assert_eq!(
            config.workflow.default_workflow.as_deref(),
            Some("meta"),
            "the later parent wins"
        );
        assert_eq!(
            config.workflow.meta_max_parallel, 2,
            "the file wins over parents"
        );
    }

    #[test]
    fn an_extends_cycle_is_an_error_naming_the_chain() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "a.toml", "extends = \"b.toml\"\n");
        let b = write(tmp.path(), "b.toml", "extends = \"a.toml\"\n");
        let err = ConfigLoader::load_file(&b).unwrap_err().to_string();
        assert!(err.contains("circular extends"), "{err}");
    }

    #[test]
    fn a_missing_extends_parent_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let child = write(tmp.path(), "child.toml", "extends = \"nope.toml\"\n");
        let err = ConfigLoader::load_file(&child).unwrap_err().to_string();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn xaft_config_is_the_explicit_file_unless_config_is_given() {
        let none = CliOverrides::default();
        assert_eq!(
            explicit_config_path(&none, Some(" /x/xaft.toml ".into())),
            Some(PathBuf::from("/x/xaft.toml"))
        );
        assert_eq!(explicit_config_path(&none, Some("  ".into())), None);
        let flag = CliOverrides {
            config_file: Some(PathBuf::from("/flag.toml")),
            ..Default::default()
        };
        assert_eq!(
            explicit_config_path(&flag, Some("/env.toml".into())),
            Some(PathBuf::from("/flag.toml"))
        );
    }

    #[test]
    fn an_explicit_config_replaces_project_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".xaft.toml",
            "[workflow]\ndefault_workflow = \"meta\"\n",
        );
        let explicit = write(
            tmp.path(),
            "explicit.toml",
            "[core]\nlog_level = \"warn\"\n",
        );
        let cli = CliOverrides {
            config_file: Some(explicit),
            project_dir: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let config = ConfigLoader::load(&cli).unwrap();
        assert_eq!(config.core.log_level, LogLevel::Warn);
        assert_eq!(
            config.workflow.default_workflow, None,
            "the discovered .xaft.toml is not merged when --config is given"
        );
    }

    #[test]
    fn set_overrides_parse_values_like_toml() {
        let mut config = XaftConfig::default();
        apply_set_overrides(
            &mut config,
            &[
                "core.log_level=debug".into(),
                "workflow.meta_max_parallel=7".into(),
                "workflow.default_workflow=code_plan".into(),
                "workflow.meta_allow_nesting=true".into(),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(config.core.log_level, LogLevel::Debug);
        assert_eq!(config.workflow.meta_max_parallel, 7);
        assert_eq!(
            config.workflow.default_workflow.as_deref(),
            Some("code_plan")
        );
        assert!(config.workflow.meta_allow_nesting);
    }

    #[test]
    fn an_unknown_set_key_is_an_error_not_a_silent_no_op() {
        let mut config = XaftConfig::default();
        let err = apply_set_overrides(&mut config, &["workflow.no_such_key=3".into()], &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown config key 'workflow.no_such_key'"),
            "{err}"
        );
        let err = apply_set_overrides(&mut config, &["noequals".into()], &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected section.key=value"), "{err}");
    }

    #[test]
    fn set_secret_reads_the_named_environment_variable() {
        let mut config = XaftConfig::default();
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        apply_set_overrides(&mut config, &[], &["workflow.default_workflow=HOME".into()]).unwrap();
        assert_eq!(
            config.workflow.default_workflow.as_deref(),
            Some(home.as_str())
        );
        let err = apply_set_overrides(
            &mut config,
            &[],
            &["workflow.default_workflow=XAFT_TEST_DEFINITELY_UNSET_9F2".into()],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("is not set"), "{err}");
    }
}
