//! Behavioural coverage for the parts of `xaft_cli::mcp` that the existing
//! `mcp.rs` / `mcp_handlers.rs` suites leave uncovered.
//!
//! The gap is not incidental: `McpError`'s `Display` impl is the *entire*
//! user-visible contract of this module (agenthicc prints `error: {msg}` and
//! returns without a non-zero exit, so the string is what a user sees), yet
//! three of its six arms had no test. Likewise the TOML escaper — a server
//! name containing a quote or a backslash is a realistic thing for a user to
//! type, and an unescaped write would produce a config file that no longer
//! parses while `mcp add` reported success.
//!
//! NOTE on scope: `read_mcp_servers` is the LENIENT reader. It never runs
//! `xaft_config`'s strict validation, so a file it accepts may still be
//! rejected by `ConfigLoader::load` at startup. These tests therefore assert
//! the lenient path's own behaviour and must not be read as covering the
//! strict loader — that is a separate defect with a separate owner.

use std::fs;
use std::path::Path;

use xaft_cli::mcp::{
    McpEntry, McpError, Scope, ServerSpec, add_mcp_server, config_path, read_mcp_servers,
};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("xaft-mcp-err-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("temp dir");
    base
}

fn spec(name: &str, target: &str) -> ServerSpec {
    ServerSpec {
        name: name.to_string(),
        transport: "stdio".to_string(),
        target: target.to_string(),
        args: vec![],
        token_env: None,
        auto_connect: true,
        reconnect_attempts: 3,
        reconnect_delay_seconds: 1.0,
    }
}

// ── McpError::Display: every arm, exactly ──────────────────────────────────

#[test]
fn every_mcp_error_renders_the_string_a_user_is_shown() {
    // agenthicc prints `error: {msg}`, so these strings ARE the UX. Pin all six
    // arms so a reword cannot silently ship.
    assert_eq!(
        McpError::ConflictingScope.to_string(),
        "choose only one target: --global or --project"
    );
    assert_eq!(
        McpError::Io("disk on fire".into()).to_string(),
        "disk on fire"
    );
    assert_eq!(
        McpError::Invalid("bad stanza".into()).to_string(),
        "bad stanza"
    );
    assert_eq!(
        McpError::NotFound("alpha".into()).to_string(),
        "MCP server not found: alpha"
    );
    assert_eq!(
        McpError::StanzaNotFound("alpha".into()).to_string(),
        "could not locate MCP stanza for alpha"
    );
    assert_eq!(
        McpError::NoHomeDir.to_string(),
        "could not determine user config directory"
    );
}

#[test]
fn the_io_error_names_the_path_that_failed() {
    // The `Io` arm is the one that carries context. `read_mcp_servers` on a
    // DIRECTORY gives `IsADirectory` (not `NotFound`), which is the arm that
    // formats the path rather than swallowing it as "no config yet".
    let dir = temp_dir("io-path");
    let err = read_mcp_servers(&dir).expect_err("a directory is not readable as text");

    match &err {
        McpError::Io(msg) => {
            assert!(
                msg.starts_with("could not read "),
                "Io must name the operation and path: {msg}"
            );
            assert!(
                msg.contains(&dir.display().to_string()),
                "Io must name the path: {msg}"
            );
        }
        other => panic!("expected Io, got {other:?}"),
    }
}

#[test]
fn a_missing_config_reads_as_empty_rather_than_failing() {
    // The deliberate counterpart to the arm above: `NotFound` is swallowed so a
    // first-run `mcp list` prints "no servers" instead of an error.
    let dir = temp_dir("missing");
    let path = dir.join("does-not-exist.toml");
    assert!(
        read_mcp_servers(&path)
            .expect("missing file is not an error")
            .is_empty(),
        "a missing config must read as zero servers"
    );
}

// ── McpEntry::name: the Invalid arm ────────────────────────────────────────

