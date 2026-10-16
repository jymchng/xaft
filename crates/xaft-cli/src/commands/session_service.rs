//! `xaft session create|events|send|control|serve` - agenthicc's `session`
//! family from `cli/commands/session_service.py` (PRD-150).
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
//! agenthicc's `serve` (row 13) lives here too, as [`serve`]: the transport
//! itself is `xaft_session::LocalSessionServer`, because it is a property of
//! the service rather than of the CLI, but the command that runs it is this
//! module's fifth entry point. It is the one command in this file that does
//! not return.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use xaft_config::{CliOverrides, ConfigLoader};
use xaft_runtime::ExitCode;
use xaft_session::service::{CapabilitySet, SessionCommand, SessionService};
use xaft_session::transport::{LocalSessionServer, TransportError};

use crate::args::{
    SessionControlArgs, SessionCreateArgs, SessionEventsArgs, SessionSendArgs, SessionServeArgs,
};
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
    Ok(ExitCode::SUCCESS)
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
        println!("{}", serde_json::to_string(&event.to_value())?);
    }
    Ok(ExitCode::SUCCESS)
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
    Ok(ExitCode::SUCCESS)
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
    Ok(ExitCode::SUCCESS)
}

/// `xaft session serve` (matrix row 13).
///
/// Runs agenthicc's local attachment transport — `xaft_session::LocalSessionServer`,
/// the port of `session_service/transport.py` — in the foreground until the
/// process is interrupted. It never returns on its own, exactly like
/// agenthicc's, because there is nothing for it to return to: the command's
/// whole product is the listening socket.
///
/// # Not injected, unlike `export`/`inspect`
///
/// `serve` calls the module-private `service()` the same way
/// `create`/`events`/`send`/`control` do, rather than taking a `SessionBackend` from `main.rs`. The transport
/// serves the *service* store; the injected backend is the local session log,
/// whose sessions the service's routes cannot read. This is not a divergence
/// from agenthicc, which also constructs its `SessionService` inside the
/// command.
///
/// # The readiness line
///
/// One JSON object is printed to stdout before the wait begins. It is the only
/// way a supervisor that asked for `--port 0` can learn the port, so it is
/// printed before anything can block. The shape is agenthicc's, including the
/// `schedule_service` key: xaft has no schedules subsystem, so that field is
/// always `"disabled"`. Keeping the key means a client that parses agenthicc's
/// readiness line does not need a xaft branch.
pub async fn serve(args: &SessionServeArgs) -> Result<ExitCode, XaftError> {
    let mut server = start_server(args, service().await?).await?;
    println!("{}", readiness_line(&server.url()));

    // agenthicc waits on `asyncio.Event().wait()`, which nothing but the
    // task's own cancellation can end: no `--once`, no timeout. Ctrl-C is the
    // Rust equivalent of that cancellation, and `stop()` runs after it so the
    // listening port is released deliberately rather than by process exit.
    tokio::signal::ctrl_c().await?;
    server.stop().await;
    Ok(ExitCode::SUCCESS)
}

/// Bind and start the transport, or fail before any client can connect.
///
/// Split out of [`serve`] for one reason: `serve` never returns, so it cannot
/// be tested by asserting on a process exit. A test can call this, talk to the
/// real socket, and then [`LocalSessionServer::stop`] it.
async fn start_server(
    args: &SessionServeArgs,
    service: SessionService,
) -> Result<LocalSessionServer, XaftError> {
    let mut server = LocalSessionServer::new(
        service,
        &args.host,
        args.port,
        Some(args.auth_token.clone()),
    )
    .map_err(transport_error)?;
    server.start().await.map_err(transport_error)?;
    Ok(server)
}

/// Render a transport failure as a CLI usage error.
///
/// Both [`TransportError`] variants describe a bind the operator asked for and
/// cannot have — a non-loopback host with no token, or a port that will not
/// bind — so they exit 2 instead of half-starting. The message is the
/// transport's own: it names the refused host and names `--auth-token`, which
/// is the fix.
fn transport_error(error: TransportError) -> XaftError {
    XaftError::Usage(error.to_string())
}

