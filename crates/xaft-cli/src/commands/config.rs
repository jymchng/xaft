//! Handler for `xaft config` subcommands.

use std::path::PathBuf;

use xaft_config::{CliOverrides, ConfigLoader, XaftConfig};
use xaft_runtime::ExitCode;

use crate::args::{ConfigInitArgs, ConfigShowArgs, ConfigSubcommand, ConfigValidateArgs, OutputFormat};
use crate::error::XaftError;

/// Execute `xaft config <subcommand>`.
pub async fn handle_config(subcommand: &ConfigSubcommand) -> Result<ExitCode, XaftError> {
    match subcommand {
        ConfigSubcommand::Show(args) => handle_config_show(args).await,
        ConfigSubcommand::Init(args) => handle_config_init(args).await,
        ConfigSubcommand::Validate(args) => handle_config_validate(args).await,
        ConfigSubcommand::Presets => handle_config_presets().await,
    }
}

async fn handle_config_show(args: &ConfigShowArgs) -> Result<ExitCode, XaftError> {
    let overrides = CliOverrides {
        config_file: args.config.clone(),
        ..Default::default()
    };
    let config = ConfigLoader::load(&overrides)?;

    if args.show_paths {
        print_config_paths();
    }

    match args.format {
        OutputFormat::Json | OutputFormat::Plain => {
            let json = serde_json::to_string_pretty(&config).unwrap_or_default();
            println!("{json}");
        }
        OutputFormat::Toml => {
            let toml = toml::to_string_pretty(&config).unwrap_or_default();
            println!("{toml}");
        }
        OutputFormat::Pretty | OutputFormat::Table => {
            print_config_pretty(&config);
        }
    }

    Ok(ExitCode::SUCCESS)
}

async fn handle_config_init(args: &ConfigInitArgs) -> Result<ExitCode, XaftError> {
    let target_path = resolve_config_init_path(args)?;

    if target_path.exists() && !args.force {
        return Err(XaftError::from(xaft_config::ConfigError::AlreadyExists {
            path: target_path,
        }));
    }

    // Create parent directory
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write the default config template
    let template = default_config_template();
    std::fs::write(&target_path, template)?;

    println!("  \x1b[32m✓\x1b[0m Created config file: {}", target_path.display());
    println!();
    println!("  Edit the file to configure your API keys and preferences.");
    println!("  Run `xaft config show` to verify the resolved configuration.");
    println!();

    Ok(ExitCode::SUCCESS)
}

async fn handle_config_validate(args: &ConfigValidateArgs) -> Result<ExitCode, XaftError> {
    let overrides = CliOverrides {
        config_file: args.config.clone(),
        ..Default::default()
    };

    match ConfigLoader::load(&overrides) {
        Ok(_) => {
            println!("  \x1b[32m✓\x1b[0m Configuration is valid.");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("  \x1b[31m✗\x1b[0m Configuration error: {e}");
            Err(XaftError::Config(e))
        }
    }
}

async fn handle_config_presets() -> Result<ExitCode, XaftError> {
    let config = ConfigLoader::load(&CliOverrides::default())?;

    println!("  \x1b[1mAvailable agent presets:\x1b[0m");
    println!();

    let mut names: Vec<_> = config.agent.keys().collect();
    names.sort();

    for name in names {
        let preset = &config.agent[name];
        println!("  \x1b[32m{name}\x1b[0m");
        println!("    model:     {}", preset.model);
        println!("    provider:  {}", preset.provider);
        println!("    max_turns: {}", preset.max_turns);
        println!();
    }

    Ok(ExitCode::SUCCESS)
}

fn resolve_config_init_path(args: &ConfigInitArgs) -> Result<PathBuf, XaftError> {
    if let Some(ref path) = args.output {
        return Ok(path.clone());
    }

    if args.global {
        dirs::config_dir()
            .map(|d| d.join("xaft/xaft.toml"))
            .ok_or_else(|| XaftError::Usage("could not determine user config directory".into()))
    } else {
        Ok(PathBuf::from(".xaft/xaft.toml"))
    }
}

fn print_config_paths() {
    println!("  \x1b[1mConfig file search order:\x1b[0m");

    if let Some(global) = dirs::config_dir() {
        let path = global.join("xaft/xaft.toml");
        let exists = if path.exists() { " \x1b[32m(found)\x1b[0m" } else { " \x1b[90m(not found)\x1b[0m" };
        println!("    5. User global:   {}{}", path.display(), exists);
    }

    let project = PathBuf::from(".xaft/xaft.toml");
    let exists = if project.exists() { " \x1b[32m(found)\x1b[0m" } else { " \x1b[90m(not found)\x1b[0m" };
    println!("    4. Project:       {}{}", project.display(), exists);
    println!("    3. Env vars:      XAFT_* prefix");
    println!("    2. CLI flags:     --model, --provider, ...");
    println!("    1. Default:       built-in defaults");
    println!();
}