#[test]
fn an_invalid_entry_still_reports_the_name_it_recovered() {
    let recovered = McpEntry::Invalid {
        name: "half-typed".to_string(),
    };
    assert_eq!(recovered.name(), "half-typed");

    // Build the Valid arm through the real reader rather than by constructing
    // `McpClientConfig` by hand: it has no `Default`, and going through the
    // parser is the seam a caller actually uses anyway.
    let entries = parse_via_file(
        "valid-name",
        "[[mcp.client]]\nname = \"alpha\"\ntransport = \"stdio\"\ncommand = \"srv\"\n",
    )
    .expect("a well-formed stanza parses");
    assert_eq!(entries[0].name(), "alpha");
}

#[test]
fn an_invalid_entry_renders_the_same_two_keys_a_valid_one_does() {
    // A consumer parsing `mcp list --json` must handle ONE shape. Assert the
    // invalid entry really does carry both keys agenthicc gives it.
    let json = McpEntry::Invalid {
        name: "half-typed".to_string(),
    }
    .redacted();

    assert_eq!(json["name"], "half-typed");
    assert_eq!(json["status"], "invalid");
}

// ── parse_servers: the two structural rejections ──────────────────────────

fn parse_via_file(tag: &str, body: &str) -> Result<Vec<McpEntry>, McpError> {
    let dir = temp_dir(tag);
    let path = dir.join("xaft.toml");
    fs::write(&path, body).expect("write fixture");
    read_mcp_servers(&path)
}

#[test]
fn a_mcp_key_that_is_not_a_table_is_rejected_with_a_naming_message() {
    let err = parse_via_file("not-table", "mcp = 1\n").expect_err("`mcp = 1` is not a table");
    assert_eq!(
        err.to_string(),
        "[mcp] must be a TOML table",
        "the message must name the key so the user can find it"
    );
}

#[test]
fn a_client_key_that_is_not_an_array_of_tables_is_rejected() {
    let err = parse_via_file("not-array", "[mcp]\nclient = 5\n").expect_err("`client = 5`");
    assert_eq!(err.to_string(), "mcp.client must be an array of tables");
}

#[test]
fn an_empty_or_mcp_free_document_yields_no_servers() {
    // The three "nothing configured" exits: blank text, no `[mcp]` at all, and
    // `[mcp]` with no `client` key. All three must be empty, not errors.
    // `McpEntry` has no `PartialEq`, so emptiness is asserted as a length.
    assert_eq!(
        parse_via_file("blank", "   \n\n")
            .expect("blank text is not an error")
            .len(),
        0
    );
    assert_eq!(
        parse_via_file("no-mcp", "[core]\nlog_level = \"info\"\n")
            .expect("a document with no [mcp] is not an error")
            .len(),
        0
    );
    assert_eq!(
        parse_via_file("no-client", "[mcp]\nenabled = true\n")
            .expect("[mcp] without client is not an error")
            .len(),
        0
    );
}

// ── the TOML escaper, driven through a real `mcp add` write ────────────────

