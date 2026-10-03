//! Integration tests for the four background-terminal tools (parity rows
//! 36/43/44/45) and for `run_command`'s `background: true` routing.
//!
//! These drive the tools the way an agent does — through the real
//! [`Tool`] trait and a real [`ToolRegistryBuilder`] — rather than calling the
//! manager directly. A test that only exercises `TerminalManager` would pass even
//! if the tools were never registered, which is the exact false-parity shape
//! `tool_parity_reachability.rs` exists to catch.
//!
//! Every security property asserted here is one that **fails loudly if the
//! corresponding code is removed**:
//!
//! * `an_orphaned_terminal_cannot_be_stopped_through_the_tool` — remove the
//!   `entries` lookup in `TerminalManager::stop` and `stop_terminal` becomes an
//!   arbitrary-kill primitive over recycled PIDs.
//! * `stop_reaches_a_grandchild_through_the_process_group_via_the_tool` — signal
//!   the child instead of the group and `sleep 100 &` survives.
//! * `a_background_terminal_refuses_a_cwd_outside_the_workspace` — drop the
//!   containment check and the escape succeeds silently.
//! * `output_is_redacted_before_it_is_persisted_and_returned` — drop redaction
//!   and a `cat .env` leaves credentials in the JSONL transcript.

use std::sync::Arc;

use agtrs_runtime::tool::{Tool, ToolContext};
use serde_json::Value;
use tempfile::TempDir;
use xaft_tools::{
    InspectTerminalTool, StopTerminalTool, TerminalManager, TerminalManagerConfig,
    ToolRegistryBuilder, WaitTerminalReadyTool, WaitTerminalTool,
};

/// A manager over a temp workspace, leaked so the store outlives the test.
fn manager_for(dir: &TempDir, session: &str) -> Arc<TerminalManager> {
    let cfg = TerminalManagerConfig::new(session, dir.path());
    Arc::new(TerminalManager::new(cfg))
}

/// Parse a tool result's JSON body.
fn body(result: &agtrs_runtime::tool::ToolResult) -> Value {
    serde_json::from_str(&result.content).unwrap_or_else(|e| {
        panic!("tool result is not JSON ({e}): {}", result.content);
    })
}

/// Start `sleep 30` as a background terminal and return its id.
async fn start_sleeper(manager: &TerminalManager, cwd: &str) -> String {
    let argv = vec!["sleep".to_string(), "30".to_string()];
    let result = manager
        .start(
            None,
            Some(argv.as_slice()),
            Some(cwd),
            0.0,
            None,
            false,
            "sleeper",
            "call-1",
            "oneshot",
            None,
        )
        .await;
    let id = result
        .get("terminal_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no terminal_id in {}", result))
        .to_string();
    manager.stop(&id, true, "test cleanup").await;
    id
}

/// Whether a pid is still running, for the grandchild assertions.
///
/// `kill(pid, 0)` sends no signal; it only reports existence. A reparented
/// orphan that has not yet been reaped still answers, so this is polled rather
/// than sampled once.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // Safety: signal 0 performs the permission and existence checks only.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

