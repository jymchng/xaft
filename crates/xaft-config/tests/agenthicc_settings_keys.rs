//! Integration tests for the Execution / Tools / Memory / Behaviour parity keys.
//!
//! Parity targets in agenthicc's `config.py`:
//!   - `ExecutionSettings.authoring_max_generation_attempts`
//!   - `ExecutionSettings.authoring_max_phase_turns`
//!   - `ExecutionSettings.profile`
//!   - `ToolSettings.plugins`, `max_live_tool_calls`, `group_exploratory_calls`,
//!     `http_timeout_s`, `question_timeout_s`
//!   - `MemorySettings.vector_db`, `session_ttl_seconds`
//!   - `BehaviourSettings.verbose`, `confirm_exits`
//!
//! The property these share, and the one worth pinning, is that
//! `Default::default()` and a config file that merely *omits* a key agree.
//! Both build the same struct, but one goes through serde's per-field
//! `#[serde(default = ...)]` helpers and the other through the hand-written
//! `Default` impl. When those two diverge, a fresh process and a loaded config
//! file silently behave differently -- which is the bug class that already bit
//! `PluginConfig` once in this crate.
//!
//! Every test therefore asserts the *pair*, not one side of it.

use tempfile::TempDir;
use xaft_config::{ConfigLoader, MemoryConfig, ToolSettingsConfig, XaftConfig};

fn load(toml_text: &str) -> Result<XaftConfig, String> {
    let dir = TempDir::new().map_err(|e| e.to_string())?;
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, toml_text).map_err(|e| e.to_string())?;
    ConfigLoader::load_file(&path).map_err(|e| e.to_string())
}

/// Load and validate. The built-in agent presets reference an `anthropic`
/// provider and `validate` checks agents before these sections, so the fixture
/// supplies one — otherwise every assertion below would stop on the *agent*
/// cross-reference instead of reaching the key under test.
fn load_validated(toml_text: &str) -> Result<XaftConfig, String> {
    let with_presets = format!(
        r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
{toml_text}"#
    );
    let config = load(&with_presets)?;
    xaft_config::validate(&config).map_err(|e| e.to_string())?;
    Ok(config)
}

fn minimal() -> &'static str {
    ""
}

// ── ToolSettingsConfig defaults ──────────────────────────────────────────────

#[test]
fn tool_settings_defaults_match_agenthicc() {
    let tools = ToolSettingsConfig::default();
    assert_eq!(tools.max_live_tool_calls, 5, "agenthicc's default");
    assert!(tools.group_exploratory_calls, "agenthicc's default");
    assert_eq!(tools.http_timeout_s, 30.0, "agenthicc's default");
    assert_eq!(
        tools.question_timeout_s, 300.0,
        "agenthicc's DEFAULT_QUESTION_TIMEOUT_S"
    );
    assert!(tools.plugins.is_empty());
}

#[test]
fn an_omitted_tools_table_produces_the_same_values_as_default() {
    // The divergence this whole file guards against: a derived `Default` would
    // give `max_live_tool_calls = 0` and `question_timeout_s = 0.0`, while a
    // deserialized config would get 5 and 300.0.
    let from_default = ToolSettingsConfig::default();
    let from_file: ToolSettingsConfig =
        toml::from_str(minimal()).expect("an absent table still deserializes");

    assert_eq!(
        from_file.max_live_tool_calls,
        from_default.max_live_tool_calls
    );
    assert_eq!(
        from_file.question_timeout_s,
        from_default.question_timeout_s
    );
    assert_eq!(from_file.http_timeout_s, from_default.http_timeout_s);
    assert_eq!(
        from_file.group_exploratory_calls,
        from_default.group_exploratory_calls
    );
}

#[test]
fn a_loaded_config_file_agrees_with_the_default() {
    let config = load_validated(minimal()).expect("load");
    let tools = &config.tools;
    assert_eq!(tools.max_live_tool_calls, 5);
    assert_eq!(tools.question_timeout_s, 300.0);
    assert_eq!(tools.http_timeout_s, 30.0);
    assert!(tools.group_exploratory_calls);
}