fn print_config_pretty(config: &XaftConfig) {
    println!("  \x1b[1mCore:\x1b[0m");
    println!("    log_level:  {}", config.core.log_level);
    println!("    data_dir:   {}", config.core.data_dir.display());
    println!("    telemetry:  {}", config.core.telemetry);
    println!();

    println!("  \x1b[1mProviders:\x1b[0m");
    let mut provider_names: Vec<_> = config.provider.keys().collect();
    provider_names.sort();
    for name in provider_names {
        let p = &config.provider[name];
        let key_status = if !p.api_key.is_empty() {
            "\x1b[32m(set)\x1b[0m"
        } else {
            "\x1b[33m(not set)\x1b[0m"
        };
        println!("    {name}: {:?} at {} {key_status}", p.provider_type, p.base_url);
    }
    println!();

    println!("  \x1b[1mAgent Presets:\x1b[0m");
    let mut agent_names: Vec<_> = config.agent.keys().collect();
    agent_names.sort();
    for name in agent_names {
        let a = &config.agent[name];
        println!("    {name}: {} via {} (turns: {}, temp: {})", a.model, a.provider, a.max_turns, a.temperature);
    }
    println!();

    println!("  \x1b[1mTools:\x1b[0m");
    let mut tool_names: Vec<_> = config.tool.keys().collect();
    tool_names.sort();
    for name in tool_names {
        let t = &config.tool[name];
        let status = if t.enabled { "\x1b[32menabled\x1b[0m" } else { "\x1b[90mdisabled\x1b[0m" };
        println!("    {name}: {status}");
    }
    println!();
}

fn default_config_template() -> &'static str {
    r#"# xaft configuration file
# Documentation: https://github.com/xaft/xaft
# Run `xaft config show` to see all resolved values.

[core]
log_level = "info"
telemetry = true

# ── Providers ─────────────────────────────────────────────────────────────────
# Set your API key via environment variable (recommended):
#   export XAFT_ANTHROPIC_API_KEY=your-key
#
# Or set it directly here (not recommended for shared configs):
#   api_key = "your-key"

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
max_retries = 3
timeout_secs = 120

# [provider.openai]
# type = "openai"
# base_url = "https://api.openai.com/v1"
# max_retries = 3
# timeout_secs = 120

# ── Agent Presets ──────────────────────────────────────────────────────────────

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
allowed_tools = ["*"]

# ── Safety ────────────────────────────────────────────────────────────────────

[guardrail]
file_destruction = true
secret_leakage = true
cost_limit = true
command_approval = false

# Cost limit settings (only used when guardrail.cost_limit = true)
[guardrail.cost_limit_config]
max_spend = 10.0
warn_at_percent = 80
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{ConfigInitArgs, ConfigShowArgs, ConfigSubcommand, ConfigValidateArgs, OutputFormat};
    use tempfile::TempDir;

    fn write_minimal_config(dir: &TempDir) -> PathBuf {
        let path = dir.path().join("xaft.toml");
        std::fs::write(
            &path,
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
"#,
        )
        .unwrap();
        path
    }

    #[tokio::test]
    async fn config_show_pretty() {
        let tmp = TempDir::new().unwrap();
        let path = write_minimal_config(&tmp);
        let args = ConfigShowArgs {
            format: OutputFormat::Pretty,
            show_paths: false,
            config: Some(path),
        };
        let code = handle_config_show(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn config_show_json() {
        let tmp = TempDir::new().unwrap();
        let path = write_minimal_config(&tmp);
        let args = ConfigShowArgs {
            format: OutputFormat::Json,
            show_paths: false,
            config: Some(path),
        };
        let code = handle_config_show(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn config_init_creates_file() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("xaft.toml");
        let args = ConfigInitArgs {
            global: false,
            force: false,
            output: Some(out.clone()),
        };
        let code = handle_config_init(&args).await.unwrap();
        assert!(code.is_success());
        assert!(out.exists());
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("[core]"));
    }

    #[tokio::test]
    async fn config_init_fails_if_exists_without_force() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("xaft.toml");
        std::fs::write(&out, "existing").unwrap();
        let args = ConfigInitArgs {
            global: false,
            force: false,
            output: Some(out),
        };
        assert!(handle_config_init(&args).await.is_err());
    }

    #[tokio::test]
    async fn config_init_overwrites_with_force() {
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("xaft.toml");
        std::fs::write(&out, "existing").unwrap();
        let args = ConfigInitArgs {
            global: false,
            force: true,
            output: Some(out.clone()),
        };
        let code = handle_config_init(&args).await.unwrap();
        assert!(code.is_success());
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("[core]"));
    }

    #[tokio::test]
    async fn config_validate_valid() {
        let tmp = TempDir::new().unwrap();
        let path = write_minimal_config(&tmp);
        let args = ConfigValidateArgs { config: Some(path) };
        let code = handle_config_validate(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn config_validate_invalid() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, "invalid toml {[").unwrap();
        let args = ConfigValidateArgs { config: Some(path) };
        assert!(handle_config_validate(&args).await.is_err());
    }

    #[test]
    fn default_config_template_is_valid_toml() {
        let template = default_config_template();
        let _: toml::Value = toml::from_str(template).expect("template should be valid TOML");
    }
}
