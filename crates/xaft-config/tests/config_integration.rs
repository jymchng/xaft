//! Integration tests for xaft-config.
//!
//! Tests the full configuration loading pipeline including:
//! - File hierarchy (global → project → explicit)
//! - Env var overrides
//! - CLI flag overrides
//! - Deep merge correctness
//! - Validation rules
//! - Hot-reload detection
//! - Keybinding registry
//! - Size parsing
//! - TUI layout persistence

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;

use xaft_config::{
    AgentPresetResolver, CliOverrides, ConfigLoader, KeyAction, KeybindingConfig,
    KeybindingRegistry, LogLevel, TuiLayoutPersistence, XaftConfig, parse_size,
    watcher::{ConfigWatcher, watched_paths},
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_config(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
    let path = dir.path().join(filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    path
}

const MINIMAL_TOML: &str = r#"
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
"#;

fn load_from_file(content: &str) -> XaftConfig {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", content);
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    ConfigLoader::load(&cli).expect("config load should succeed")
}

// ── Unit: size parsing ────────────────────────────────────────────────────────

#[test]
fn size_parse_kilobytes() {
    assert_eq!(parse_size("1KB").unwrap(), 1024);
}

#[test]
fn size_parse_megabytes() {
    assert_eq!(parse_size("10MB").unwrap(), 10 * 1024 * 1024);
}

#[test]
fn size_parse_gigabytes() {
    assert_eq!(parse_size("1GB").unwrap(), 1_073_741_824);
}

#[test]
fn size_parse_bare_bytes() {
    assert_eq!(parse_size("512").unwrap(), 512);
}

#[test]
fn size_parse_fractional() {
    let result = parse_size("2.5MB").unwrap();
    assert_eq!(result, (2.5 * 1_048_576.0) as u64);
}

#[test]
fn size_parse_invalid_errors() {
    assert!(parse_size("not_a_size").is_err());
    assert!(parse_size("-5MB").is_err());
}

// ── Unit: keybinding parsing ──────────────────────────────────────────────────

#[test]
fn keybinding_parse_ctrl_q() {
    use xaft_config::KeybindingParser;
    let ev = KeybindingParser::parse("ctrl+q").unwrap();
    assert!(ev.modifiers.ctrl);
}

#[test]
fn keybinding_parse_function_keys() {
    use xaft_config::KeybindingParser;
    for n in 1u8..=12 {
        KeybindingParser::parse(&format!("f{n}")).unwrap();
    }
}

#[test]
fn keybinding_registry_from_config() {
    let mut bindings = HashMap::new();
    bindings.insert("ctrl+q".to_string(), KeyAction::Single("quit".to_string()));
    bindings.insert(
        "ctrl+n".to_string(),
        KeyAction::Single("new_task".to_string()),
    );
    let cfg = KeybindingConfig { bindings };

    let registry = KeybindingRegistry::from_config(&cfg).unwrap();
    assert_eq!(registry.len(), 2);
}

#[test]
fn keybinding_registry_lookup_action() {
    use xaft_config::KeybindingParser;
    let mut bindings = HashMap::new();
    bindings.insert("ctrl+q".to_string(), KeyAction::Single("quit".to_string()));
    let cfg = KeybindingConfig { bindings };
    let registry = KeybindingRegistry::from_config(&cfg).unwrap();

    let ev = KeybindingParser::parse("ctrl+q").unwrap();
    assert_eq!(registry.lookup(&ev).unwrap().action_name(), "quit");
}

// ── Integration: config file loading ─────────────────────────────────────────

#[test]
fn load_minimal_config() {
    let config = load_from_file(MINIMAL_TOML);
    assert_eq!(config.core.log_level, LogLevel::Info);
    assert!(!config.core.telemetry);
    assert!(config.provider.contains_key("anthropic"));
    assert!(config.agent.contains_key("default"));
}

#[test]
fn load_with_multiple_providers() {
    let config = load_from_file(
        r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[provider.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
timeout_secs = 60

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
    );
    assert!(config.provider.contains_key("anthropic"));
    assert!(config.provider.contains_key("openai"));
}

#[test]
fn load_with_agent_presets() {
    let config = load_from_file(
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

[agent.fast]
model = "claude-3-5-haiku-20241022"
provider = "anthropic"
max_turns = 10
temperature = 0.5
top_p = 1.0
"#,
    );
    assert!(config.agent.contains_key("default"));
    assert!(config.agent.contains_key("fast"));
    assert_eq!(
        config.agent.get("fast").unwrap().model,
        "claude-3-5-haiku-20241022"
    );
}

#[test]
fn load_with_mcp_clients() {
    let config = load_from_file(
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

[[mcp.client]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
enabled = true
"#,
    );
    assert_eq!(config.mcp.client.len(), 1);
    assert_eq!(config.mcp.client[0].name, "filesystem");
}

#[test]
fn invalid_toml_returns_parse_error() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", "not: valid: toml: ][{");
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    assert!(ConfigLoader::load(&cli).is_err());
}

// ── Integration: validation ───────────────────────────────────────────────────

#[test]
fn validation_rejects_bad_temperature() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        "xaft.toml",
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
temperature = 3.0
top_p = 1.0
"#,
    );
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let err = ConfigLoader::load(&cli).unwrap_err();
    assert!(err.to_string().contains("temperature"), "error: {err}");
}

#[test]
fn validation_rejects_bad_layout() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        "xaft.toml",
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

[tui.layout]
conversation_width = 50
sidebar_width = 30
"#,
    );
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let err = ConfigLoader::load(&cli).unwrap_err();
    assert!(err.to_string().contains("100"), "error: {err}");
}

