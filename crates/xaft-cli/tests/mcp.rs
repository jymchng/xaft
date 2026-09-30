//! Tests for the `xaft mcp` config layer.
//!
//! These lock in the parts of the contract a user can actually observe by
//! reading their own config file back: that a literal token is never printed,
//! that `auth` distinguishes an environment reference from a pasted secret,
//! that one malformed stanza does not hide the others, and — most importantly
//! — that `add` and `remove` leave every other byte of the file alone.

use std::collections::BTreeMap;

use xaft_cli::mcp::{
    McpEntry, McpError, Scope, ServerSpec, add_mcp_server, config_path, read_mcp_servers,
    remove_mcp_server,
};
use xaft_config::{McpClientConfig, is_env_reference};

fn spec(name: &str, target: &str) -> ServerSpec {
    ServerSpec {
        name: name.into(),
        transport: "stdio".into(),
        target: target.into(),
        args: Vec::new(),
        token_env: None,
        auto_connect: true,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
    }
}

fn config_with(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, body).expect("write config");
    (dir, path)
}

// ── Redaction ──────────────────────────────────────────────────────────────────────────

#[test]
fn redacted_output_never_contains_a_literal_token() {
    let config = McpClientConfig {
        name: "files".into(),
        transport: "stdio".into(),
        command: Some("./mcp-files".into()),
        args: vec![],
        url: None,
        enabled: true,
        token: Some("sk-super-secret-value".into()),
        token_env: None,
        env_headers: BTreeMap::new(),
        auto_connect: true,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
    };

    let rendered = serde_json::to_string(&config.redacted()).expect("serialise");
    assert!(
        !rendered.contains("sk-super-secret-value"),
        "redacted output leaked the token: {rendered}"
    );
    // The key must be absent entirely, not merely blanked: a consumer that
    // sees `"token": ""` still learns there is a token to go looking for.
    assert!(
        !rendered.contains("\"token\""),
        "redacted output still has a token key: {rendered}"
    );
}

#[test]
fn redacted_output_keeps_env_header_names_because_they_are_not_secrets() {
    let mut env_headers = BTreeMap::new();
    env_headers.insert("Authorization".to_string(), "MCP_TOKEN".to_string());
    let config = McpClientConfig {
        name: "remote".into(),
        transport: "http".into(),
        command: None,
        args: vec![],
        url: Some("https://example.test/mcp".into()),
        enabled: true,
        token: None,
        token_env: None,
        env_headers,
        auto_connect: true,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
    };

    let rendered = serde_json::to_string(&config.redacted()).expect("serialise");
    assert!(
        rendered.contains("MCP_TOKEN"),
        "env var name was dropped: {rendered}"
    );
    assert!(rendered.contains("example.test"));
}

#[test]
fn a_stdio_server_never_prints_its_url() {
    let config = McpClientConfig {
        name: "files".into(),
        transport: "STDIO".into(),
        command: Some("./mcp-files".into()),
        args: vec!["--root".into(), "/srv".into()],
        url: Some("https://should-not-appear.test".into()),
        enabled: true,
        token: None,
        token_env: None,
        env_headers: BTreeMap::new(),
        auto_connect: true,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
    };
    let redacted = config.redacted();
    assert_eq!(redacted["url"], "<stdio>");
    assert_eq!(
        redacted["transport"], "stdio",
        "transport must be normalised"
    );
    assert!(
        !serde_json::to_string(&redacted)
            .unwrap()
            .contains("should-not-appear")
    );
}

// ── auth: environment reference vs pasted secret ───────────────────────────────────────

#[test]
fn is_env_reference_matches_agenthiccs_deliberately_loose_rule() {
    // agenthicc's `mcp auth` tests only `token.startswith("${")`.
    assert!(is_env_reference("${MCP_TOKEN}"));
    assert!(
        is_env_reference("${lower}"),
        "agenthicc does not require uppercase here"
    );
    assert!(is_env_reference("${"), "even a bare prefix counts");
    assert!(!is_env_reference("sk-literal"));
    assert!(
        !is_env_reference("prefix ${MCP_TOKEN}"),
        "must be at the start"
    );
    assert!(!is_env_reference("$MCP_TOKEN"), "braces are required");
    assert!(!is_env_reference(""), "empty is not a reference");
}

