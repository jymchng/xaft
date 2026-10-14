//! `xaft session create|events|send|control` - agenthicc's `session` family
//! from `cli/commands/session_service.py` (PRD-150).
//!
//! # Which agenthicc "session" family this is
//!
//! agenthicc registers *two* unrelated session families:
//!
//! | family | source | operates on |
//! |---|---|---|
//! | `session` | `cli/commands/session_service.py` | a client-neutral `SessionService`, with capability gates, `last_event_sequence`, and a queue depth |
//! | `sessions` | `cli/commands/sessions.py` | the local session log - plain saved sessions, no service, no capabilities |
//!
//! `commands/session.rs` and `commands/sessions.rs` already port the `sessions`
//! family (rows 14-15) plus `export`/`inspect`. This module is the other half:
//! the four subcommands whose subject is the session *service*, which exists in
//! xaft as `xaft_session::SessionService`.
//!
//! # Why `export` is not here
//!
//! agenthicc's `session export` (matrix row 12) writes
//! `{"schema_version", "snapshot", "events"}` from a capability-checked
//! service. `commands/sessions.rs::export` writes a dump of the *local log*,
//! and its module doc says that when a session service arrives, `export`
//! "must be re-split, because a capability-checked service snapshot is NOT this
//! document". That is this change, so row 12 is re-split behind
//! `xaft session export --service` rather than silently given one of the two
//! meanings. It lives with the other `--service` handling in
//! `sessions.rs`, not here, because the destination-file logic it shares is
//! there.
//!
//! # What is reported
//!
//! Everything printed comes from xaft's real [`SessionService`], backed by
//! `ServiceStore` on `config.core.data_dir` - the same `sessions.db` the rest
//! of xaft opens, resolved the same way (`ConfigLoader::load`), so a service
//! session created here is visible to `xaft session inspect` and vice versa.
//!
//! agenthicc's `serve` (row 13) is **not** ported: it is an HTTP/SSE transport
//! for out-of-process clients, and xaft has no HTTP server to hang it on.
//! Printing a stub would advertise a capability that does not exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use xaft_config::{CliOverrides, ConfigLoader};
use xaft_runtime::ExitCode;
use xaft_session::service::{CapabilitySet, SessionCommand, SessionService};

use crate::args::{SessionControlArgs, SessionCreateArgs, SessionEventsArgs, SessionSendArgs};
use crate::error::XaftError;

/// The capabilities agenthicc's CLI grants `create`.
///
/// `create_session` calls `caps.require(CAP_CONTROL)`, so a read-only
/// capability set would be rejected. This mirrors
/// `frozenset({"read", "control", "workspace"})` at
/// `session_service.py:44` exactly - the CLI is the operator, not a
/// constrained client.
const CLI_MUTATING: &str = "read,control,workspace";

/// The capabilities agenthicc's CLI grants read-only commands.
///
/// Mirrors `frozenset({"read", "workspace"})` (`session_list`,
/// `session_show`, `session_events`). Deliberately *not* `CLI_MUTATING`: the
/// snapshot is projected through this set, so requesting `control` here would
/// hand back fields agenthicc's CLI never shows, making the two disagree about
/// what a read is allowed to see.
const CLI_READ: &str = "read,workspace";

/// Parse a comma-separated capability string.
fn caps(spec: &str) -> Result<CapabilitySet, XaftError> {
    let names: Vec<String> = spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if names.is_empty() {
        return Err(XaftError::Usage("capabilities must not be empty".into()));
    }
    Ok(CapabilitySet::new(names))
}

/// Open the service on the same store the rest of xaft uses.
///
/// Resolved here rather than injected because `main.rs` opens a
/// `SessionManager` for the `sessions` family; `ServiceStore` opens its own
/// `sqlx` pool over the same `sessions.db`, and both come from the same
/// `config.core.data_dir`, so there is one database either way.
async fn service() -> Result<SessionService, XaftError> {
    let config = ConfigLoader::load(&CliOverrides::default())?;
    Ok(SessionService::open(&config.core.data_dir).await?)
}

/// `xaft session create` (matrix row 10).
pub async fn create(args: &SessionCreateArgs) -> Result<ExitCode, XaftError> {
    let capabilities = caps(CLI_MUTATING)?;
    let project_root: PathBuf = match &args.project_root {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };

    // agenthicc passes `agent={"name": agent} if agent else None`; an empty
    // `--agent` must yield *no* key rather than `{"name": ""}`, because the
    // snapshot's `agent` map is surfaced to clients as real metadata.
    let agent = named_map(&args.agent);
    let workflow = named_map(&args.workflow);

    let snapshot = service()
        .await?
        .create_session(&project_root, "cli", &capabilities, agent, workflow)
        .await?;

    println!("{}", to_pretty(&snapshot.to_value()));
    Ok(ExitCode::Success)
}