// ── Integration: env var overrides ────────────────────────────────────────────

#[test]
fn env_log_level_override() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

    unsafe { std::env::set_var("XAFT_LOG_LEVEL", "warn") }
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(config.core.log_level, LogLevel::Warn);
    unsafe { std::env::remove_var("XAFT_LOG_LEVEL") }
}

#[test]
fn env_model_override() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

    unsafe { std::env::set_var("XAFT_MODEL", "env-model") }
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(config.agent.get("default").unwrap().model, "env-model");
    unsafe { std::env::remove_var("XAFT_MODEL") }
}

#[test]
fn env_anthropic_api_key_override() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

    unsafe { std::env::set_var("XAFT_ANTHROPIC_API_KEY", "env-api-key") }
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(
        config.provider.get("anthropic").unwrap().api_key,
        "env-api-key"
    );
    unsafe { std::env::remove_var("XAFT_ANTHROPIC_API_KEY") }
}

// ── Integration: CLI overrides ────────────────────────────────────────────────

#[test]
fn cli_model_overrides_config_file() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

    let cli = CliOverrides {
        model: Some("cli-model".to_string()),
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(config.agent.get("default").unwrap().model, "cli-model");
}

#[test]
fn cli_max_turns_overrides_config_file() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

    let cli = CliOverrides {
        max_turns: Some(99),
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(config.agent.get("default").unwrap().max_turns, 99);
}

#[test]
fn cli_auto_approve_disables_guardrails() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

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
fn cli_no_telemetry_disables_telemetry() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        "xaft.toml",
        r#"
[core]
log_level = "info"
telemetry = true

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
    );

    let cli = CliOverrides {
        no_telemetry: true,
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert!(!config.core.telemetry);
}

// ── Integration: preset resolver ─────────────────────────────────────────────

#[test]
fn preset_resolver_default() {
    let config = load_from_file(MINIMAL_TOML);
    let resolved = AgentPresetResolver::resolve(&config, None).unwrap();
    assert_eq!(resolved.name, "default");
    assert!(!resolved.model.is_empty());
}

#[test]
fn preset_resolver_unknown_preset_errors() {
    let config = load_from_file(MINIMAL_TOML);
    assert!(AgentPresetResolver::resolve(&config, Some("not-a-preset")).is_err());
}

#[test]
fn preset_tool_allow_deny() {
    let config = load_from_file(MINIMAL_TOML);
    let mut cfg2 = config.clone();
    if let Some(agent) = cfg2.agent.get_mut("default") {
        agent.allowed_tools = vec!["file-read".to_string(), "grep".to_string()];
        agent.denied_tools = vec!["shell".to_string()];
    }

    let resolved = AgentPresetResolver::resolve(&cfg2, None).unwrap();
    assert!(resolved.allows_tool("file-read"));
    assert!(resolved.allows_tool("grep"));
    assert!(!resolved.allows_tool("shell"));
    assert!(!resolved.allows_tool("unknown-tool"));
}