/// The whole loop, through the real registry: start → inspect → wait → stop.
///
/// This is the acceptance test for the subsystem. If the tools are registered
/// but cannot be reached by name, or a terminal started by one cannot be named
/// by another, this fails.
#[tokio::test]
async fn a_background_run_command_round_trips_start_inspect_wait_stop() {
    let tmp = TempDir::new().unwrap();
    let manager = manager_for(&tmp, "round-trip");

    let registry = ToolRegistryBuilder::new(tmp.path())
        .without_git()
        .with_shell()
        .with_terminal_manager(Arc::clone(&manager))
        .build_coder()
        .expect("build coder registry");

    // --- the four tools are reachable BY NAME from a real registry ---------
    for name in [
        "inspect_terminal",
        "wait_terminal",
        "wait_terminal_ready",
        "stop_terminal",
        "run_command",
    ] {
        assert!(
            registry.get(name).is_some(),
            "{name} is not registered in a real ToolRegistryBuilder registry"
        );
    }

    let ctx = ToolContext::new("round-trip-1");
    let start = registry
        .get("run_command")
        .unwrap()
        .call(
            serde_json::json!({
                "argv": ["/bin/sh", "-c", "printf 'ready\\n'; sleep 30"],
                "cwd": ".",
                "background": true,
                "label": "round-trip",
            }),
            &ctx,
        )
        .await
        .expect("run_command must not error at the transport layer");

    let started = body(&start);
    assert_eq!(started["ok"], true, "start refused: {started}");
    assert_eq!(started["background"], true, "{started}");
    let id = started["terminal_id"]
        .as_str()
        .expect("terminal_id")
        .to_string();

    // --- inspect ---------------------------------------------------------
    let inspected = body(
        &registry
            .get("inspect_terminal")
            .unwrap()
            .call(serde_json::json!({ "terminal_id": id }), &ctx)
            .await
            .unwrap(),
    );
    assert_eq!(inspected["terminal_id"], id.as_str(), "{inspected}");
    assert_eq!(inspected["label"], "round-trip", "{inspected}");
    assert!(
        inspected["state"] == "running" || inspected["state"] == "exited",
        "unexpected state: {inspected}"
    );

    // --- wait with a bounded observation (must NOT kill the process) ------
    let waited = body(
        &registry
            .get("wait_terminal")
            .unwrap()
            .call(
                serde_json::json!({ "terminal_id": id, "timeout": 0.2 }),
                &ctx,
            )
            .await
            .unwrap(),
    );
    assert_eq!(waited["terminal_id"], id.as_str(), "{waited}");
    assert_eq!(
        waited["timed_out"], false,
        "an observer timeout must not mark the terminal timed out: {waited}"
    );

    // --- readiness on a terminal with no probe is a no-op, not a failure --
    let ready = body(
        &registry
            .get("wait_terminal_ready")
            .unwrap()
            .call(serde_json::json!({ "terminal_id": id }), &ctx)
            .await
            .unwrap(),
    );
    assert_eq!(ready["terminal_id"], id.as_str(), "{ready}");

    // --- stop ------------------------------------------------------------
    let stopped = body(
        &registry
            .get("stop_terminal")
            .unwrap()
            .call(
                serde_json::json!({ "terminal_id": id, "force": true }),
                &ctx,
            )
            .await
            .unwrap(),
    );
    assert_eq!(
        stopped["stop_requested"], true,
        "a manager-owned terminal must be signallable: {stopped}"
    );
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    // Stopping is not the success the agent asked for, so `ok` must be false.
    assert_eq!(
        stopped["ok"], false,
        "a stopped terminal is not ok: {stopped}"
    );
}

