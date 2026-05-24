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
    pub fn load(cli: &CliOverrides) -> Result<XaftConfig, ConfigError> {
        // Step 1: Built-in defaults (zero allocation)
        let mut config = XaftConfig::default();
        debug!("Config: built-in defaults loaded");

        // Step 2: User global config
        let global_path = global_config_path();
        if let Some(ref path) = global_path {
            if path.exists() {
                let layer = Self::load_file(path)?;
                Self::merge_into(&mut config, layer)?;
                debug!("Config: merged user global config from {}", path.display());
            }
        }

        // Step 3: Project config (walk up from cwd / project_dir)
        let project_dir = cli
            .project_dir
            .clone()
            .or_else(|| std::env::current_dir().ok());

        if let Some(ref path) = find_project_config(project_dir.as_deref()) {
            let layer = Self::load_file(path)?;
            Self::merge_into(&mut config, layer)?;
            debug!("Config: merged project config from {}", path.display());
        }

        // Explicit --config file (replaces project discovery if given)
        if let Some(ref path) = cli.config_file {
            let layer = Self::load_file(path)?;
            Self::merge_into(&mut config, layer)?;
            debug!("Config: merged explicit config from {}", path.display());
        }

        // Step 4: Session overrides
        if let Some(ref session_id) = cli.session_id {
            let session_path = session_config_path(session_id);
            if session_path.exists() {
                let layer = Self::load_file(&session_path)?;
                Self::merge_into(&mut config, layer)?;
                debug!("Config: merged session config for {}", session_id);
            }
        }

        // Step 5: Environment variables
        apply_env_overrides(&mut config)?;
        debug!("Config: env var overrides applied");

        // Step 6: CLI flags
        apply_cli_overrides(&mut config, cli);
        debug!("Config: CLI overrides applied");

        // Step 7: Validate final config
        validate(&config)?;

        Ok(config)
    }

    /// Load a single TOML config file and parse it.
    ///
    /// Returns a partial `XaftConfig` — only keys present in the file are set.
    pub fn load_file(path: &Path) -> Result<XaftConfig, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Parse as TOML Value first so we can do interpolation
        let raw: toml::Value = toml::from_str(&content).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Convert to serde_json::Value for interpolation and merge
        let mut json_val: serde_json::Value = serde_json::to_value(&raw)?;

        // Expand ${ENV_VAR} references
        interpolate_strings(&mut json_val);

        // Deserialize into a partial XaftConfig
        // We merge on top of defaults so absent keys use the default value
        let partial: XaftConfig =
            serde_json::from_value(json_val.clone()).map_err(|e| ConfigError::Validation {
                section: format!("{}", path.display()),
                message: e.to_string(),
            })?;

        Ok(partial)
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