#[test]
fn every_tool_settings_key_loads_from_a_file() {
    let config = load_validated(
        r#"
[tools]
plugins = ["my-plugin", "other"]
max_live_tool_calls = 12
group_exploratory_calls = false
http_timeout_s = 45.0
question_timeout_s = 90.0
"#,
    )
    .expect("load");

    assert_eq!(config.tools.plugins, ["my-plugin", "other"]);
    assert_eq!(config.tools.max_live_tool_calls, 12);
    assert!(!config.tools.group_exploratory_calls);
    assert_eq!(config.tools.http_timeout_s, 45.0);
    assert_eq!(config.tools.question_timeout_s, 90.0);
}

#[test]
fn a_non_positive_question_timeout_is_rejected() {
    // A question that times out immediately denies the agent any chance to
    // answer, so this is refused rather than honoured.
    for bad in ["0.0", "-1.0"] {
        let error = load_validated(&format!("[tools]\nquestion_timeout_s = {bad}\n"))
            .expect_err("a non-positive question timeout must be rejected");
        assert!(error.contains("question_timeout_s"), "{error}");
    }
}

#[test]
fn a_zero_http_timeout_means_disabled_and_is_accepted() {
    // 0.0 is the documented "no timeout" value, so only a *negative* is an
    // error. Asserting the positive half matters as much as the negative: a
    // future "reject zero" change would silently make this config unloadable.
    let config = load_validated("[tools]\nhttp_timeout_s = 0.0\n").expect("0.0 is valid");
    assert_eq!(config.tools.http_timeout_s, 0.0);

    let error = load_validated("[tools]\nhttp_timeout_s = -0.5\n")
        .expect_err("a negative http timeout must be rejected");
    assert!(error.contains("negative"), "{error}");
}

#[test]
fn a_zero_live_call_budget_is_rejected() {
    // Zero would hide every tool call from the user while still running them.
    let error = load_validated("[tools]\nmax_live_tool_calls = 0\n")
        .expect_err("a zero live-call budget must be rejected");
    assert!(error.contains("max_live_tool_calls"), "{error}");
}

// ── ExecutionSettings ────────────────────────────────────────────────────────

#[test]
fn authoring_limits_are_unset_by_default() {
    let config = XaftConfig::default();
    assert_eq!(config.execution.authoring_max_generation_attempts, None);
    assert_eq!(config.execution.authoring_max_phase_turns, None);
    assert_eq!(config.execution.profile, None);
    assert_eq!(config.execution.provider_capabilities, None);
}

#[test]
fn authoring_limits_load_from_a_file() {
    let config = load_validated(
        r#"
[execution]
authoring_max_generation_attempts = 3
authoring_max_phase_turns = 8
"#,
    )
    .expect("load");
    assert_eq!(config.execution.authoring_max_generation_attempts, Some(3));
    assert_eq!(config.execution.authoring_max_phase_turns, Some(8));
}

#[test]
fn an_absent_execution_table_agrees_with_the_default() {
    let from_file = load_validated(minimal()).expect("load");
    let from_default = XaftConfig::default();
    assert_eq!(
        from_file.execution.authoring_max_generation_attempts,
        from_default.execution.authoring_max_generation_attempts
    );
    assert_eq!(
        from_file.execution.authoring_max_phase_turns,
        from_default.execution.authoring_max_phase_turns
    );
}

#[test]
fn provider_capabilities_load_as_a_map() {
    let config = load_validated(
        r#"
[execution.provider_capabilities]
tools = false
vision = true
"#,
    )
    .expect("load");

    let caps = config
        .execution
        .provider_capabilities
        .as_ref()
        .expect("capabilities present");
    assert_eq!(caps.get("tools"), Some(&false));
    assert_eq!(caps.get("vision"), Some(&true));
    assert_eq!(caps.len(), 2, "only the stated keys are overridden");
}

#[test]
fn a_named_profile_that_exists_is_accepted() {
    let config = load_validated(
        r#"
[execution]
profile = "anthropic"
"#,
    )
    .expect("a profile naming a real provider must validate");
    assert_eq!(config.execution.profile.as_deref(), Some("anthropic"));
}

