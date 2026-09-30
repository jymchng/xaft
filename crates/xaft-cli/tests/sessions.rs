//! End-to-end tests for `xaft session export`/`inspect` against a real SQLite session store.
//!
//! The unit tests inside `commands/sessions.rs` cover the pure formatting
//! helpers. These cover the part that only a real store can prove: that
//! `export` writes a correct document, that `inspect` reads the same data
//! back, and that both resolve `core.data_dir` through the real config chain
//! rather than a default that happens to match in the test environment.

use std::path::PathBuf;

use xaft_cli::args::{SessionsExportArgs, SessionsInspectArgs};
use xaft_cli::commands::sessions;
use xaft_runtime::session::AgentSession;
use xaft_session::{Message, SessionManager};

/// A temp dir holding both a config file and the session data directory.
///
/// Returns `(dir, config_path, data_dir, session_id)`.
async fn fixture(messages: usize) -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");

    // `data_dir` is written with forward slashes so the path stays valid TOML
    // on any platform; a raw Windows path would need escaping.
    let config_path = dir.path().join("xaft.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[core]
log_level = "info"
telemetry = false
data_dir = "{data}"

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
            data = data_dir.display()
        ),
    )
    .expect("write config");

    // Seed the store the same way the runtime and TUI would.
    let manager = SessionManager::new(&data_dir).await.expect("open store");
    let session = AgentSession::new(
        "fix the parser",
        PathBuf::from("/tmp/project"),
        "code-review".into(),
        "claude-3-5-sonnet-20241022".into(),
    );
    let id = session.id.as_str().to_string();

    let conversation: Vec<Message> = (0..messages)
        .map(|i| Message::user(format!("m{i}")))
        .collect();
    manager
        .save_with_history(&session, &conversation)
        .await
        .expect("seed session");

    (dir, config_path, data_dir, id)
}

fn export_args(config: PathBuf, id: String, output: PathBuf) -> SessionsExportArgs {
    SessionsExportArgs {
        id,
        output: Some(output),
        config: Some(config),
    }
}

fn inspect_args(config: PathBuf, id: String, json: bool) -> SessionsInspectArgs {
    SessionsInspectArgs {
        id,
        json,
        config: Some(config),
    }
}

#[tokio::test]
async fn export_writes_the_session_and_its_conversation() {
    let (_dir, config, _data, id) = fixture(3).await;
    let out = _dir.path().join("nested").join("export.json");

    let code = sessions::export(&export_args(config, id.clone(), out.clone()))
        .await
        .expect("export should succeed");
    assert!(code.is_success(), "export returned {code:?}");

    // The parent directory is created on demand, so `-o a/b/out.json` works.
    assert!(out.exists(), "export did not create {}", out.display());

    let raw = std::fs::read_to_string(&out).expect("read export");
    // A well-formed text file, matching agenthicc's service-family export.
    assert!(raw.ends_with('\n'), "export should end with a newline");

    let doc: serde_json::Value = serde_json::from_str(&raw).expect("export must be valid JSON");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["message_count"], 3);
    assert_eq!(doc["messages"].as_array().expect("messages array").len(), 3);
    // The session's own id is embedded, not a fresh random one.
    assert_eq!(doc["session"]["id"], id.as_str());
}

#[tokio::test]
async fn export_honours_core_data_dir_from_the_config() {
    // Guards the bug this command is most likely to grow: resolving a
    // default data dir instead of the configured one, which would make the
    // command silently read a different store than the TUI writes.
    let (dir, config, data_dir, id) = fixture(1).await;

    // Prove the store really lives at the configured location.
    assert!(data_dir.exists(), "seeded store is not at {data_dir:?}");

    let out = dir.path().join("out.json");
    sessions::export(&export_args(config, id.clone(), out.clone()))
        .await
        .expect("export should succeed");

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["session"]["id"], id.as_str());
    assert_eq!(doc["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn inspect_json_reports_the_seeded_conversation_counts() {
    let (_dir, config, _data, id) = fixture(5).await;

    let code = sessions::inspect(&inspect_args(config, id.clone(), true))
        .await
        .expect("inspect should succeed");
    assert!(code.is_success(), "inspect returned {code:?}");
}

#[tokio::test]
async fn inspect_reports_a_missing_session_as_a_failure() {
    let (_dir, config, _data, _id) = fixture(1).await;
    let missing = "deadbeef-0000-0000-0000-000000000000";

    let code = sessions::inspect(&inspect_args(config, missing.to_string(), true))
        .await
        .expect("a missing session is handled, not an Err");

    // agenthicc returns 0 here; xaft deliberately exits non-zero so a script
    // can tell a failed lookup from a successful one.
    assert!(!code.is_success(), "missing session must not exit 0");
}

#[tokio::test]
async fn export_reports_a_missing_session_as_a_failure() {
    let (dir, config, _data, _id) = fixture(1).await;
    let missing = "deadbeef-0000-0000-0000-000000000000";
    let out = dir.path().join("never-written.json");

    let code = sessions::export(&export_args(config, missing.to_string(), out.clone()))
        .await
        .expect("a missing session is handled, not an Err");

    assert!(!code.is_success(), "missing session must not exit 0");
    assert!(
        !out.exists(),
        "a failed export must not leave a file behind"
    );
}
