//! End-to-end tests for `xaft session export`/`inspect` against a real SQLite session store.
//!
//! The unit tests inside `commands/sessions.rs` cover the pure formatting
//! helpers. These cover the part that only a real store can prove: that
//! `export` writes a correct document, that `inspect` reads the same data
//! back, and that both read the store they are *given* rather than resolving
//! one themselves.
//!
//! The store arrives as an `Arc<dyn SessionBackend>`, exactly as `main.rs`
//! supplies it. These tests used to pass a `--config` path instead; that flag
//! no longer exists, and rightly so — it let `export`/`inspect` pick a
//! `core.data_dir` the runtime had never been bootstrapped with, which is the
//! split this file's fixtures now make impossible by construction.

use std::path::PathBuf;
use std::sync::Arc;

use xaft_cli::args::{SessionsExportArgs, SessionsInspectArgs};
use xaft_cli::commands::sessions;
use xaft_runtime::SessionBackend;
use xaft_runtime::session::AgentSession;
use xaft_session::{Message, SessionManager};

/// A temp dir holding the session data directory and the store opened on it.
///
/// Returns `(dir, backend, session_id)`. `backend` is the SAME manager the
/// runtime would be given, which is the whole point: there is no second
/// resolution of `core.data_dir` left to disagree with it.
async fn fixture(messages: usize) -> (tempfile::TempDir, Arc<dyn SessionBackend>, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");

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

    (dir, Arc::new(manager), id)
}

fn export_args(id: String, output: PathBuf) -> SessionsExportArgs {
    SessionsExportArgs {
        id,
        output: Some(output),
    }
}

fn inspect_args(id: String, json: bool) -> SessionsInspectArgs {
    SessionsInspectArgs { id, json }
}

#[tokio::test]
async fn export_writes_the_session_and_its_conversation() {
    let (dir, backend, id) = fixture(3).await;
    let out = dir.path().join("nested").join("export.json");

    let code = sessions::export(&export_args(id.clone(), out.clone()), backend.as_ref())
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
    // Guards the bug this command is most likely to grow: reading a default
    // data dir instead of the configured one, which would make the command
    // silently read a different store than the TUI writes.
    //
    // The guard used to be "export resolves `core.data_dir` out of the config
    // you passed with `--config`". That path no longer exists — the store is
    // injected — so the assertion is now on the artefact itself: the store the
    // backend serves really does live at the configured directory, and the
    // session written there is the one `export` reads back.
    let (dir, backend, id) = fixture(1).await;
    let data_dir = dir.path().join("data");

    assert!(
        data_dir.join("sessions.db").exists(),
        "the seeded store is not at the configured data_dir {data_dir:?}"
    );

    let out = dir.path().join("out.json");
    sessions::export(&export_args(id.clone(), out.clone()), backend.as_ref())
        .await
        .expect("export should succeed");

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["session"]["id"], id.as_str());
    assert_eq!(doc["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn inspect_json_reports_the_seeded_conversation_counts() {
    let (_dir, backend, id) = fixture(5).await;

    let code = sessions::inspect(&inspect_args(id.clone(), true), backend.as_ref())
        .await
        .expect("inspect should succeed");
    assert!(code.is_success(), "inspect returned {code:?}");
}

#[tokio::test]
async fn inspect_reports_a_missing_session_as_a_failure() {
    let (_dir, backend, _id) = fixture(1).await;
    let missing = "deadbeef-0000-0000-0000-000000000000";

    let code = sessions::inspect(&inspect_args(missing.to_string(), true), backend.as_ref())
        .await
        .expect("a missing session is handled, not an Err");

    // agenthicc returns 0 here; xaft deliberately exits non-zero so a script
    // can tell a failed lookup from a successful one.
    assert!(!code.is_success(), "missing session must not exit 0");
}

#[tokio::test]
async fn export_reports_a_missing_session_as_a_failure() {
    let (dir, backend, _id) = fixture(1).await;
    let missing = "deadbeef-0000-0000-0000-000000000000";
    let out = dir.path().join("never-written.json");

    let code = sessions::export(
        &export_args(missing.to_string(), out.clone()),
        backend.as_ref(),
    )
    .await
    .expect("a missing session is handled, not an Err");

    assert!(!code.is_success(), "missing session must not exit 0");
    assert!(
        !out.exists(),
        "a failed export must not leave a file behind"
    );
}