/// The readiness document `serve` prints once, in agenthicc's shape.
///
/// Key order is sorted rather than agenthicc's insertion order, because
/// `serde_json`'s map is sorted by construction everywhere else in this
/// workspace; a JSON parser cannot tell the two apart.
fn readiness_line(url: &str) -> String {
    serde_json::json!({
        "status": "ready",
        "url": url,
        "schedule_service": "disabled",
    })
    .to_string()
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
        assert!(!document["events"].as_array().expect("array").is_empty());
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

    // ── The production entry points ─────────────────────────────────────────
    //
    // Everything above drives an `in_memory` service directly. These call the
    // real CLI entry points, so the production `service()` helper runs too: the
    // same `ConfigLoader::load` + `SessionService::open(&config.core.data_dir)`
    // pair the binary uses. That is the half of this module a user actually
    // reaches, and until now every line of it was uncovered.
    //
    // `XAFT_CORE__DATA_DIR` is how the loaded config is redirected at a
    // throwaway directory. It is process-global, so these tests serialise on
    // `ENV_LOCK` for the (short) window in which it is set.

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set `XAFT_CORE__DATA_DIR` for the duration of `f`, then restore it.
    fn with_data_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let previous = std::env::var_os("XAFT_CORE__DATA_DIR");
        // SAFETY: serialised by ENV_LOCK; no other test in this binary reads
        // this variable (`XAFT_HOME` is a different mechanism).
        unsafe { std::env::set_var("XAFT_CORE__DATA_DIR", dir) };
        let out = f();
        match previous {
            Some(v) => unsafe { std::env::set_var("XAFT_CORE__DATA_DIR", v) },
            None => unsafe { std::env::remove_var("XAFT_CORE__DATA_DIR") },
        }
        out
    }

    /// Create one session through the *service* (not the CLI) so the CLI tests
    /// below have an id to address.
    fn seed_session(rt: &tokio::runtime::Runtime, data_dir: &Path) -> String {
        let svc = rt
            .block_on(SessionService::open(data_dir))
            .expect("open service");
        rt.block_on(svc.create_session(
            data_dir,
            "cli",
            &caps_mut(),
            BTreeMap::new(),
            BTreeMap::new(),
        ))
        .expect("seed session")
        .session_id
    }

    #[test]
    fn create_opens_the_store_that_core_data_dir_names() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let args = SessionCreateArgs {
            project_root: Some(tmp.path().to_path_buf()),
            agent: "planner".into(),
            workflow: "build".into(),
        };

        let code = with_data_dir(&data_dir, || rt.block_on(create(&args))).expect("create");
        assert_eq!(code, ExitCode::SUCCESS);

        // The session landed in the store `core.data_dir` names -- not in some
        // private map -- because reopening that directory finds it.
        let svc = rt
            .block_on(SessionService::open(&data_dir))
            .expect("reopen service");
        let listed = rt
            .block_on(svc.list_sessions(None, &caps_mut()))
            .expect("list");
        assert_eq!(listed.len(), 1, "create must persist exactly one session");
        assert_eq!(
            listed[0].agent.get("name"),
            Some(&serde_json::json!("planner")),
            "the --agent flag must reach the snapshot"
        );
        assert_eq!(
            listed[0].workflow.get("name"),
            Some(&serde_json::json!("build")),
            "the --workflow flag must reach the snapshot"
        );
    }

    #[test]
    fn create_without_a_project_root_and_with_blank_flags_uses_the_cwd() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let args = SessionCreateArgs {
            project_root: None,
            agent: String::new(),
            workflow: "   ".into(),
        };

        with_data_dir(&data_dir, || rt.block_on(create(&args))).expect("create");

        let svc = rt
            .block_on(SessionService::open(&data_dir))
            .expect("reopen service");
        let listed = rt
            .block_on(svc.list_sessions(None, &caps_mut()))
            .expect("list");
        assert_eq!(listed.len(), 1);
        assert!(
            listed[0].agent.is_empty(),
            "a blank --agent must add no metadata key, got {:?}",
            listed[0].agent
        );
        assert!(
            listed[0].workflow.is_empty(),
            "a whitespace-only --workflow must add no metadata key, got {:?}",
            listed[0].workflow
        );
    }

    #[test]
    fn events_replays_the_durable_log_through_the_cli_entry_point() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let id = seed_session(&rt, &data_dir);

        let all = SessionEventsArgs {
            id: id.clone(),
            after: 0,
        };
        assert_eq!(
            with_data_dir(&data_dir, || rt.block_on(events(&all))).expect("events"),
            ExitCode::SUCCESS
        );

        // A cursor past the only event is an empty replay, not an error.
        let past = SessionEventsArgs {
            id: id.clone(),
            after: 1,
        };
        assert_eq!(
            with_data_dir(&data_dir, || rt.block_on(events(&past))).expect("events"),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn events_on_a_missing_session_surfaces_an_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let args = SessionEventsArgs {
            id: "sess_missing".into(),
            after: 0,
        };

        let err = with_data_dir(&data_dir, || rt.block_on(events(&args)))
            .expect_err("a typo'd id must not replay as an empty stream");
        assert!(
            err.to_string().contains("sess_missing"),
            "error should name the session, got: {err}"
        );
    }

    #[test]
    fn send_queues_a_message_through_the_cli_envelope() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let id = seed_session(&rt, &data_dir);
        let args = SessionSendArgs {
            id: id.clone(),
            message: "hello from the CLI".into(),
            json: false,
        };

        assert_eq!(
            with_data_dir(&data_dir, || rt.block_on(send(&args))).expect("send"),
            ExitCode::SUCCESS
        );

        // `send` submits `submit_message`, which queues a turn and appends it to
        // the durable log -- the observable effect, not just the exit code.
        let svc = rt
            .block_on(SessionService::open(&data_dir))
            .expect("reopen service");
        let log = rt
            .block_on(svc.events(&id, 0, &caps_mut()))
            .expect("events");
        assert!(
            log.iter().any(|e| e.kind == "turn_queued"),
            "send must append turn_queued, got {:?}",
            log.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn control_rejects_a_non_object_payload_before_it_opens_the_store() {
        let args = SessionControlArgs {
            id: "sess_never_opened".into(),
            kind: "cancel".into(),
            payload: Some("[1,2]".into()),
            json: false,
        };

        // No data dir is configured here on purpose: `decode_payload` runs
        // first, so this must fail on the payload rather than reach the store.
        let err = rt().block_on(control(&args)).expect_err("list payload");
        assert!(
            matches!(err, XaftError::Usage(_)),
            "expected a usage error, got {err:?}"
        );
        assert!(err.to_string().contains("payload must be a JSON object"));
    }

    #[test]
    fn control_submits_an_empty_payload_by_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let id = seed_session(&rt, &data_dir);
        let args = SessionControlArgs {
            id: id.clone(),
            kind: "cancel".into(),
            payload: None,
            json: false,
        };

        assert_eq!(
            with_data_dir(&data_dir, || rt.block_on(control(&args))).expect("control"),
            ExitCode::SUCCESS
        );

        let svc = rt
            .block_on(SessionService::open(&data_dir))
            .expect("reopen service");
        let snap = rt.block_on(svc.snapshot(&id, &caps_mut())).expect("snap");
        assert!(
            format!("{:?}", snap.state).contains("Cancelled"),
            "cancel must reach the state machine, got {:?}",
            snap.state
        );
    }

    #[test]
    fn export_document_is_readable_and_write_json_document_is_pretty_and_newline_terminated() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_dir = tmp.path().join("data");
        let rt = rt();
        let id = seed_session(&rt, &data_dir);

        let document = with_data_dir(&data_dir, || rt.block_on(export_document(&id)))
            .expect("export_document");
        assert_eq!(document["schema_version"], serde_json::json!(1));
        assert_eq!(document["snapshot"]["session_id"], serde_json::json!(id));
        assert!(!document["events"].as_array().expect("array").is_empty());

        let out = tmp.path().join("export.json");
        write_json_document(&out, &document).expect("write");
        let text = std::fs::read_to_string(&out).expect("read back");
        assert!(text.ends_with('\n'), "document must be newline terminated");
        assert!(
            text.contains("\n  \""),
            "document must be pretty-printed with two-space indent"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).expect("reparse"),
            document,
            "write_json_document must round-trip byte-for-byte through JSON"
        );
    }

    // ── `serve` (matrix row 13) ─────────────────────────────────────────────

    /// `serve`'s arguments, with the empty token agenthicc's default implies.
    fn serve_args(host: &str, port: u16, auth_token: &str) -> SessionServeArgs {
        SessionServeArgs {
            host: host.to_string(),
            port,
            auth_token: auth_token.to_string(),
        }
    }

    /// One raw HTTP/1.1 request against `addr`, returning the whole response.
    ///
    /// Hand-written rather than pulled from a client crate for the same reason
    /// the transport is hand-written: there is no HTTP client in the graph, and
    /// a test is not a good enough reason to add one.
    fn raw_request(rt: &tokio::runtime::Runtime, addr: std::net::SocketAddr, raw: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        rt.block_on(async {
            let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            stream.write_all(raw.as_bytes()).await.expect("write");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).await.expect("read");
            String::from_utf8_lossy(&buf).into_owned()
        })
    }

    /// `--port 0` must reach a real socket, and the readiness line must carry
    /// the port that was actually bound.
    ///
    /// The routes are tested in `xaft-session`; what this pins is the CLI half,
    /// which no transport test can: that the parsed flags are the ones the
    /// server was built with, and that a supervisor reading the readiness line
    /// is told the bound port rather than the requested `0`.
    #[test]
    fn serve_binds_a_free_port_and_reports_the_bound_one() {
        let (rt, svc) = service();
        let mut server = rt
            .block_on(start_server(&serve_args("127.0.0.1", 0, ""), svc))
            .expect("start");

        let readiness: serde_json::Value =
            serde_json::from_str(&readiness_line(&server.url())).expect("readiness is JSON");
        assert_eq!(readiness["status"], serde_json::json!("ready"));
        assert_eq!(
            readiness["schedule_service"],
            serde_json::json!("disabled"),
            "xaft has no schedules subsystem; the key is kept for agenthicc clients"
        );

        let addr = server.local_addr().expect("bound");
        assert_ne!(addr.port(), 0, "--port 0 must bind a real port");
        assert_eq!(
            readiness["url"],
            serde_json::json!(format!("http://127.0.0.1:{}", addr.port())),
            "the readiness line must name the bound port, not the requested one"
        );

        let response = raw_request(&rt, addr, "GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(response.contains("200 OK"), "health was {response}");
        assert!(
            response.contains("xaft-session"),
            "health must identify the service: {response}"
        );

        rt.block_on(server.stop());
    }

    /// A refused bind is a usage error that names `--auth-token`, and nothing
    /// is listening afterwards.
    #[test]
    fn serve_refuses_a_non_loopback_host_without_a_token() {
        let (rt, svc) = service();
        let error = rt
            .block_on(start_server(&serve_args("0.0.0.0", 0, ""), svc))
            .expect_err("a non-loopback bind with no token must be refused");

        assert!(
            matches!(error, XaftError::Usage(_)),
            "a refused bind must be a usage error, got {error:?}"
        );
        assert!(
            error.to_string().contains("--auth-token"),
            "the error must name the fix: {error}"
        );
        assert_eq!(error.exit_code(), ExitCode::USAGE_ERROR);
    }

    /// `--auth-token` must reach the transport, and an unauthenticated request
    /// must be refused — including `/health`, which is the route a probing
    /// client would try first.
    #[test]
    fn serve_enforces_the_auth_token_on_every_route() {
        let (rt, svc) = service();
        let mut server = rt
            .block_on(start_server(&serve_args("127.0.0.1", 0, "s3cret"), svc))
            .expect("start");
        let addr = server.local_addr().expect("bound");

        let unauthenticated = raw_request(&rt, addr, "GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(
            unauthenticated.contains("401"),
            "an unauthenticated /health must be refused: {unauthenticated}"
        );
        assert!(
            !unauthenticated.contains("s3cret"),
            "the response must not echo the token"
        );

        let authenticated = raw_request(
            &rt,
            addr,
            "GET /health HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer s3cret\r\n\r\n",
        );
        assert!(
            authenticated.contains("200 OK"),
            "the configured bearer token must be accepted: {authenticated}"
        );
        assert!(
            !authenticated.contains("s3cret"),
            "the response must not echo the token"
        );

        rt.block_on(server.stop());
    }

    /// The readiness line is the documented document, not whatever `serve`
    /// happens to print: `status`, `url` and agenthicc's `schedule_service`.
    #[test]
    fn readiness_line_is_the_three_key_document() {
        let parsed: serde_json::Value =
            serde_json::from_str(&readiness_line("http://127.0.0.1:8787")).expect("JSON");
        assert_eq!(
            parsed,
            serde_json::json!({
                "status": "ready",
                "url": "http://127.0.0.1:8787",
                "schedule_service": "disabled",
            })
        );
    }
}