/// The arbitrary-kill property, asserted at the tool boundary.
///
/// A record whose state is `orphaned` carries a PID this manager never spawned.
/// Signalling it would make `stop_terminal` able to kill any process on the
/// machine that later reuses that PID. `stop_requested: false` is the only
/// acceptable answer.
#[tokio::test]
async fn an_orphaned_terminal_cannot_be_stopped_through_the_tool() {
    let tmp = TempDir::new().unwrap();
    let manager = manager_for(&tmp, "orphan-session");

    // Plant a record that looks live but was not spawned by this manager.
    let mut record = xaft_tools::TerminalRecord {
        terminal_id: "term-planted".into(),
        session_id: "orphan-session".into(),
        project_root: tmp.path().to_string_lossy().into_owned(),
        cwd: tmp.path().to_string_lossy().into_owned(),
        kind: "exec".into(),
        command: "sleep 30".into(),
        label: "planted".into(),
        state: xaft_tools::TerminalState::Running,
        created_at: 1.0,
        pid: Some(std::process::id() as i32),
        pgid: Some(std::process::id() as i32),
        ..Default::default()
    };
    manager.store().upsert(&record);

    // A fresh manager over the same store adopts it as orphaned, not as
    // signal-eligible.
    let adopter = Arc::new(TerminalManager::new(TerminalManagerConfig::new(
        "orphan-session",
        tmp.path(),
    )));
    let adopted = adopter.get("term-planted").expect("record is visible");
    assert_eq!(
        adopted.state,
        xaft_tools::TerminalState::Orphaned,
        "an inherited live record must be orphaned, never adopted as stoppable"
    );

    let tool = StopTerminalTool::new(Some(Arc::clone(&adopter)));
    let ctx = ToolContext::new("orphan-1");
    let out = body(
        &tool
            .call(serde_json::json!({ "terminal_id": "term-planted" }), &ctx)
            .await
            .unwrap(),
    );
    assert_eq!(
        out["stop_requested"], false,
        "stop_terminal signalled a record it never spawned: {out}"
    );
    // And this very test process is still alive, which is the whole point.
    record.pid = None;
}

/// Signalling the child rather than the group leaves `sleep 100 &` running.
///
/// `sh -c 'sleep 100 &'` exits immediately, leaving the grandchild reparented to
/// init. Only `killpg` reaches it.
#[tokio::test]
async fn stop_reaches_a_grandchild_through_the_process_group_via_the_tool() {
    let tmp = TempDir::new().unwrap();
    let manager = manager_for(&tmp, "pgid-session");

    // The trailing `wait` is load-bearing. Without it `sh` exits the moment
    // `echo` finishes, the record is already non-active by the time `stop`
    // runs, and `stop_requested` is false -- which would make this test fail
    // for a reason that has nothing to do with process groups. Holding the
    // shell open as the group leader is what keeps the terminal *owned*, and
    // so still stoppable, at the moment `stop` is called.
    let pid_file = tmp.path().join("grandchild.pid");
    let result = manager
        .start(
            Some(&format!(
                "sleep 100 & echo $! > {}; wait",
                pid_file.display()
            )),
            None,
            Some("."),
            0.0,
            None,
            true,
            "grandchild",
            "call-pgid",
            "oneshot",
            None,
        )
        .await;
    let id = result["terminal_id"]
        .as_str()
        .expect("terminal_id")
        .to_string();

    // The pid goes through a file rather than stdout: `start` returns before
    // the shell has written a byte, so a test that had to poll the transcript
    // to learn the pid would be exercising the capture path too.
    let mut pid = 0;
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&pid_file) {
            pid = text.trim().parse().unwrap_or(0);
            if pid > 0 {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        pid > 0,
        "expected a grandchild pid at {}",
        pid_file.display()
    );

    let tool = StopTerminalTool::new(Some(Arc::clone(&manager)));
    let ctx = ToolContext::new("pgid-1");
    let out = body(
        &tool
            .call(
                serde_json::json!({ "terminal_id": id, "force": true }),
                &ctx,
            )
            .await
            .unwrap(),
    );
    assert_eq!(out["stop_requested"], true, "{out}");

    // The property that actually matters: the GRANDCHILD is gone. Signalling
    // only the shell leaves `sleep 100` reparented to init and running, so
    // `stop_requested: true` above can hold while the leak still happens.
    let mut alive = true;
    for _ in 0..40 {
        if !pid_alive(pid) {
            alive = false;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        !alive,
        "grandchild {pid} survived; killpg did not reach it: {out}"
    );
}

/// A `cwd` outside the workspace is refused BEFORE any spawn.
#[tokio::test]
async fn a_background_terminal_refuses_a_cwd_outside_the_workspace() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistryBuilder::new(tmp.path())
        .without_git()
        .with_shell()
        .with_terminal_manager(manager_for(&tmp, "escape"))
        .build_coder()
        .unwrap();

    let ctx = ToolContext::new("escape-1");
    let out = body(
        &registry
            .get("run_command")
            .unwrap()
            .call(
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "sleep 100"],
                    "cwd": "../..",
                    "background": true,
                }),
                &ctx,
            )
            .await
            .unwrap(),
    );
    assert_eq!(out["ok"], false, "a cwd escape was accepted: {out}");
    assert_eq!(out["state"], "rejected", "{out}");
    assert!(
        out["error"].as_str().unwrap_or("").contains("escapes"),
        "unexpected error: {out}"
    );
}

