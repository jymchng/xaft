//! Integration tests for the `[tools]` section.
//!
//! Parity target: agenthicc's `ToolSettings.mcp_servers`, which it declares as
//! `[[tools.mcp_servers]]`. xaft historically took MCP servers under
//! `[[mcp.client]]`, so before this the config class existed and nothing read
//! it -- a dead field, which the audit script scores ❌ for exactly that reason.
//!
//! These tests pin the typed side (this crate) and the duplicate-name rule; the
//! lenient CLI reader that actually consumes these stanzas is covered by
//! `crates/xaft-cli/tests/mcp_tools_section.rs`.

use tempfile::TempDir;

use xaft_config::{ConfigLoader, McpClientConfig, ToolSettingsConfig, XaftConfig};

fn load(toml_text: &str) -> Result<XaftConfig, String> {
    let dir = TempDir::new().map_err(|e| e.to_string())?;
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, toml_text).map_err(|e| e.to_string())?;
    ConfigLoader::load_file(&path).map_err(|e| e.to_string())
}

/// Load and validate, the way `ConfigLoader::load` does at startup.
///
/// The built-in agent presets all reference an `anthropic` provider, and
/// `validate` checks agents before tools, so the fixture supplies it.
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

#[test]
fn the_tools_section_defaults_to_no_servers() {
    let settings = ToolSettingsConfig::default();
    assert!(settings.mcp_servers.is_empty());
    assert!(settings.duplicate_server_names().is_empty());
}

#[test]
fn a_config_with_no_tools_section_loads() {
    // Backward compatibility: nothing about the existing file shape changed.
    let config = load(
        r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
"#,
    )
    .expect("load");
    assert!(config.tools.mcp_servers.is_empty());
}

#[test]
fn the_tool_map_and_the_tools_table_are_separate_sections() {
    // `[tool.<name>]` configures one tool; `[tools]` holds cross-tool policy.
    // Collapsing them would make a global default indistinguishable from a
    // per-tool override.
    let config = load(
        r#"
[tool.shell]
enabled = false

[tools]
"#,
    )
    .expect("load");

    assert!(!config.tool["shell"].enabled);
    assert!(config.tools.mcp_servers.is_empty());
}

#[test]
fn servers_declared_under_tools_mcp_servers_load() {
    let config = load(
        r#"
[[tools.mcp_servers]]
name = "docs"
transport = "stdio"
command = "docs-server"

[[tools.mcp_servers]]
name = "api"
transport = "http"
url = "https://example.invalid/mcp"
"#,
    )
    .expect("load");

    let servers: Vec<&McpClientConfig> = config.tools.mcp_servers.iter().collect();
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0].name, "docs");
    assert_eq!(servers[0].normalized_transport(), "stdio");
    assert_eq!(servers[1].normalized_transport(), "http");
    assert_eq!(
        servers[1].url.as_deref(),
        Some("https://example.invalid/mcp")
    );
}

#[test]
fn a_partially_specified_stanza_still_loads() {
    // `name` and `transport` carry serde defaults so one incomplete stanza does
    // not reject the whole file -- see the note in `xaft_cli::mcp` on why that
    // default is load-bearing rather than a convenience.
    let config = load(
        r#"
[[tools.mcp_servers]]
name = "bare"
"#,
    )
    .expect("load");
    assert_eq!(config.tools.mcp_servers[0].normalized_transport(), "stdio");
}

#[test]
fn both_mcp_spellings_can_coexist_in_one_file() {
    // They are separate sections, so a file may declare servers in each. The
    // typed loader does not merge them; the CLI reports both.
    let config = load(
        r#"
[[mcp.client]]
name = "legacy"

[[tools.mcp_servers]]
name = "agenthicc"
"#,
    )
    .expect("load");

    assert_eq!(config.mcp.client.len(), 1);
    assert_eq!(config.mcp.client[0].name, "legacy");
    assert_eq!(config.tools.mcp_servers.len(), 1);
    assert_eq!(config.tools.mcp_servers[0].name, "agenthicc");
}

#[test]
fn distinct_names_are_not_duplicates() {
    let config = load(
        r#"
[[tools.mcp_servers]]
name = "a"

[[tools.mcp_servers]]
name = "b"

[[tools.mcp_servers]]
name = "c"
"#,
    )
    .expect("load");
    assert!(config.tools.duplicate_server_names().is_empty());
    // The same servers, this time through the validating loader, so the rule is
    // pinned on the path that actually runs at startup.
    assert!(
        load_validated(
            r#"
[[tools.mcp_servers]]
name = "a"

[[tools.mcp_servers]]
name = "b"
"#
        )
        .is_ok()
    );
}

#[test]
fn a_repeated_name_is_reported_once_and_sorted() {
    let config = load(
        r#"
[[tools.mcp_servers]]
name = "zebra"

[[tools.mcp_servers]]
name = "apple"

[[tools.mcp_servers]]
name = "apple"

[[tools.mcp_servers]]
name = "zebra"
"#,
    )
    .expect("load");

    assert_eq!(
        config.tools.duplicate_server_names(),
        ["apple", "zebra"],
        "each duplicated name appears once, in a stable order"
    );
}

#[test]
fn validation_rejects_duplicate_server_names() {
    // Two stanzas for one name is an error, not a merge: which one wins would
    // depend on array order, so a server's transport and credentials would
    // depend on how the file happens to be sorted.
    let error = load_validated(
        r#"
[[tools.mcp_servers]]
name = "shared"
transport = "stdio"

[[tools.mcp_servers]]
name = "shared"
transport = "http"
"#,
    )
    .expect_err("duplicate names must be rejected");
    assert!(error.contains("duplicate"), "{error}");
    assert!(error.contains("shared"), "{error}");
}

#[test]
fn validation_accepts_distinct_server_names() {
    load_validated(
        r#"
[[tools.mcp_servers]]
name = "a"

[[tools.mcp_servers]]
name = "b"
"#,
    )
    .expect("distinct names must validate");
}

#[test]
fn the_tools_section_round_trips_through_toml() {
    let config = load(
        r#"
[[tools.mcp_servers]]
name = "docs"
transport = "stdio"
command = "docs-server"
"#,
    )
    .expect("load");

    let rendered = toml::to_string(&config.tools).expect("render");
    let reparsed: ToolSettingsConfig = toml::from_str(&rendered).expect("re-parse");

    assert_eq!(reparsed.mcp_servers.len(), 1);
    assert_eq!(reparsed.mcp_servers[0].name, "docs");
    assert_eq!(
        reparsed.mcp_servers[0].command.as_deref(),
        Some("docs-server")
    );
}

#[test]
fn an_empty_tools_section_is_not_written_back_out() {
    let config = load("[core]\nlog_level = \"info\"\n").expect("load");
    let rendered = toml::to_string(&config.tools).expect("render");
    assert!(
        !rendered.contains("mcp_servers"),
        "no servers were declared and none must be invented: {rendered}"
    );
}