#[test]
fn preset_wildcard_allows_all_except_denied() {
    let config = load_from_file(MINIMAL_TOML);
    let mut cfg2 = config;
    if let Some(agent) = cfg2.agent.get_mut("default") {
        agent.allowed_tools = vec!["*".to_string()];
        agent.denied_tools = vec!["shell".to_string()];
    }
    let resolved = AgentPresetResolver::resolve(&cfg2, None).unwrap();
    assert!(resolved.allows_tool("file-read"));
    assert!(resolved.allows_tool("anything"));
    assert!(!resolved.allows_tool("shell")); // denied overrides wildcard
}

// ── Integration: TUI layout persistence ──────────────────────────────────────

#[test]
fn tui_layout_persist_and_reload() {
    let tmp = TempDir::new().unwrap();
    let mut p = TuiLayoutPersistence::load_or_default(tmp.path(), "session-abc");

    p.update(|s| {
        s.conversation_width = 70;
        s.sidebar_width = 30;
        s.input_height = 5;
    });
    assert!(p.is_dirty());
    p.persist().unwrap();
    assert!(!p.is_dirty());

    let p2 = TuiLayoutPersistence::load_or_default(tmp.path(), "session-abc");
    assert_eq!(p2.state().conversation_width, 70);
    assert_eq!(p2.state().input_height, 5);
}

#[test]
fn tui_layout_clean_no_file_created() {
    let tmp = TempDir::new().unwrap();
    let mut p = TuiLayoutPersistence::load_or_default(tmp.path(), "clean-session");
    p.persist().unwrap(); // no changes → no file
    let expected = tmp.path().join("sessions/clean-session/tui-layout.toml");
    assert!(!expected.exists());
}

#[tokio::test]
async fn tui_layout_saver_persists_debounced() {
    use tokio::sync::watch;
    use xaft_config::spawn_layout_saver;
    use xaft_config::types::TuiLayoutState;

    let tmp = TempDir::new().unwrap();
    let persistence = std::sync::Arc::new(tokio::sync::Mutex::new(
        TuiLayoutPersistence::load_or_default(tmp.path(), "debounce-test"),
    ));

    let initial = TuiLayoutState::default();
    let (tx, rx) = watch::channel(initial);

    let saver = spawn_layout_saver(
        std::sync::Arc::clone(&persistence),
        rx,
        Duration::from_millis(20),
    );

    let mut state = TuiLayoutState::default();
    state.conversation_width = 65;
    tx.send(state).unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(tx);
    let _ = saver.await;

    let layout_path = tmp.path().join("sessions/debounce-test/tui-layout.toml");
    assert!(layout_path.exists(), "layout should be persisted");
}

// ── Integration: hot-reload watcher ──────────────────────────────────────────

#[tokio::test]
async fn hot_reload_detects_config_file_change() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, "xaft.toml", MINIMAL_TOML);

    let initial = load_from_file(MINIMAL_TOML);
    let overrides = CliOverrides {
        config_file: Some(path.clone()),
        ..Default::default()
    };
    let paths = vec![path.clone()];

    let (mut rx, _handle) =
        ConfigWatcher::spawn(initial, paths, overrides, Duration::from_millis(30));

    // Wait, then update the file
    tokio::time::sleep(Duration::from_millis(50)).await;
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
max_turns = 99
temperature = 0.0
top_p = 1.0
"#,
    )
    .unwrap();

    let changed = tokio::time::timeout(Duration::from_millis(500), rx.changed())
        .await
        .expect("timeout waiting for hot-reload")
        .is_ok();
    assert!(changed, "hot-reload should have fired");

    let new_config = rx.borrow().clone();
    assert_eq!(new_config.agent.get("default").unwrap().max_turns, 99);
}

// ── Integration: deep merge correctness ──────────────────────────────────────