/// Output is redacted BEFORE it is persisted and before it is returned.
#[tokio::test]
async fn output_is_redacted_before_it_is_persisted_and_returned() {
    let tmp = TempDir::new().unwrap();
    let manager = manager_for(&tmp, "redact-session");

    let result = manager
        .start(
            Some("echo TOKEN=super-secret-value-12345; exit 0"),
            None,
            Some("."),
            0.0,
            None,
            true,
            "leaky",
            "call-redact",
            "oneshot",
            None,
        )
        .await;
    let id = result["terminal_id"]
        .as_str()
        .expect("terminal_id")
        .to_string();

    // Observe it to completion so the output is captured.
    let waited = manager.wait(&id, 5.0).await;
    assert!(
        !waited.to_string().contains("super-secret-value-12345"),
        "the secret leaked through wait: {waited}"
    );
    assert!(
        waited["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("TOKEN=<redacted>"),
        "expected a redacted assignment, got: {waited}"
    );

    // The durable store must not contain it either.
    let raw = std::fs::read_to_string(manager.store().path()).unwrap_or_default();
    assert!(
        !raw.contains("super-secret-value-12345"),
        "the secret was persisted to the JSONL store"
    );

    // And the tool must not leak it on the way out.
    let tool = InspectTerminalTool::new(Some(Arc::clone(&manager)));
    let ctx = ToolContext::new("redact-1");
    let out = tool
        .call(serde_json::json!({ "terminal_id": id }), &ctx)
        .await
        .unwrap();
    assert!(
        !out.content.contains("super-secret-value-12345"),
        "inspect_terminal leaked the secret: {}",
        out.content
    );
}

/// A terminal belonging to another session is refused, not inspected.
#[tokio::test]
async fn a_terminal_from_another_session_is_not_inspectable() {
    let tmp = TempDir::new().unwrap();
    let other = manager_for(&tmp, "session-a");

    let started = other
        .start(
            Some("sleep 5"),
            None,
            Some("."),
            0.0,
            None,
            true,
            "theirs",
            "call-a",
            "oneshot",
            None,
        )
        .await;
    let id = started["terminal_id"]
        .as_str()
        .expect("terminal_id")
        .to_string();

    // Built AFTER `other` started its terminal, on purpose. Both managers
    // share a store root, but `TerminalManager::new` snapshots the store into
    // an in-memory map once and never re-reads it, so a manager built first
    // has never heard of a terminal the other one started afterwards. Build it
    // last so the record IS visible and the session check is what refuses it
    // -- building it first would make this test pass for the wrong reason,
    // asserting "unknown" without ever reaching the ownership check.
    let mine = manager_for(&tmp, "session-b");

    let tool = InspectTerminalTool::new(Some(Arc::clone(&mine)));
    let ctx = ToolContext::new("cross-1");
    let out = body(
        &tool
            .call(serde_json::json!({ "terminal_id": id }), &ctx)
            .await
            .unwrap(),
    );
    assert_eq!(
        out["ok"], false,
        "another session's terminal was visible: {out}"
    );
    assert_eq!(out["state"], "rejected", "{out}");
    assert_eq!(out["error"], "terminal belongs to another session", "{out}");

    other.stop(&id, true, "cleanup").await;
}

/// Without a manager the four tools refuse with agenthicc's exact envelope.
///
/// They are still REGISTERED, which is the point: an agent that cannot see a
/// tool learns nothing, while one that sees it refused learns the capability is
/// absent and why.
#[tokio::test]
async fn the_tools_refuse_honestly_when_no_manager_is_wired() {
    let ctx = ToolContext::new("no-manager-1");

    let inspections = vec![
        (
            InspectTerminalTool::new(None)
                .call(serde_json::json!({ "terminal_id": "x" }), &ctx)
                .await
                .unwrap(),
            "inspect_terminal",
        ),
        (
            WaitTerminalTool::new(None)
                .call(serde_json::json!({ "terminal_id": "x" }), &ctx)
                .await
                .unwrap(),
            "wait_terminal",
        ),
        (
            WaitTerminalReadyTool::new(None)
                .call(serde_json::json!({ "terminal_id": "x" }), &ctx)
                .await
                .unwrap(),
            "wait_terminal_ready",
        ),
        (
            StopTerminalTool::new(None)
                .call(serde_json::json!({ "terminal_id": "x" }), &ctx)
                .await
                .unwrap(),
            "stop_terminal",
        ),
    ];

    for (result, name) in inspections {
        assert!(result.is_error, "{name} claimed success without a manager");
        let out = body(&result);
        assert_eq!(out["ok"], false, "{name}: {out}");
        assert_eq!(out["state"], "rejected", "{name}: {out}");
        assert_eq!(
            out["error"], "terminal manager unavailable",
            "{name}: {out}"
        );
    }
}

/// `background: true` with no manager is refused, never silently ignored.
///
/// This is the branch the old code took unconditionally; now it is reachable
/// only when no manager was supplied, and it must still be honest.
#[tokio::test]
async fn background_is_refused_when_no_manager_is_wired() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistryBuilder::new(tmp.path())
        .without_git()
        .with_shell()
        .build_coder()
        .unwrap();
    let ctx = ToolContext::new("no-mgr-rc");
    let out = body(
        &registry
            .get("run_command")
            .unwrap()
            .call(
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "sleep 1"],
                    "cwd": ".",
                    "background": true,
                }),
                &ctx,
            )
            .await
            .unwrap(),
    );
    assert_eq!(out["ok"], false, "{out}");
    assert_eq!(out["background"], true, "{out}");
    assert_eq!(out["state"], "rejected", "{out}");
}