#[test]
fn has_auth_is_true_for_env_backed_credentials_and_false_for_literal_ones() {
    let base = McpClientConfig {
        name: "s".into(),
        transport: "http".into(),
        command: None,
        args: vec![],
        url: Some("https://e.test".into()),
        enabled: true,
        token: None,
        token_env: None,
        env_headers: BTreeMap::new(),
        auto_connect: true,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
    };

    let mut with_env_header = base.clone();
    with_env_header
        .env_headers
        .insert("Authorization".into(), "MCP_TOKEN".into());
    assert!(
        with_env_header.has_auth(),
        "an env header is a configured credential"
    );

    let mut with_ref = base.clone();
    with_ref.token = Some("${MCP_TOKEN}".into());
    assert!(
        with_ref.has_auth(),
        "an env-var reference is a configured credential"
    );

    let mut with_literal = base.clone();
    with_literal.token = Some("sk-pasted-into-the-file".into());
    assert!(
        !with_literal.has_auth(),
        "a pasted literal is NOT environment-backed configuration"
    );

    assert!(!base.has_auth(), "no credentials at all");
}

// ── Reading: one bad stanza must not hide the good ones ───────────────────────────────

#[test]
fn a_stanza_that_fails_to_parse_degrades_to_invalid_and_keeps_its_name() {
    let (_dir, path) = config_with(
        r#"
[[mcp.client]]
name = "good"
transport = "stdio"
command = "./ok"

[[mcp.client]]
name = "broken"
# `transport` is a required String, so omitting it makes this stanza invalid.
# (Omitting `command` would NOT: serde defaults a missing Option to None.)
"#,
    );

    let entries = read_mcp_servers(&path).expect("read");
    assert_eq!(
        entries.len(),
        2,
        "a bad stanza must not swallow the good one"
    );

    assert!(matches!(&entries[0], McpEntry::Valid(c) if c.name == "good"));

    let McpEntry::Invalid { name } = &entries[1] else {
        panic!(
            "expected the second stanza to be invalid, got {:?}",
            entries[1]
        );
    };
    assert_eq!(name, "broken", "the name is recovered from the raw table");
}

#[test]
fn an_invalid_entry_serialises_to_the_two_keys_agenthicc_uses() {
    let entry = McpEntry::Invalid {
        name: "broken".into(),
    };
    let json = entry.redacted();
    assert_eq!(json["name"], "broken");
    assert_eq!(json["status"], "invalid");
    assert_eq!(json.as_object().expect("object").len(), 2);
}

#[test]
fn a_missing_file_reads_as_an_empty_list_rather_than_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entries = read_mcp_servers(&dir.path().join("absent.toml")).expect("read");
    assert!(entries.is_empty());
}

// ── Writing: the file must survive being edited ────────────────────────────────────────

#[test]
fn add_appends_a_stanza_and_leaves_every_other_byte_alone() {
    let original = "\
# My xaft config — I wrote this comment myself.
[core]
log_level = \"info\"

[provider.openai]
type = \"openai\"
base_url = \"https://api.openai.com/v1\"
";
    let (_dir, path) = config_with(original);

    add_mcp_server(&path, &spec("files", "./mcp-files")).expect("add");

    let after = std::fs::read_to_string(&path).expect("read back");
    assert!(
        after.starts_with(original),
        "add rewrote the existing content; the original must be a byte-for-byte prefix"
    );
    assert!(after.contains("[[mcp.client]]"));
    assert!(after.contains("name = \"files\""));
    assert!(after.contains("command = \"./mcp-files\""));
}

#[test]
fn add_refuses_a_duplicate_name() {
    let (_dir, path) = config_with("");
    add_mcp_server(&path, &spec("files", "./a")).expect("first add");
    let err = add_mcp_server(&path, &spec("files", "./b")).expect_err("duplicate");
    assert!(
        err.to_string().contains("already exists"),
        "unexpected error: {err}"
    );
}