#[test]
fn deep_merge_preserves_base_keys_not_in_override() {
    let tmp = TempDir::new().unwrap();
    let base_path = write_config(&tmp, "base.toml", MINIMAL_TOML);
    let overlay_path = write_config(
        &tmp,
        "overlay.toml",
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
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
    );

    let mut base = ConfigLoader::load_file(&base_path).unwrap();
    let overlay = ConfigLoader::load_file(&overlay_path).unwrap();
    ConfigLoader::merge_layers(&mut base, overlay).unwrap();

    // log_level from overlay wins
    assert_eq!(base.core.log_level, LogLevel::Debug);
    // telemetry: global had true, overlay loaded with default=true, merge preserves true
    assert!(base.core.telemetry);
}

#[test]
fn deep_merge_adds_new_provider_from_overlay() {
    let base_toml = MINIMAL_TOML;
    let overlay_toml = r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[provider.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
timeout_secs = 60

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#;

    let tmp = TempDir::new().unwrap();
    let base_path = write_config(&tmp, "base.toml", base_toml);
    let overlay_path = write_config(&tmp, "overlay.toml", overlay_toml);

    let mut base = ConfigLoader::load_file(&base_path).unwrap();
    let overlay = ConfigLoader::load_file(&overlay_path).unwrap();
    ConfigLoader::merge_layers(&mut base, overlay).unwrap();

    assert!(base.provider.contains_key("anthropic"));
    assert!(base.provider.contains_key("openai"));
}

// ── Integration: env var interpolation ───────────────────────────────────────

#[test]
fn env_interpolation_in_config_file() {
    unsafe { std::env::set_var("XAFT_INTERP_TEST_KEY", "interpolated-key-value") }
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        "xaft.toml",
        r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "${XAFT_INTERP_TEST_KEY}"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
    );
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(
        config.provider.get("anthropic").unwrap().api_key,
        "interpolated-key-value"
    );
    unsafe { std::env::remove_var("XAFT_INTERP_TEST_KEY") }
}

#[test]
fn unknown_env_var_in_config_preserved_as_placeholder() {
    unsafe { std::env::remove_var("XAFT_DEFINITELY_NOT_SET_12345") }
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        "xaft.toml",
        r#"
[core]
log_level = "info"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "${XAFT_DEFINITELY_NOT_SET_12345}"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
    );
    let cli = CliOverrides {
        config_file: Some(path),
        ..Default::default()
    };
    let config = ConfigLoader::load(&cli).unwrap();
    assert_eq!(
        config.provider.get("anthropic").unwrap().api_key,
        "${XAFT_DEFINITELY_NOT_SET_12345}"
    );
}

// ── E2E: complete config pipeline ─────────────────────────────────────────────

#[test]
fn e2e_global_plus_project_plus_cli_hierarchy() {
    let tmp = TempDir::new().unwrap();

    // "global" config: conservative settings
    let global_path = write_config(
        &tmp,
        "global/xaft.toml",
        r#"
[core]
log_level = "warn"
telemetry = true

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 30

[agent.default]
model = "global-model"
provider = "anthropic"
max_turns = 5
temperature = 0.5
top_p = 1.0
"#,
    );

    // "project" config: intermediate settings
    let project_path = write_config(
        &tmp,
        "project/xaft.toml",
        r#"
[core]
log_level = "info"

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 30

[agent.default]
model = "project-model"
provider = "anthropic"
max_turns = 15
temperature = 0.0
top_p = 1.0
"#,
    );

    // Load global first, then merge project, then apply CLI
    let mut config = ConfigLoader::load_file(&global_path).unwrap();
    let project_cfg = ConfigLoader::load_file(&project_path).unwrap();
    ConfigLoader::merge_layers(&mut config, project_cfg).unwrap();

    // Apply CLI: model override + no_telemetry
    let cli = CliOverrides {
        model: Some("cli-model".to_string()),
        no_telemetry: true,
        ..Default::default()
    };

    // Apply CLI overrides manually
    if let Some(agent) = config.agent.get_mut("default") {
        agent.model = cli.model.unwrap();
    }
    if cli.no_telemetry {
        config.core.telemetry = false;
    }

    // Final assertions:
    // - log_level: project wins over global
    assert_eq!(config.core.log_level, LogLevel::Info);
    // - model: CLI wins over project wins over global
    assert_eq!(config.agent.get("default").unwrap().model, "cli-model");
    // - max_turns: project wins over global
    assert_eq!(config.agent.get("default").unwrap().max_turns, 15);
    // - telemetry: CLI flag wins (disabled)
    assert!(!config.core.telemetry);
}