/// A non-loopback TCP readiness probe is refused without `allow_external`.
///
/// `wait_terminal_ready` is the tool that carries the probe, so this is the
/// tool-level half of the guard.
#[tokio::test]
async fn a_non_loopback_probe_is_refused_through_the_readiness_tool() {
    let tmp = TempDir::new().unwrap();
    let manager = manager_for(&tmp, "probe-session");

    let started = manager
        .start(
            Some("sleep 10"),
            None,
            Some("."),
            0.0,
            None,
            true,
            "probe-target",
            "call-probe",
            "service",
            None,
        )
        .await;
    let id = started["terminal_id"]
        .as_str()
        .expect("terminal_id")
        .to_string();

    let tool = WaitTerminalReadyTool::new(Some(Arc::clone(&manager)));
    let ctx = ToolContext::new("probe-1");
    let out = body(
        &tool
            .call(
                serde_json::json!({
                    "terminal_id": id,
                    // `tcp` is a KEY whose value is the target object, which is
                    // the shape agenthicc's `probe.get("tcp")` reads. A flat
                    // `{"type":"tcp",...}` names none of url/tcp/marker and is
                    // rejected as a malformed probe instead of being attempted.
                    "readiness": { "tcp": { "host": "example.com", "port": 80 } },
                }),
                &ctx,
            )
            .await
            .unwrap(),
    );
    assert!(
        out["ready"] == false || out["ok"] == false,
        "a non-loopback probe was accepted: {out}"
    );
    let evidence = out["readiness_evidence"].as_str().unwrap_or("");
    assert!(
        evidence.contains("loopback") || evidence.contains("allow_external"),
        "the refusal must say why: {out}"
    );

    manager.stop(&id, true, "cleanup").await;
}