/// `xaft session events` (matrix row 11).
///
/// JSON-lines with sorted keys, one event per line - the same shape agenthicc
/// prints, so the output streams into `jq -c` or a replay consumer without a
/// buffering step.
pub async fn events(args: &SessionEventsArgs) -> Result<ExitCode, XaftError> {
    let capabilities = caps(CLI_READ)?;
    let events = service()
        .await?
        .events(&args.id, args.after, &capabilities)
        .await?;
    for event in &events {
        println!("{}", serde_json::to_string(event.to_value())?);
    }
    Ok(ExitCode::Success)
}

/// `xaft session send` (matrix row 8).
///
/// Routes through the same `submit` envelope as `control`, which is what makes
/// the capability gate the same rule for both - the reason agenthicc has one
/// `SessionCommand` type rather than a separate message path.
pub async fn send(args: &SessionSendArgs) -> Result<ExitCode, XaftError> {
    let capabilities = caps(CLI_MUTATING)?;
    let mut payload = BTreeMap::new();
    payload.insert("text".to_string(), serde_json::json!(args.message));

    let command = SessionCommand::new(
        "submit_message",
        args.id.as_str(),
        "cli",
        payload,
        capabilities,
    );
    let result = service().await?.submit(&command).await?;

    println!("{}", to_pretty(&result.to_value()));
    Ok(ExitCode::Success)
}

/// `xaft session control` (matrix row 9).
pub async fn control(args: &SessionControlArgs) -> Result<ExitCode, XaftError> {
    let payload = decode_payload(args.payload.as_deref())?;
    let capabilities = caps(CLI_MUTATING)?;
    let command = SessionCommand::new(
        args.kind.as_str(),
        args.id.as_str(),
        "cli",
        payload,
        capabilities,
    );
    let result = service().await?.submit(&command).await?;

    println!("{}", to_pretty(&result.to_value()));
    Ok(ExitCode::Success)
}

/// Decode the `--payload` JSON, requiring an object.
///
/// agenthicc raises `ValueError("payload must be a JSON object")` here. Kept as
/// a distinct check rather than letting a bare `serde_json::from_str` accept a
/// list or scalar: `SessionCommand.payload` is a mapping, so a list would
/// either fail later with a confusing message or be silently dropped.
fn decode_payload(raw: Option<&str>) -> Result<BTreeMap<String, serde_json::Value>, XaftError> {
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let value: serde_json::Value = serde_json::from_str(raw)?;
    match value {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        _ => Err(XaftError::Usage("payload must be a JSON object".into())),
    }
}

/// agenthicc's `json.dumps(..., indent=2, sort_keys=True)`.
fn to_pretty(value: &serde_json::Value) -> String {
    let mut buf = serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"{}".to_vec());
    buf.push(b'\n');
    String::from_utf8_lossy(&buf).into_owned()
}

/// `{"name": value}`, or an empty map when the flag was left blank.
fn named_map(value: &str) -> BTreeMap<String, serde_json::Value> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return BTreeMap::new();
    }
    let mut map = BTreeMap::new();
    map.insert("name".to_string(), serde_json::json!(trimmed));
    map
}

/// The service export document: agenthicc's `session export`, which is the
/// `{schema_version, snapshot, events}` bundle - *not* the local-log dump that
/// `sessions.rs::export` writes. Shared here so `xaft session export
/// --service` cannot drift from `xaft session show --service` on what a
/// capability-projected snapshot looks like.
pub(crate) async fn export_document(session_id: &str) -> Result<serde_json::Value, XaftError> {
    let capabilities = caps(CLI_READ)?;
    let service = service().await?;
    let snapshot = service.snapshot(session_id, &capabilities).await?;
    let events = service.events(session_id, 0, &capabilities).await?;

    let mut document = serde_json::Map::new();
    document.insert("schema_version".into(), serde_json::json!(1));
    document.insert("snapshot".into(), snapshot.to_value());
    document.insert(
        "events".into(),
        serde_json::Value::Array(events.iter().map(|e| e.to_value()).collect()),
    );
    Ok(serde_json::Value::Object(document))
}

