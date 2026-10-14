//! Handler for `xaft config` subcommands.

use std::path::PathBuf;

use xaft_config::{CliOverrides, ConfigLoader, ProviderConfig, XaftConfig};
use xaft_runtime::ExitCode;

use crate::args::{
    ConfigInitArgs, ConfigProfilesArgs, ConfigShowArgs, ConfigSubcommand, ConfigValidateArgs,
    OutputFormat,
};
use crate::error::XaftError;

/// agenthicc prints this when the effective config declares no provider
/// profiles.
///
/// Note this is close to unreachable in xaft: `XaftConfig::default()` seeds
/// `anthropic` and `openai`, and the loader always starts from the defaults, so
/// a user who deletes every `[provider.*]` table still gets the built-in two
/// back. The branch is kept because agenthicc has the same output and because
/// a user-supplied config that somehow yields an empty table should not print
/// an empty listing with exit 0.
const NO_PROFILES: &str = "No provider profiles configured.";

/// agenthicc's stand-in for a profile field the user left unset. Reused for
/// xaft's endpoint column verbatim so the token stays recognisable to anyone
/// coming from agenthicc.
const PROVIDER_DEFAULT: &str = "<provider default>";

/// Execute `xaft config <subcommand>`.
pub async fn handle_config(subcommand: &ConfigSubcommand) -> Result<ExitCode, XaftError> {
    match subcommand {
        ConfigSubcommand::Show(args) => handle_config_show(args).await,
        ConfigSubcommand::Init(args) => handle_config_init(args).await,
        ConfigSubcommand::Validate(args) => handle_config_validate(args).await,
        ConfigSubcommand::Presets => handle_config_presets().await,
        ConfigSubcommand::Profiles(args) => handle_config_profiles(args).await,
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
        OutputFormat::Json | OutputFormat::Plain | OutputFormat::Toml => {
            println!("{}", render_serialized_config(&config, args.format));
        }
        OutputFormat::Pretty | OutputFormat::Table => {
            print_config_pretty(&config);
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Render the machine-readable forms of `xaft config show`.
///
/// Factored out of [`handle_config_show`] so the redaction contract can be
/// asserted against the exact bytes that reach stdout, without capturing the
/// process's own stdout -- the same reason `xaft mcp` has `render_mcp`. It is
/// also the single definition of what gets serialized.
///
/// The rule it enforces: the *only* thing handed to a serializer is
/// [`XaftConfig::redacted`]. Passing `&config` is the historical defect --
/// `show --format json` and `show --format toml` printed
/// `provider.<name>.api_key`, `provider.<name>.headers` values,
/// `mcp.client[].token` and `tools.mcp_servers[].token` verbatim, with any
/// literal pasted into `env_headers` alongside them. agenthicc never printed
/// the raw config: its `config show` serializes `Config.redacted_dict()`.
///
/// An empty string is returned if a serializer fails. That is the pre-existing
/// behaviour of the `unwrap_or_default()` calls this replaced, and it is the
/// safe direction: a serialization failure the redactor introduced must not
/// degrade into printing the unredacted document.
fn render_serialized_config(config: &XaftConfig, format: OutputFormat) -> String {
    let redacted = config.redacted();
    match format {
        // TOML has no null and rejects a JSON `null` outright, so the unset
        // members the JSON arm keeps as `null` are dropped here. Without this
        // the serializer errors and the `unwrap_or_default()` below prints
        // nothing at all -- `--format toml` silently becoming a no-op.
        OutputFormat::Toml => {
            toml::to_string_pretty(&xaft_config::without_nulls(redacted)).unwrap_or_default()
        }
        _ => serde_json::to_string_pretty(&redacted).unwrap_or_default(),
    }
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

    println!(
        "  \x1b[32m✓\x1b[0m Created config file: {}",
        target_path.display()
    );
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

/// `xaft config profiles` — list provider profiles and non-secret connection
/// metadata.
///
/// Mirrors `agenthicc config profiles`, which prints one tab-separated line per
/// entry of `providers.*`, sorted by name:
///
/// ```text
/// <name>\t<provider>\t<model>\t<endpoint>\t<capabilities>
/// ```
///
/// Two deliberate divergences, both because xaft's model of a profile is
/// genuinely different rather than an unimplemented subset:
///
/// * **Model column.** agenthicc's `ProviderProfile.model` is a single default
///   model string, shown with a `<provider default>` fallback when unset.
///   xaft's [`ProviderConfig`] has no such field: the model comes from the
///   referring agent preset, and a profile only carries an optional `models`
///   alias table (`short_name -> full_model_id`). The column therefore shows
///   that alias table, sorted and rendered `short=full`, reusing agenthicc's
///   own `<provider default>` token when a profile declares no aliases.
/// * **Endpoint column has no fallback.** agenthicc prints
///   `<provider default>` for an unset `base_url`. xaft does not: the config
///   validator rejects an empty `base_url` outright (`validate.rs`, "base_url
///   must not be empty"), and `ConfigLoader::load` runs that validator, so a
///   profile that reaches this handler always has a real endpoint. Printing a
///   placeholder for a state the loader cannot produce would be dead output.
/// * **Capabilities column dropped.** agenthicc's `capabilities` is a
///   per-profile feature-toggle table. xaft has no equivalent on
///   `ProviderConfig`, and emitting a constant `-` would imply a
///   `providers.<name>.capabilities` TOML key that xaft does not read. It
///   should be added here if xaft's config ever grows that key.
///
/// The output is otherwise non-secret by construction: it never touches
/// `api_key` / `api_key_env` resolution.
async fn handle_config_profiles(args: &ConfigProfilesArgs) -> Result<ExitCode, XaftError> {
    let overrides = CliOverrides {
        config_file: args.config.clone(),
        ..Default::default()
    };
    let config = ConfigLoader::load(&overrides)?;

    let rows = profile_rows(&config);
    if rows.is_empty() {
        println!("{NO_PROFILES}");
        return Ok(ExitCode::SUCCESS);
    }
    for row in rows {
        println!("{row}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Render one tab-separated row per provider profile, sorted by name.
///
/// Kept separate from [`handle_config_profiles`] so the column contract can be
/// asserted in unit tests without capturing stdout, and so the rendering has a
/// single definition the handler and the tests both go through.
fn profile_rows(config: &XaftConfig) -> Vec<String> {
    let mut names: Vec<_> = config.provider.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let profile = &config.provider[name];
            format!(
                "{}\t{}\t{}\t{}",
                name,
                profile.provider_type,
                model_column(profile),
                profile.base_url,
            )
        })
        .collect()
}

/// The model column: the profile's `models` alias table, sorted, as
/// `short=full` pairs joined by commas. An empty table means the profile
/// declared no aliases, so agenthicc's `<provider default>` stands -- the model
/// is whatever the referring agent preset asks for.
///
/// The pairs are sorted on the rendered `short=full` string rather than on the
/// key alone so the order is stable and obvious in the output. A `BTreeMap` is
/// not used because the source is a `HashMap`, whose iteration order is
/// randomised per process -- an unstable column order would make the command
/// useless for `sort` and `diff`.
fn model_column(profile: &ProviderConfig) -> String {
    if profile.models.is_empty() {
        return PROVIDER_DEFAULT.to_string();
    }
    let mut pairs: Vec<String> = profile
        .models
        .iter()
        .map(|(short, full)| format!("{short}={full}"))
        .collect();
    pairs.sort();
    pairs.join(",")
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
        let exists = if path.exists() {
            " \x1b[32m(found)\x1b[0m"
        } else {
            " \x1b[90m(not found)\x1b[0m"
        };
        println!("    5. User global:   {}{}", path.display(), exists);
    }

    let project = PathBuf::from(".xaft/xaft.toml");
    let exists = if project.exists() {
        " \x1b[32m(found)\x1b[0m"
    } else {
        " \x1b[90m(not found)\x1b[0m"
    };
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
        println!(
            "    {name}: {:?} at {} {key_status}",
            p.provider_type, p.base_url
        );
    }
    println!();

    println!("  \x1b[1mAgent Presets:\x1b[0m");
    let mut agent_names: Vec<_> = config.agent.keys().collect();
    agent_names.sort();
    for name in agent_names {
        let a = &config.agent[name];
        println!(
            "    {name}: {} via {} (turns: {}, temp: {})",
            a.model, a.provider, a.max_turns, a.temperature
        );
    }
    println!();

    println!("  \x1b[1mTools:\x1b[0m");
    let mut tool_names: Vec<_> = config.tool.keys().collect();
    tool_names.sort();
    for name in tool_names {
        let t = &config.tool[name];
        let status = if t.enabled {
            "\x1b[32menabled\x1b[0m"
        } else {
            "\x1b[90mdisabled\x1b[0m"
        };
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
    use crate::args::{
        ConfigInitArgs, ConfigProfilesArgs, ConfigShowArgs, ConfigSubcommand, ConfigValidateArgs,
        OutputFormat,
    };
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

    // ── config show redaction ────────────────────────────────────────────────
    //
    // `config show --format json|toml` used to serialize the whole `XaftConfig`
    // and print every credential the loader had just read. These tests are the
    // guard: they run the loaded config through the same renderer the handler
    // uses, so re-introducing `&config` in `render_serialized_config` turns them
    // red. The canaries are deliberately *not* credential-shaped -- a
    // realistic-looking key would be masked by the test harness's own output
    // filter and the tests would pass without the redactor running.

    /// Every canary, one per leak site, in a file loaded the way the CLI loads
    /// it (through `ConfigLoader`, not by parsing TOML in the test).
    const PLANTED_CONFIG: &str = r#"
[core]
log_level = "info"

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "CANARYPROVIDERKEY"

[[mcp.client]]
name = "planted"
transport = "stdio"
command = "npx"
token = "CANARYMCPTOKEN"
env_headers = { X-Api-Key = "canary-mcp-header" }

[[tools.mcp_servers]]
name = "remote"
transport = "http"
url = "https://mcp.example.invalid/mcp"
token = "CANARYTOOLSTOKEN"
"#;

    fn planted_config(dir: &TempDir) -> XaftConfig {
        let path = dir.path().join("xaft.toml");
        std::fs::write(&path, PLANTED_CONFIG).unwrap();
        ConfigLoader::load(&CliOverrides {
            config_file: Some(path),
            ..Default::default()
        })
        .expect("the planted config must load")
    }

    /// Assert no canary survived *and* that something was masked, so an empty
    /// rendering cannot satisfy the first half by accident.
    fn assert_redacted(out: &str) {
        for canary in [
            "CANARYPROVIDERKEY",
            "CANARYMCPTOKEN",
            "canary-mcp-header",
            "CANARYTOOLSTOKEN",
        ] {
            assert!(!out.contains(canary), "{canary} reached the output: {out}");
        }
        assert!(
            out.contains("<redacted>"),
            "nothing was masked, so the absence assertions prove nothing: {out}"
        );
        // The config must still be recognisable -- a blank document would be
        // "safe and useless".
        assert!(out.contains("https://api.anthropic.com"));
    }

    #[tokio::test]
    async fn config_show_json_never_prints_a_credential() {
        let tmp = TempDir::new().unwrap();
        let config = planted_config(&tmp);
        assert_redacted(&render_serialized_config(&config, OutputFormat::Json));
    }

    #[tokio::test]
    async fn config_show_toml_never_prints_a_credential() {
        let tmp = TempDir::new().unwrap();
        let config = planted_config(&tmp);
        let out = render_serialized_config(&config, OutputFormat::Toml);
        // `unwrap_or_default()` turns a serialization failure into an empty
        // string, which would make `!contains(canary)` trivially true -- so pin
        // that the TOML actually rendered.
        assert!(!out.is_empty(), "TOML serialization failed (empty output)");
        assert_redacted(&out);
    }

    #[tokio::test]
    async fn config_show_plain_uses_the_redacted_json() {
        // `--format plain` shares the JSON arm, so it shares the redaction.
        let tmp = TempDir::new().unwrap();
        let config = planted_config(&tmp);
        assert_redacted(&render_serialized_config(&config, OutputFormat::Plain));
    }

    #[tokio::test]
    async fn config_show_keeps_a_conforming_env_header_variable_name() {
        // The counterpart to the masking: `Authorization = "MCP_TOKEN"` is the
        // information `config show` exists to print, and blanking it would be a
        // different bug.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(
            &path,
            r#"
[[mcp.client]]
name = "named"
transport = "stdio"
command = "npx"
env_headers = { Authorization = "MCP_TOKEN" }
"#,
        )
        .unwrap();
        let config = ConfigLoader::load(&CliOverrides {
            config_file: Some(path),
            ..Default::default()
        })
        .unwrap();
        let out = render_serialized_config(&config, OutputFormat::Json);
        assert!(
            out.contains("MCP_TOKEN"),
            "the variable name was dropped: {out}"
        );
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

    // ── config profiles ──────────────────────────────────────────────────
    //
    // The rows are asserted through `profile_rows` rather than by capturing
    // stdout: that function is the single definition of the column layout, so
    // testing it directly pins the contract the handler prints.

    /// Build a config straight from TOML so the test never touches the real
    /// `$HOME`, a project `.xaft/`, or any `XAFT_*` env var the loader reads.
    fn config_from_toml(toml_src: &str) -> XaftConfig {
        toml::from_str(toml_src).expect("valid config TOML")
    }

    #[test]
    fn profile_rows_are_tab_separated_and_sorted_by_name() {
        let config = config_from_toml(
            r#"
[provider.ollama]
type = "openai-compatible"
base_url = "http://localhost:11434/v1"

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
"#,
        );
        let rows = profile_rows(&config);
        assert_eq!(
            rows,
            vec![
                // Sorted by name, so `anthropic` precedes `ollama` even though
                // `ollama` was written first in the TOML.
                "anthropic\tanthropic\t<provider default>\thttps://api.anthropic.com".to_string(),
                "ollama\topenai-compatible\t<provider default>\thttp://localhost:11434/v1"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn model_column_renders_the_alias_table_sorted() {
        let config = config_from_toml(
            r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"

[provider.anthropic.models]
opus = "claude-opus-4-20250514"
sonnet = "claude-3-5-sonnet-20241022"
haiku = "claude-3-5-haiku-20241022"
"#,
        );
        let rows = profile_rows(&config);
        assert_eq!(rows.len(), 1);
        let cols: Vec<&str> = rows[0].split('\t').collect();
        assert_eq!(cols[0], "anthropic");
        assert_eq!(cols[1], "anthropic");
        // Sorted by the rendered `short=full` pair, not by declaration order,
        // because the source is a HashMap with randomised iteration.
        assert_eq!(
            cols[2],
            "haiku=claude-3-5-haiku-20241022,\
             opus=claude-opus-4-20250514,\
             sonnet=claude-3-5-sonnet-20241022"
                .replace("             ", "")
        );
        assert_eq!(cols[3], "https://api.anthropic.com");
    }

    #[test]
    fn model_column_falls_back_to_the_provider_default_token() {
        // A profile with no `[provider.x.models]` table: the model column must
        // use agenthicc's own placeholder rather than rendering an empty
        // field, and the endpoint column must still show the real base_url.
        let config = config_from_toml(
            r#"
[provider.custom]
type = "openai-compatible"
base_url = "https://example.invalid/v1"
"#,
        );
        let rows = profile_rows(&config);
        assert_eq!(
            rows,
            vec!["custom\topenai-compatible\t<provider default>\thttps://example.invalid/v1"]
        );
    }

    #[test]
    fn a_profile_with_an_empty_base_url_never_reaches_the_cli() {
        // Documents WHY the endpoint column has no `<provider default>`
        // fallback: the loader's validator rejects this config outright, so
        // `handle_config_profiles` can never be handed such a profile. Parsing
        // alone is enough to produce one, which is exactly why the CLI must
        // not pretend to handle it.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, "[provider.bare]\ntype = \"openai\"\n").unwrap();

        let err = ConfigLoader::load(&CliOverrides {
            config_file: Some(path),
            ..Default::default()
        })
        .expect_err("empty base_url must be rejected by validation");
        assert!(
            err.to_string().contains("base_url must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn profile_rows_are_empty_only_when_no_provider_is_declared() {
        // `XaftConfig::default()` seeds `anthropic` and `openai`, so parsing an
        // empty TOML document is NOT enough to reach the empty case -- the
        // built-in defaults fill the provider table in. The table has to be
        // cleared explicitly, which is also the only way the `No provider
        // profiles configured.` branch is reachable today.
        let mut config = config_from_toml("");
        assert_eq!(
            config.provider.len(),
            2,
            "defaults should seed two providers"
        );
        config.provider.clear();
        assert!(profile_rows(&config).is_empty());
    }

    #[tokio::test]
    async fn config_profiles_succeeds_for_a_real_config() {
        let tmp = TempDir::new().unwrap();
        let path = write_minimal_config(&tmp);
        let args = ConfigProfilesArgs { config: Some(path) };
        let code = handle_config_profiles(&args).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn config_profiles_surfaces_a_broken_config() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("xaft.toml");
        std::fs::write(&path, "invalid toml {[").unwrap();
        let args = ConfigProfilesArgs { config: Some(path) };
        assert!(handle_config_profiles(&args).await.is_err());
    }

    #[test]
    fn default_config_template_is_valid_toml() {
        let template = default_config_template();
        let _: toml::Value = toml::from_str(template).expect("template should be valid TOML");
    }
}