#[test]
fn a_profile_naming_an_unknown_provider_is_rejected() {
    // Silently falling back would run the agent against a provider the user did
    // not ask for, which is the opposite of what the key means.
    let error = load_validated(
        r#"
[execution]
profile = "does-not-exist"
"#,
    )
    .expect_err("an unknown profile must be rejected");
    assert!(error.contains("does-not-exist"), "{error}");
}

#[test]
fn an_empty_profile_is_rejected() {
    let error = load_validated("[execution]\nprofile = \"\"\n")
        .expect_err("an empty profile name must be rejected");
    assert!(error.contains("profile"), "{error}");
}

#[test]
fn whitespace_only_profile_is_rejected() {
    let error = load_validated("[execution]\nprofile = \"   \"\n")
        .expect_err("a whitespace-only profile name must be rejected");
    assert!(error.contains("profile"), "{error}");
}

// ── MemorySettings ────────────────────────────────────────────────────────────

#[test]
fn memory_defaults_match_agenthicc() {
    let memory = MemoryConfig::default();
    assert_eq!(memory.vector_db, "sqlite-vec", "agenthicc's default");
    assert_eq!(
        memory.session_ttl_seconds, 86_400,
        "agenthicc's one-day default"
    );
}

#[test]
fn an_absent_memory_table_agrees_with_the_default() {
    let from_file = load_validated(minimal()).expect("load");
    assert_eq!(
        from_file.memory.vector_db,
        MemoryConfig::default().vector_db
    );
    assert_eq!(
        from_file.memory.session_ttl_seconds,
        MemoryConfig::default().session_ttl_seconds
    );
}

#[test]
fn the_memory_keys_load_from_a_file() {
    let config = load_validated(
        r#"
[memory]
vector_db = "hnsw"
session_ttl_seconds = 3600
"#,
    )
    .expect("load");
    assert_eq!(config.memory.vector_db, "hnsw");
    assert_eq!(config.memory.session_ttl_seconds, 3600);
}

#[test]
fn vector_db_is_distinct_from_the_persistence_backend() {
    // agenthicc can hold vectors in sqlite-vec while keeping sessions in
    // postgres. Folding the two into one field would make that inexpressible,
    // so setting one must not disturb the other.
    let config = load_validated(
        r#"
[memory]
vector_db = "sqlite-vec"
backend = "postgres"
"#,
    )
    .expect("load");
    assert_eq!(config.memory.vector_db, "sqlite-vec");
    assert_eq!(config.memory.backend, "postgres");
}

#[test]
fn session_ttl_is_distinct_from_the_per_fact_ttl() {
    // `session_ttl_seconds` is the session record's lifetime;
    // `auto_remember_ttl_secs` is how long one remembered fact lives. xaft
    // deliberately defaults the latter to "never" while matching agenthicc's
    // 86400 for the former, so the two must not be aliased.
    let config = MemoryConfig::default();
    assert_eq!(config.session_ttl_seconds, 86_400);
    assert_eq!(
        config.auto_remember_ttl_secs, None,
        "per-fact expiry stays off by default; see the field doc"
    );
}

// ── BehaviourSettings ────────────────────────────────────────────────────────

#[test]
fn behaviour_defaults_match_agenthicc() {
    let tui = xaft_config::types::TuiConfig::default();
    assert!(!tui.verbose, "agenthicc's default");
    assert!(tui.confirm_exits, "agenthicc's default");
}

#[test]
fn the_behaviour_keys_load_from_a_file() {
    let config = load_validated(
        r#"
[tui]
verbose = true
confirm_exits = false
"#,
    )
    .expect("load");
    assert!(config.tui.verbose);
    assert!(!config.tui.confirm_exits);
}

#[test]
fn an_absent_tui_table_agrees_with_the_default() {
    let from_file = load_validated(minimal()).expect("load");
    assert_eq!(
        from_file.tui.verbose,
        xaft_config::types::TuiConfig::default().verbose
    );
    assert_eq!(
        from_file.tui.confirm_exits,
        xaft_config::types::TuiConfig::default().confirm_exits
    );
}

// ── Round-trips ───────────────────────────────────────────────────────────────

