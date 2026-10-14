//! Coverage for reading MCP servers from `[tools] mcp_servers`.
//!
//! agenthicc declares MCP servers under `[tools]` as `[[tools.mcp_servers]]`.
//! xaft historically used `[[mcp.client]]`. `parse_servers` now reads both, so
//! this file pins:
//!
//! - that the agenthicc spelling actually reaches `McpEntry`, rather than being
//!   declared in `XaftConfig` and ignored (which is exactly how
//!   `ToolSettings.mcp_servers` sat at ❌ as a dead field);
//! - that a malformed `[tools]` is *rejected*, not silently skipped -- the same
//!   invariant `mcp = 1` already had, applied to the second spelling;
//! - that a name declared in both tables is reported twice rather than silently
//!   resolved, since resolving it would mean picking a transport and a set of
//!   credentials by table order.
//!
//! As in `mcp_error_surface.rs`, this is the LENIENT reader: it never runs
//! `xaft_config`'s strict validation.

use std::fs;

use xaft_cli::mcp::{McpEntry, McpError, read_mcp_servers};

fn parse(tag: &str, body: &str) -> Result<Vec<McpEntry>, McpError> {
    let dir = std::env::temp_dir().join(format!("xaft-mcp-tools-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("xaft.toml");
    fs::write(&path, body).expect("write fixture");
    read_mcp_servers(&path)
}

fn names(entries: &[McpEntry]) -> Vec<&str> {
    entries.iter().map(McpEntry::name).collect()
}

// ── The agenthicc spelling ────────────────────────────────────────────────────

#[test]
fn servers_declared_under_tools_mcp_servers_are_read() {
    let entries = parse(
        "tools-only",
        r#"
[[tools.mcp_servers]]
name = "docs"
transport = "stdio"
command = "docs-server"
"#,
    )
    .expect("[[tools.mcp_servers]] must parse");

    assert_eq!(names(&entries), ["docs"]);
    match &entries[0] {
        McpEntry::Valid(config) => {
            assert_eq!(config.name, "docs");
            assert_eq!(config.normalized_transport(), "stdio");
            assert_eq!(config.command.as_deref(), Some("docs-server"));
        }
        McpEntry::Invalid { name } => panic!("expected a valid entry, got invalid {name:?}"),
    }
}

#[test]
fn a_remote_server_under_tools_mcp_servers_keeps_its_url() {
    let entries = parse(
        "remote",
        r#"
[[tools.mcp_servers]]
name = "api"
transport = "http"
url = "https://example.invalid/mcp"
"#,
    )
    .expect("remote stanza must parse");

    match &entries[0] {
        McpEntry::Valid(config) => {
            assert_eq!(config.url.as_deref(), Some("https://example.invalid/mcp"));
            assert_eq!(config.normalized_transport(), "http");
        }
        McpEntry::Invalid { name } => panic!("expected valid, got {name:?}"),
    }
}

#[test]
fn a_stanza_under_tools_mcp_servers_defaults_its_transport() {
    // Same reason `McpClientConfig::transport` carries a serde default: a
    // partially-specified stanza must not reject the whole file.
    let entries = parse(
        "default-transport",
        r#"
[[tools.mcp_servers]]
name = "bare"
"#,
    )
    .expect("a name-only stanza must parse");

    match &entries[0] {
        McpEntry::Valid(config) => assert_eq!(config.normalized_transport(), "stdio"),
        McpEntry::Invalid { name } => panic!("expected valid, got {name:?}"),
    }
}

#[test]
fn a_stanza_under_tools_mcp_servers_with_no_name_is_invalid_not_skipped() {
    // Degrading one stanza must not hide the servers around it, so it is
    // reported under agenthicc's literal `<invalid>` placeholder.
    let entries = parse(
        "no-name",
        r#"
[[tools.mcp_servers]]
transport = "stdio"

[[tools.mcp_servers]]
name = "good"
"#,
    )
    .expect("a nameless stanza must not reject the file");

    assert_eq!(names(&entries), ["<invalid>", "good"]);
}

#[test]
fn a_malformed_stanza_under_tools_mcp_servers_keeps_its_name() {
    let entries = parse(
        "malformed",
        r#"
[[tools.mcp_servers]]
name = "broken"
reconnect_attempts = "not a number"
"#,
    )
    .expect("one bad stanza must not reject the file");

    assert_eq!(names(&entries), ["broken"]);
    assert!(matches!(entries[0], McpEntry::Invalid { .. }));
}

// ── Structural rejections ─────────────────────────────────────────────────────

#[test]
fn a_tools_key_that_is_not_a_table_is_rejected_with_a_naming_message() {
    // The regression guard for the refactor that introduced this table:
    // `document.get("tools").and_then(...)` returns `None` for `tools = 1`, which
    // reads as "no servers configured" rather than "this config is malformed".
    let err = parse("tools-not-table", "tools = 1\n").expect_err("`tools = 1` is not a table");
    assert_eq!(
        err.to_string(),
        "[tools] must be a TOML table",
        "the message must name the key so the user can find it"
    );
}

#[test]
fn a_mcp_servers_key_that_is_not_an_array_is_rejected() {
    let err = parse("not-array", "[tools]\nmcp_servers = 5\n")
        .expect_err("`mcp_servers = 5` is not an array");
    assert_eq!(
        err.to_string(),
        "tools.mcp_servers must be an array of tables"
    );
}

// ── Both spellings at once ────────────────────────────────────────────────────

#[test]
fn both_spellings_are_read_and_both_appear() {
    let entries = parse(
        "both",
        r#"
[[mcp.client]]
name = "legacy"

[[tools.mcp_servers]]
name = "agenthicc"
"#,
    )
    .expect("both tables must parse");

    assert_eq!(names(&entries), ["legacy", "agenthicc"]);
}

#[test]
fn a_name_declared_in_both_tables_is_reported_twice() {
    // Deliberately not deduplicated. Choosing one would mean a server's
    // transport and credentials depend on table order, so two stanzas that
    // disagree produce a config that silently uses one transport and the
    // other's token.
    let entries = parse(
        "conflict",
        r#"
[[mcp.client]]
name = "shared"
transport = "stdio"

[[tools.mcp_servers]]
name = "shared"
transport = "http"
"#,
    )
    .expect("both stanzas must parse");

    assert_eq!(names(&entries), ["shared", "shared"]);
    let transports: Vec<String> = entries
        .iter()
        .map(|entry| match entry {
            McpEntry::Valid(c) => c.normalized_transport(),
            McpEntry::Invalid { .. } => "invalid".to_string(),
        })
        .collect();
    assert_eq!(transports, ["stdio", "http"]);
}

#[test]
fn an_absent_tools_table_is_not_an_error() {
    assert_eq!(
        names(&parse("no-tools", "[core]\nlog_level = \"info\"\n").unwrap()).len(),
        0
    );
}

#[test]
fn an_empty_tools_table_yields_no_servers() {
    let entries = parse("empty-tools", "[tools]\n").expect("an empty table is not an error");
    assert!(entries.is_empty());
}