/// `stop_terminal` signals a process, so it must sit behind the approval gate.
#[test]
fn the_terminal_tools_declare_the_right_approval_and_parallel_shape() {
    let manager: Option<Arc<TerminalManager>> = None;
    assert!(
        StopTerminalTool::new(manager.clone()).requires_confirmation(),
        "stop_terminal kills a process group and must require confirmation"
    );
    assert!(
        !InspectTerminalTool::new(manager.clone()).requires_confirmation(),
        "inspect_terminal is read-only"
    );
    assert!(!WaitTerminalTool::new(manager.clone()).requires_confirmation());
    assert!(!WaitTerminalReadyTool::new(manager).requires_confirmation());

    // Only the pure read is parallel-safe: the other three either block or
    // mutate process state, and running two at once could race a stop against
    // a wait.
    assert!(InspectTerminalTool::new(None).parallel_safe());
    assert!(!WaitTerminalTool::new(None).parallel_safe());
    assert!(!WaitTerminalReadyTool::new(None).parallel_safe());
    assert!(!StopTerminalTool::new(None).parallel_safe());
}

/// The names and schemas must match agenthicc's, since the agent sees them.
#[test]
fn the_tool_names_and_required_fields_match_agenthicc() {
    let cases: Vec<(
        Box<dyn Tool<Inputs = Value, Output = agtrs_runtime::tool::ToolResult>>,
        &str,
        &str,
    )> = vec![
        (
            Box::new(InspectTerminalTool::new(None)),
            "inspect_terminal",
            "terminal_id",
        ),
        (
            Box::new(WaitTerminalTool::new(None)),
            "wait_terminal",
            "terminal_id",
        ),
        (
            Box::new(WaitTerminalReadyTool::new(None)),
            "wait_terminal_ready",
            "terminal_id",
        ),
        (
            Box::new(StopTerminalTool::new(None)),
            "stop_terminal",
            "terminal_id",
        ),
    ];
    for (tool, name, required) in cases {
        assert_eq!(tool.name(), name);
        let schema = tool.schema();
        let req = schema["required"].as_array().expect("required array");
        assert!(
            req.iter().any(|v| v == required),
            "{name} must require {required}: {schema}"
        );
        assert_eq!(
            schema["additionalProperties"], false,
            "{name} must not accept unknown properties: {schema}"
        );
        assert!(
            !tool.description().is_empty(),
            "{name} has an empty description"
        );
    }

    // agenthicc's `wait_terminal` defaults timeout to 0.0, meaning "report now".
    assert_eq!(
        WaitTerminalTool::new(None).schema()["properties"]["timeout"]["default"],
        0.0
    );
    assert_eq!(
        StopTerminalTool::new(None).schema()["properties"]["force"]["default"],
        false
    );
}

/// A terminal with no readiness probe must not be reported as failed.
///
/// An early version of this shape would mark `ready: false` for a terminal that
/// was never probed, which reads as "this service is broken" to the agent.
#[tokio::test]
async fn readiness_on_a_terminal_with_no_probe_is_neither_ready_nor_failed() {
    let tmp = TempDir::new().unwrap();
    let manager = manager_for(&tmp, "no-probe");
    let id = start_sleeper(&manager, ".").await;

    let tool = WaitTerminalReadyTool::new(Some(Arc::clone(&manager)));
    let ctx = ToolContext::new("noprobe-1");
    let out = body(
        &tool
            .call(serde_json::json!({ "terminal_id": id }), &ctx)
            .await
            .unwrap(),
    );
    assert!(
        out["ready"].is_null(),
        "an unprobed terminal must carry ready: null, got {out}"
    );
}