#[test]
fn every_new_key_survives_a_toml_round_trip() {
    let config = load_validated(
        r#"
[execution]
authoring_max_generation_attempts = 4
authoring_max_phase_turns = 9
profile = "anthropic"

[execution.provider_capabilities]
tools = true

[tools]
plugins = ["p"]
max_live_tool_calls = 7
group_exploratory_calls = false
http_timeout_s = 11.0
question_timeout_s = 22.0

[memory]
vector_db = "hnsw"
session_ttl_seconds = 60

[tui]
verbose = true
confirm_exits = false
"#,
    )
    .expect("load");

    // Round-trip each struct through its own TOML text. Comparing against the
    // live config catches a key that is read but not written back -- which is
    // how a value survives a load and then vanishes on the next save.
    let execution = toml::to_string(&config.execution).expect("render");
    let execution_rt: xaft_config::types::ExecutionConfig =
        toml::from_str(&execution).expect("re-parse");
    assert_eq!(
        execution_rt.authoring_max_generation_attempts,
        config.execution.authoring_max_generation_attempts
    );
    assert_eq!(
        execution_rt.authoring_max_phase_turns,
        config.execution.authoring_max_phase_turns
    );
    assert_eq!(execution_rt.profile, config.execution.profile);
    assert_eq!(
        execution_rt.provider_capabilities,
        config.execution.provider_capabilities
    );

    let tools = toml::to_string(&config.tools).expect("render");
    let tools_rt: ToolSettingsConfig = toml::from_str(&tools).expect("re-parse");
    assert_eq!(tools_rt.max_live_tool_calls, 7);
    assert_eq!(tools_rt.question_timeout_s, 22.0);
    assert_eq!(tools_rt.http_timeout_s, 11.0);
    assert_eq!(tools_rt.plugins, ["p"]);
    assert!(!tools_rt.group_exploratory_calls);

    let memory = toml::to_string(&config.memory).expect("render");
    let memory_rt: MemoryConfig = toml::from_str(&memory).expect("re-parse");
    assert_eq!(memory_rt.vector_db, "hnsw");
    assert_eq!(memory_rt.session_ttl_seconds, 60);

    let tui = toml::to_string(&config.tui).expect("render");
    let tui_rt: xaft_config::types::TuiConfig = toml::from_str(&tui).expect("re-parse");
    assert!(tui_rt.verbose);
    assert!(!tui_rt.confirm_exits);
}

#[test]
fn unset_keys_are_not_written_back_out() {
    // A round-tripped config must not claim defaults the user never set.
    let config = load_validated(minimal()).expect("load");

    let tools = toml::to_string(&config.tools).expect("render");
    for absent in [
        "max_live_tool_calls",
        "group_exploratory_calls",
        "http_timeout_s",
        "question_timeout_s",
        "plugins",
    ] {
        assert!(
            !tools.contains(absent),
            "{absent} was never set and must not be written back: {tools}"
        );
    }

    let memory = toml::to_string(&config.memory).expect("render");
    for absent in ["vector_db", "session_ttl_seconds"] {
        assert!(
            !memory.contains(absent),
            "{absent} was never set and must not be written back: {memory}"
        );
    }

    let tui = toml::to_string(&config.tui).expect("render");
    for absent in ["verbose", "confirm_exits"] {
        assert!(
            !tui.contains(absent),
            "{absent} was never set and must not be written back: {tui}"
        );
    }

    let execution = toml::to_string(&config.execution).expect("render");
    for absent in [
        "authoring_max_generation_attempts",
        "authoring_max_phase_turns",
        "profile",
        "provider_capabilities",
    ] {
        assert!(
            !execution.contains(absent),
            "{absent} was never set and must not be written back: {execution}"
        );
    }
}

#[test]
fn a_config_without_any_of_these_sections_still_loads() {
    // Backward compatibility: existing files must not need editing.
    let config = load_validated("[core]\nlog_level = \"info\"\n").expect("load");
    assert_eq!(config.tools.max_live_tool_calls, 5);
    assert_eq!(config.memory.vector_db, "sqlite-vec");
    assert!(config.tui.confirm_exits);
    assert_eq!(config.execution.authoring_max_phase_turns, None);
}