#[test]
fn a_name_requiring_toml_escaping_is_written_escaped_and_re_parses() {
    // A quote, a backslash, a tab and a control character all appear in
    // `ServerSpec::render`'s output through the shared `toml_string` escaper.
    // The assertion that matters is the ROUND TRIP: `mcp add` reported success,
    // so the file it wrote must still parse back to the same name.
    let dir = temp_dir("escape");
    let path = dir.join("xaft.toml");
    let nasty = "qu\"ote\\back\tslash\u{1}bell";

    add_mcp_server(&path, &spec(nasty, "srv"))
        .expect("a name with escapes is still a legal server name");

    let raw = fs::read_to_string(&path).expect("written config");
    // The escapes are present in the raw bytes, not the literal characters.
    assert!(raw.contains(r#"\"#), "quote must be escaped: {raw}");
    assert!(raw.contains(r"\\"), "backslash must be escaped: {raw}");
    assert!(raw.contains(r"\t"), "tab must be escaped: {raw}");
    assert!(
        raw.contains(r"\u0001"),
        "control char must be escaped: {raw}"
    );
    assert!(
        !raw.contains('\u{1}'),
        "raw control char must not survive: {raw}"
    );

    // And it round-trips: the lenient reader gets the original name back.
    let entries = read_mcp_servers(&path).expect("the written file must parse");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name(), nasty);
}

#[test]
fn stdio_args_are_rendered_as_a_toml_array() {
    // The `args = [...]` line is only emitted when there is at least one arg,
    // and each element goes through the same escaper as any other string.
    let dir = temp_dir("args");
    let path = dir.join("xaft.toml");
    let mut s = spec("withargs", "srv");
    s.args = vec!["--flag".to_string(), "a \"quoted\" value".to_string()];

    add_mcp_server(&path, &s).expect("args are allowed");

    let raw = fs::read_to_string(&path).expect("written config");
    assert!(
        raw.contains(r#"args = ["--flag", "a \"quoted\" value"]"#),
        "args line missing or unescaped: {raw}"
    );

    let entries = read_mcp_servers(&path).expect("parses");
    assert_eq!(entries[0].name(), "withargs");
}

#[test]
fn a_server_with_no_args_omits_the_args_line_entirely() {
    // agenthicc's `_server_block` omits defaults rather than writing them out,
    // and the same holds here: an empty `args` must not produce `args = []`.
    let dir = temp_dir("noargs");
    let path = dir.join("xaft.toml");
    add_mcp_server(&path, &spec("plain", "srv")).expect("add");

    let raw = fs::read_to_string(&path).expect("written config");
    assert!(!raw.contains("args"), "empty args must be omitted: {raw}");
    assert!(raw.contains("command = \"srv\""), "{raw}");
}

// ── config_path: the scope resolution errors ───────────────────────────────

#[test]
fn asking_for_two_scopes_at_once_is_an_error() {
    let dir = temp_dir("scope");
    let err = config_path(
        Scope {
            global: true,
            project: true,
            explicit: None,
        },
        &dir,
        None,
    )
    .expect_err("global + project is ambiguous");
    assert_eq!(
        err.to_string(),
        "choose only one target: --global or --project"
    );
}

#[test]
fn a_conflicting_pair_of_flags_is_rejected_before_an_explicit_path_is_considered() {
    // RESOLVED DIVERGENCE. This test was written while `Scope.explicit`'s doc
    // comment claimed the explicit path "wins over both flags", which read as
    // though `--config X --global --project` resolves to `X`. MEASURED: it does
    // not — `config_path` evaluates `global && project` FIRST and returns
    // `ConflictingScope` before it ever looks at `explicit`.
    //
    // The goal offered two ways to reconcile them: reword the field doc to the
    // measured order (option (a)), or move the conflict check after the
    // explicit early return so `--config` genuinely wins (option (b)). Option
    // (a) was taken, for two reasons, and the ordering below is therefore
    // deliberate rather than incidental:
    //
    //   1. Asking for both scopes is contradictory whether or not a path was
    //      also named, so rejecting the pair outright is the honest semantic.
    //   2. Option (b) could not have delivered its stated benefit anyway. The
    //      pair never reaches `config_path` from the CLI: `McpScopeArgs` marks
    //      `--global`/`--project` as clap conflicts, so clap exits 2 first —
    //      see `the_cli_rejects_the_contradictory_scope_pair_before_config_path_runs`.
    //      Making `--config` win would have meant dropping that clap conflict
    //      too, changing `--help`, which is a far larger change than the goal
    //      described and is not authorized here.
    //
    // The field doc comment now states this order, so the assertion below pins
    // the behaviour the documentation claims rather than a documented
    // divergence.
    let dir = temp_dir("explicit-conflict");
    let want = dir.join("explicit.toml");
    assert_eq!(
        config_path(
            Scope {
                global: true,
                project: true,
                explicit: Some(want.clone()),
            },
            &dir,
            None,
        )
        .expect_err("the conflict is detected first")
        .to_string(),
        "choose only one target: --global or --project"
    );
}

#[test]
fn an_explicit_path_is_used_when_the_scope_flags_agree() {
    // The same precedence question, asked the way it can actually be answered:
    // with a single scope flag set alongside `--config`.
    let dir = temp_dir("explicit-ok");
    let want = dir.join("explicit.toml");
    assert_eq!(
        config_path(
            Scope {
                global: true,
                project: false,
                explicit: Some(want.clone()),
            },
            &dir,
            None,
        )
        .expect("explicit path resolves"),
        want
    );
    assert_eq!(
        config_path(
            Scope {
                global: false,
                project: true,
                explicit: Some(want.clone()),
            },
            &dir,
            None,
        )
        .expect("explicit path resolves"),
        want
    );
}

#[test]
fn the_cli_rejects_the_contradictory_scope_pair_before_config_path_runs() {
    // WHY THIS TEST EXISTS: the ordering pinned above is only observable from a
    // library caller. It is not user-visible, because clap refuses the pair at
    // parse time — with or without `--config` — so no argv can put both flags
    // in front of `config_path`. Pin that, because it is the fact that makes
    // "reorder the two checks in `config_path`" insufficient to deliver the
    // rejected option (b): making `--config` win for a CLI user would require
    // dropping these clap conflicts as well.
    use clap::Parser;

    let err =
        xaft_cli::args::XaftCli::try_parse_from(["xaft", "mcp", "list", "--global", "--project"])
            .expect_err("`--global --project` must not parse");
    assert_eq!(err.exit_code(), 2, "clap usage errors keep their exit code");
    assert!(
        err.to_string().contains("cannot be used with"),
        "clap's message must name the conflict, got: {err}"
    );

    let err = xaft_cli::args::XaftCli::try_parse_from([
        "xaft",
        "mcp",
        "list",
        "--config",
        "/tmp/explicit.toml",
        "--global",
        "--project",
    ])
    .expect_err("`--config` must not rescue the contradictory pair");
    assert_eq!(err.exit_code(), 2);

    // The conflict is specific to the PAIR, not to `--config` being present:
    // one scope flag alongside an explicit path still parses, which is the
    // single-scope case `config_path` resolves to the explicit path.
    for argv in [
        vec!["xaft", "mcp", "list", "--config", "/tmp/explicit.toml"],
        vec![
            "xaft",
            "mcp",
            "list",
            "--config",
            "/tmp/explicit.toml",
            "--global",
        ],
        vec![
            "xaft",
            "mcp",
            "list",
            "--config",
            "/tmp/explicit.toml",
            "--project",
        ],
    ] {
        xaft_cli::args::XaftCli::try_parse_from(&argv)
            .unwrap_or_else(|e| panic!("{argv:?} must parse, got: {e}"));
    }
}

#[test]
fn global_scope_without_a_home_directory_is_an_error() {
    let dir = temp_dir("nohome");
    let err = config_path(
        Scope {
            global: true,
            project: false,
            explicit: None,
        },
        &dir,
        None,
    )
    .expect_err("no home dir means no global config path");
    assert_eq!(err.to_string(), "could not determine user config directory");
}

#[test]
fn adding_the_same_server_name_twice_is_refused() {
    let dir = temp_dir("dup");
    let path = dir.join("xaft.toml");
    add_mcp_server(&path, &spec("alpha", "srv")).expect("first add");
    let err =
        add_mcp_server(&path, &spec("alpha", "other")).expect_err("a duplicate name is refused");
    // DIVERGENCE: first written on the assumption that a duplicate add reuses
    // the `NotFound` wording. MEASURED: `add_mcp_server` has its own
    // already-exists message, which is the more useful of the two for a
    // duplicate (the user did not fail to find anything). Asserted as measured.
    assert_eq!(err.to_string(), "MCP server already exists: alpha");

    // Exactly one stanza survives, and it is the first one.
    let entries = read_mcp_servers(&path).expect("parses");
    assert_eq!(entries.len(), 1);
}

#[test]
fn removing_an_absent_server_reports_it_by_name() {
    let dir = temp_dir("rm-absent");
    let path = dir.join("xaft.toml");
    add_mcp_server(&path, &spec("alpha", "srv")).expect("add");

    let err = remove_missing(&path, "ghost");
    assert_eq!(err.to_string(), "MCP server not found: ghost");
    assert!(
        Path::new(&path).exists(),
        "a failed remove must not delete the file"
    );
}

fn remove_missing(path: &Path, name: &str) -> McpError {
    xaft_cli::mcp::remove_mcp_server(path, name).expect_err("removing an absent server fails")
}