#[test]
fn add_writes_both_the_token_reference_and_the_variable_name() {
    let (_dir, path) = config_with("");
    let mut s = spec("remote", "https://example.test/mcp");
    s.transport = "http".into();
    s.token_env = Some("MCP_TOKEN".into());
    add_mcp_server(&path, &s).expect("add");

    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains(r#"token = "${MCP_TOKEN}""#), "{text}");
    assert!(text.contains(r#"token_env = "MCP_TOKEN""#), "{text}");

    // And the file must still parse into the typed struct.
    let entries = read_mcp_servers(&path).expect("read");
    let McpEntry::Valid(c) = &entries[0] else {
        panic!("expected a valid entry, got {:?}", entries[0]);
    };
    assert_eq!(c.token.as_deref(), Some("${MCP_TOKEN}"));
    assert_eq!(c.token_env.as_deref(), Some("MCP_TOKEN"));
    assert!(c.has_auth());
}

#[test]
fn add_omits_fields_left_at_their_defaults() {
    let (_dir, path) = config_with("");
    add_mcp_server(&path, &spec("files", "./mcp-files")).expect("add");
    let text = std::fs::read_to_string(&path).expect("read");
    for omitted in [
        "auto_connect",
        "reconnect_attempts",
        "reconnect_delay_seconds",
        "token",
    ] {
        assert!(
            !text.contains(omitted),
            "{omitted} is at its default and should not be written: {text}"
        );
    }
}

#[test]
fn add_writes_non_default_reconnect_settings() {
    let (_dir, path) = config_with("");
    let mut s = spec("files", "./mcp-files");
    s.reconnect_attempts = 7;
    s.reconnect_delay_seconds = 2.5;
    s.auto_connect = false;
    add_mcp_server(&path, &s).expect("add");

    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("reconnect_attempts = 7"), "{text}");
    assert!(text.contains("reconnect_delay_seconds = 2.5"), "{text}");
    assert!(text.contains("auto_connect = false"), "{text}");
}

#[test]
fn remove_excises_only_the_named_stanza() {
    let (_dir, path) = config_with("");
    add_mcp_server(&path, &spec("keep", "./keep")).expect("add");
    add_mcp_server(&path, &spec("drop", "./drop")).expect("add");
    add_mcp_server(&path, &spec("keep2", "./keep2")).expect("add");

    remove_mcp_server(&path, "drop").expect("remove");

    let names: Vec<String> = read_mcp_servers(&path)
        .expect("read")
        .iter()
        .map(|e| e.name().to_string())
        .collect();
    assert_eq!(names, vec!["keep".to_string(), "keep2".to_string()]);
}

#[test]
fn add_then_remove_restores_the_original_text() {
    let original = "# comment\n[core]\nlog_level = \"info\"\n";
    let (_dir, path) = config_with(original);
    add_mcp_server(&path, &spec("temp", "./temp")).expect("add");
    remove_mcp_server(&path, "temp").expect("remove");
    // `add` writes a blank separator line before the stanza and `remove`
    // excises only the stanza's own lines, so one trailing blank line
    // survives. The guarantee is that no *content* is lost or reordered.
    let after = std::fs::read_to_string(&path).expect("read");
    assert!(
        after.starts_with(original),
        "original content was not preserved"
    );
    assert!(
        !after.contains("[[mcp.client]]"),
        "stanza survived: {after}"
    );
    assert_eq!(after.trim_end(), original.trim_end());
}

#[test]
fn remove_reports_a_missing_server_instead_of_rewriting_the_file() {
    let original = "# comment\n[core]\nlog_level = \"info\"\n";
    let (_dir, path) = config_with(original);
    let err = remove_mcp_server(&path, "absent").expect_err("missing");
    assert!(matches!(err, McpError::NotFound(_)), "got {err:?}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        original,
        "a failed remove must not modify the file"
    );
}

#[test]
fn remove_refuses_to_rewrite_an_inlined_array() {
    // The surgical edit can only find an explicit `[[mcp.client]]` header. An
    // inlined array is left completely alone rather than round-tripped, which
    // would destroy the user's comments.
    let original = "# comment\n[core]\nlog_level = \"info\"\n";
    let (_dir, path) = config_with(original);
    std::fs::write(
        &path,
        "[mcp]\nclient = [{ name = \"inline\", transport = \"stdio\", command = \"./x\" }]\n",
    )
    .expect("write");

    let err = remove_mcp_server(&path, "inline").expect_err("no stanza header");
    assert!(matches!(err, McpError::StanzaNotFound(_)), "got {err:?}");
}

#[test]
fn add_refuses_to_write_into_a_file_that_does_not_parse() {
    let (_dir, path) = config_with("this is not = = toml");
    let err = add_mcp_server(&path, &spec("files", "./x")).expect_err("bad toml");
    assert!(matches!(err, McpError::Io(_)), "got {err:?}");
}

// ── Validation ─────────────────────────────────────────────────────────────────────────

#[test]
fn server_names_follow_agenthiccs_character_class() {
    assert!(spec("ok.name-1_2", "./x").validated().is_ok());
    assert!(
        spec(".leading", "./x").validated().is_err(),
        "must start alnum"
    );
    assert!(spec("has space", "./x").validated().is_err());
    assert!(spec("has:colon", "./x").validated().is_err());
    assert!(
        spec(&"a".repeat(65), "./x").validated().is_err(),
        "65 chars is too long"
    );
    assert!(
        spec(&"a".repeat(64), "./x").validated().is_ok(),
        "64 chars is allowed"
    );
}

#[test]
fn an_empty_target_is_rejected() {
    let err = spec("files", "   ").validated().expect_err("empty");
    assert!(err.to_string().contains("cannot be empty"), "{err}");
}

#[test]
fn an_unsupported_transport_lists_the_choices() {
    let mut s = spec("files", "./x");
    s.transport = "carrier-pigeon".into();
    let err = s.validated().expect_err("bad transport");
    let text = err.to_string();
    assert!(text.contains("unsupported transport"), "{text}");
    assert!(text.contains("stdio"), "choices should be listed: {text}");
}

#[test]
fn a_lowercase_token_env_is_rejected() {
    let mut s = spec("remote", "https://e.test");
    s.token_env = Some("mcp_token".into());
    let err = s.validated().expect_err("lowercase env");
    assert!(err.to_string().contains("uppercase"), "{err}");
}

#[test]
fn a_negative_or_nan_reconnect_delay_is_rejected() {
    let mut s = spec("files", "./x");
    s.reconnect_delay_seconds = -1.0;
    assert!(s.clone().validated().is_err(), "negative delay");

    s.reconnect_delay_seconds = f64::NAN;
    assert!(s.clone().validated().is_err(), "NaN delay");

    s.reconnect_delay_seconds = f64::INFINITY;
    assert!(s.validated().is_err(), "infinite delay");
}

#[test]
fn transport_is_case_insensitive() {
    let mut s = spec("files", "./x");
    s.transport = "  STDIO  ".into();
    assert!(s.validated().expect("valid").transport == "stdio");
}

// ── Path resolution ────────────────────────────────────────────────────────────────────

#[test]
fn an_explicit_config_path_beats_every_scope_flag() {
    let scope = Scope {
        global: true,
        project: false,
        explicit: Some("/tmp/explicit.toml".into()),
    };
    let path = config_path(scope, &std::env::current_dir().unwrap(), None).expect("resolve");
    assert_eq!(
        path,
        std::path::PathBuf::from("/tmp/explicit.toml"),
        "an explicit --config must win over --global"
    );
}

#[test]
fn both_scope_flags_together_is_an_error() {
    let scope = Scope {
        global: true,
        project: true,
        explicit: None,
    };
    let err = config_path(scope, &std::env::current_dir().unwrap(), None).expect_err("conflict");
    assert!(matches!(err, McpError::ConflictingScope));
    assert!(err.to_string().contains("choose only one target"), "{err}");
}

#[test]
fn project_scope_walks_up_to_an_existing_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("a/b/c");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(dir.path().join(".xaft.toml"), "").expect("write");

    let scope = Scope {
        project: true,
        ..Default::default()
    };
    let path = config_path(scope, &nested, None).expect("resolve");
    assert_eq!(path, dir.path().join(".xaft.toml"));
}

#[test]
fn project_scope_falls_back_to_the_conventional_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope {
        project: true,
        ..Default::default()
    };
    let path = config_path(scope, dir.path(), None).expect("resolve");
    assert_eq!(path, dir.path().join(".xaft/xaft.toml"));
}

#[test]
fn global_scope_uses_the_home_config_directory() {
    let home = tempfile::tempdir().expect("tempdir");
    let scope = Scope {
        global: true,
        ..Default::default()
    };
    let path =
        config_path(scope, &std::env::current_dir().unwrap(), Some(home.path())).expect("resolve");
    assert_eq!(path, home.path().join(".config/xaft/xaft.toml"));
}

#[test]
fn global_scope_without_a_home_is_an_error() {
    let scope = Scope {
        global: true,
        ..Default::default()
    };
    let err = config_path(scope, &std::env::current_dir().unwrap(), None).expect_err("no home");
    assert!(matches!(err, McpError::NoHomeDir));
}

// ── A new file is not world-readable ───────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn a_newly_created_config_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".xaft").join("xaft.toml");
    add_mcp_server(&path, &spec("files", "./x")).expect("add");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "an MCP stanza can carry a token");
}

#[cfg(unix)]
#[test]
fn an_existing_config_keeps_its_own_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, path) = config_with("");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    add_mcp_server(&path, &spec("files", "./x")).expect("add");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "the user's chosen mode must not change");
}