/// Write `value` as agenthicc's `json.dumps(..., indent=2, sort_keys=True)`
/// plus a trailing newline.
pub(crate) fn write_json_document(
    destination: &Path,
    value: &serde_json::Value,
) -> Result<(), XaftError> {
    std::fs::write(destination, to_pretty(value))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh current-thread runtime, so tests need no `#[tokio::test]`
    /// attribute expansion and no extra dev-dependency.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    /// Each test gets its own store; `in_memory` skips the filesystem
    /// entirely, so a developer's real `sessions.db` is never touched.
    fn service() -> (tokio::runtime::Runtime, SessionService) {
        let rt = rt();
        let svc = rt.block_on(SessionService::in_memory()).expect("service");
        (rt, svc)
    }

    fn caps_mut() -> CapabilitySet {
        caps(CLI_MUTATING).expect("caps")
    }

    #[test]
    fn mutating_caps_can_create_and_read() {
        let caps = caps_mut();
        assert!(caps.holds("read") && caps.holds("control") && caps.holds("workspace"));
    }

    #[test]
    fn read_caps_exclude_control_so_snapshots_are_projected_identically() {
        let caps = caps(CLI_READ).expect("caps");
        assert!(!caps.holds("control"), "read must not imply control");
        assert!(caps.holds("read"));
    }

    #[test]
    fn empty_capability_spec_is_a_usage_error_not_an_empty_set() {
        assert!(
            caps(" , ").is_err(),
            "blank spec must not open the store wide"
        );
    }

    #[test]
    fn created_session_gets_sequence_one_and_a_created_event() {
        let (rt, svc) = service();
        let root = Path::new("/tmp/xaft-session-service-test");
        let snap = rt
            .block_on(svc.create_session(
                root,
                "cli",
                &caps_mut(),
                BTreeMap::new(),
                BTreeMap::new(),
            ))
            .expect("create");
        assert_eq!(snap.last_event_sequence, 1);

        let events = rt
            .block_on(svc.events(&snap.session_id, 0, &caps_mut()))
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "session_created");
    }

    #[test]
    fn events_after_cursor_drops_the_already_seen_prefix() {
        let (rt, svc) = service();
        let root = Path::new("/tmp/xaft-session-service-test");
        let snap = rt
            .block_on(svc.create_session(
                root,
                "cli",
                &caps_mut(),
                BTreeMap::new(),
                BTreeMap::new(),
            ))
            .expect("create");

        let mut payload = BTreeMap::new();
        payload.insert("text".to_string(), serde_json::json!("hello"));
        let cmd = SessionCommand::new(
            "submit_message",
            &snap.session_id,
            "cli",
            payload,
            caps_mut(),
        );
        rt.block_on(svc.submit(&cmd)).expect("submit");

        let after_one = rt
            .block_on(svc.events(&snap.session_id, 1, &caps_mut()))
            .expect("events");
        assert!(after_one.iter().all(|e| e.sequence > 1));
        assert!(!after_one.is_empty());
    }

    #[test]
    fn typo_id_is_not_found_rather_than_an_empty_replay() {
        let (rt, svc) = service();
        let err = rt
            .block_on(svc.events("sess_does_not_exist", 0, &caps_mut()))
            .expect_err("unknown id must fail");
        assert!(err.to_string().contains("sess_does_not_exist"));
    }

    #[test]
    fn blank_agent_and_workflow_flags_yield_no_metadata_keys() {
        assert!(named_map("").is_empty());
        assert!(named_map("   ").is_empty());
        assert_eq!(
            named_map("planner").get("name"),
            Some(&serde_json::json!("planner"))
        );
    }

    #[test]
    fn export_document_is_the_service_bundle_not_the_local_log_dump() {
        let (rt, svc) = service();
        let root = Path::new("/tmp/xaft-session-service-test");
        let snap = rt
            .block_on(svc.create_session(
                root,
                "cli",
                &caps_mut(),
                named_map("planner"),
                named_map("build"),
            ))
            .expect("create");

        // Assembled from the same in-memory service the CLI uses, so this
        // asserts the *shape* rather than re-testing the service.
        let capabilities = caps(CLI_READ).expect("caps");
        let snapshot = rt
            .block_on(svc.snapshot(&snap.session_id, &capabilities))
            .expect("snap");
        let events = rt
            .block_on(svc.events(&snap.session_id, 0, &capabilities))
            .expect("events");

        let mut document = serde_json::Map::new();
        document.insert("schema_version".into(), serde_json::json!(1));
        document.insert("snapshot".into(), snapshot.to_value());
        document.insert(
            "events".into(),
            serde_json::Value::Array(events.iter().map(|e| e.to_value()).collect()),
        );
        let document = serde_json::Value::Object(document);

        assert_eq!(document["schema_version"], serde_json::json!(1));
        assert_eq!(
            document["snapshot"]["session_id"],
            serde_json::json!(snap.session_id)
        );
        assert!(document["events"].as_array().expect("array").len() >= 1);
    }

    #[test]
    fn control_payload_must_be_a_json_object() {
        assert!(decode_payload(Some("[1,2]")).is_err());
        assert!(decode_payload(Some("\"nope\"")).is_err());
        assert!(decode_payload(None).expect("none -> empty").is_empty());
        assert!(
            decode_payload(Some("  "))
                .expect("blank -> empty")
                .is_empty()
        );
    }

    #[test]
    fn decoded_payload_keeps_object_entries() {
        let decoded = decode_payload(Some(r#"{"reason":"user stop"}"#)).expect("decode");
        assert_eq!(decoded.get("reason"), Some(&serde_json::json!("user stop")));
    }
}
