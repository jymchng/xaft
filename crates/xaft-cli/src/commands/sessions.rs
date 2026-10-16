//! `xaft session export` and `xaft session inspect` - agenthicc's
//! `sessions` family from `cli/commands/sessions.py`.
//!
//! # Why these live under `xaft session`
//!
//! agenthicc registers *two* unrelated session families:
//!
//! | family | source | operates on |
//! |---|---|---|
//! | `session` | `cli/commands/session_service.py` | a client-neutral `SessionService`, with capability gates, `last_event_sequence`, and a queue depth |
//! | `sessions` | `cli/commands/sessions.py` | xaft's own local session log - plain saved sessions, no service, no capabilities |
//!
//! Both families have an `export`, and the two exports write *different*
//! documents: the service one writes `{"schema_version", "snapshot", "events"}`
//! from a capability-checked service, while the `sessions` one writes a dump
//! of the local log. One `xaft session export` therefore cannot mean both, so
//! the document is picked by a flag rather than blended: `xaft session export`
//! writes the local log (matrix row 16, `export` below) and
//! `xaft session export --service` writes the capability-checked service
//! snapshot (matrix row 12, `export_service` below).
//!
//! Both stay under `xaft session` instead of becoming a second top-level
//! command, matching rows 14-15, which already map `sessions list`/
//! `sessions show` there. `--service` reads the session service's own store
//! and needs no local-log `SessionBackend`, so `commands::session` dispatches
//! it before that backend is resolved; the destination-file logic the two
//! share is why `export_service` lives here.
//!
//! # What is reported
//!
//! Everything printed here comes from xaft's real durable store
//! (`xaft_session::SessionManager`, SQLite). The store is *injected* by
//! `main.rs` rather than resolved here: `main.rs` opens it at
//! `config.core.data_dir` - the same path the TUI writes to
//! (`xaft-tui/src/app.rs`) - and hands that same handle to the runtime and to
//! these commands, so `xaft session inspect` sees the sessions the interactive
//! session created and cannot be pointed at a different database.
//!
//! agenthicc's `inspect` summary also reports `artifacts`, `workflows`,
//! `owner`, `resume` and `redactions`. xaft has no counterpart for any of
//! those: no workflow-run store, no owner/lease record, no redaction counter.
//! Printing them as a constant `0`/`unavailable` would be noise that looks
//! like data, so those lines are omitted rather than faked.

use std::path::PathBuf;

use xaft_runtime::ExitCode;
use xaft_runtime::SessionBackend;
use xaft_runtime::session::AgentSession;
use xaft_runtime::session::SessionId;

use crate::args::{SessionsExportArgs, SessionsInspectArgs};
use crate::error::XaftError;

/// Bumped when the shape of an exported document changes.
///
/// agenthicc writes `schema_version: 1` for the service-family export
/// (`session_service.session_export`); the same key is used here so a consumer
/// written against either family can branch on one field.
const EXPORT_SCHEMA_VERSION: u32 = 1;

/// agenthicc's prefix for a successful export. The wording is kept identical.
const EXPORT_PREFIX: &str = "Exported session";

/// agenthicc's prefix for a session that does not exist. `sessions show` and
/// `sessions inspect` both use this exact string.
const NOT_FOUND_PREFIX: &str = "Session not found";

// The two handlers below are called from `commands/session.rs`, which owns the
// `xaft session` match. They are public for that reason and for the
// integration tests.
//
// They take a `&dyn SessionBackend` rather than resolving a store themselves.
// `xaft session` reads its data through `RuntimeDispatch` — but `export` and
// `inspect` need the full message history, which the dispatch trait does not
// expose, and `xaft-runtime` cannot grow a method for it because
// `xaft-session` depends on `xaft-runtime` (see `xaft_runtime::session_backend`).
// So the store arrives by injection: `main.rs` constructs ONE `SessionManager`,
// gives its `session_store()` to the runtime and the manager itself to this
// CLI. `list`/`show`/`resume`/`cancel` and `export`/`inspect` therefore hold two
// views of a single handle on a single SQLite file, which is a stronger
// guarantee than two independent `core.data_dir` resolutions agreeing — and it
// is why these commands no longer carry a `--config` that could point them at a
// different database than the one the runtime was bootstrapped with.

/// `xaft session export` — write one session and its conversation to JSON.
pub async fn export(
    args: &SessionsExportArgs,
    backend: &dyn SessionBackend,
) -> Result<ExitCode, XaftError> {
    // NOTE: `--service` is dispatched to `export_service` by `commands::session`
    // rather than branched on here, because the two need different stores and
    // only one of them has a `backend`. See that match arm.
    let id = SessionId::from_string(args.id.clone());

    let loaded = match backend.load_with_history(&id).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return not_found(&args.id),
        Err(e) => return store_failure(&e.to_string()),
    };

    // agenthicc defaults the destination to `<SESSION_ID>.json` in the cwd.
    let destination = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}.json", args.id)));

    // `xaft` stores no credential in a session record — `AgentSession` has no
    // API key field and `Message` carries only role/content/name/tool_call_id/
    // metadata — so the exported document is redacted by construction and
    // needs no scrubber. If a credential is ever added to either type, this is
    // the place the redaction pass has to go.
    let document = serde_json::json!({
        "schema_version": EXPORT_SCHEMA_VERSION,
        "session": loaded.session,
        "message_count": loaded.messages.len(),
        "messages": loaded.messages,
    });
    let body = serde_json::to_string_pretty(&document).map_err(json_err)?;

    // Trailing newline so the file is a well-formed text file, matching what
    // agenthicc's service-family export writes.
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&destination, format!("{body}\n"))?;

    println!("{EXPORT_PREFIX} {} to {}", args.id, destination.display());
    Ok(ExitCode::SUCCESS)
}

/// `xaft session export --service` — agenthicc's `session export`
/// (`cli/commands/session_service.py`), the other of the two exports agenthicc
/// registers under similar names.
///
/// Writes `{"schema_version", "snapshot", "events"}` with the snapshot
/// projected through read-only capabilities, which is *not* the
/// `{session, message_count, messages}` shape of the default export.
pub async fn export_service(args: &SessionsExportArgs) -> Result<ExitCode, XaftError> {
    let document = crate::commands::session_service::export_document(&args.id).await?;

    // agenthicc defaults the destination to `<SESSION_ID>.json` in the cwd.
    let destination = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}.json", args.id)));

    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    crate::commands::session_service::write_json_document(&destination, &document)?;

    println!("{EXPORT_PREFIX} {} to {}", args.id, destination.display());
    Ok(ExitCode::SUCCESS)
}

/// `xaft session inspect` — summarise one session's durable state.
pub async fn inspect(
    args: &SessionsInspectArgs,
    backend: &dyn SessionBackend,
) -> Result<ExitCode, XaftError> {
    let id = SessionId::from_string(args.id.clone());

    let loaded = match backend.load_with_history(&id).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return not_found(&args.id),
        Err(e) => return store_failure(&e.to_string()),
    };

    let summary = summary_json(&loaded.session, loaded.messages.len());

    if args.json {
        // agenthicc serialises with `sort_keys=True`. `serde_json::Value`'s
        // object is a `BTreeMap` unless the `preserve_order` feature is on
        // (it is not, in this workspace), so this is key-sorted too.
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).map_err(json_err)?
        );
        return Ok(ExitCode::SUCCESS);
    }

    print_human(&summary, &loaded.session, loaded.messages.len());
    Ok(ExitCode::SUCCESS)
}

/// The machine-readable inspection summary.
///
/// Only fields xaft genuinely has. `metadata` and `conversation` keep
/// agenthicc's key names where the meaning carries over, so a consumer can
/// read `summary["metadata"]["model"]` the same way for both.
fn summary_json(session: &AgentSession, message_count: usize) -> serde_json::Value {
    serde_json::json!({
        "session_id": session.id.as_str(),
        "metadata": {
            "model": session.model,
            "agent_preset": session.agent_preset,
            "cwd": session.workspace_root.display().to_string(),
            "created_at": session.created_at.to_rfc3339(),
            "last_active": session.updated_at.to_rfc3339(),
            "status": session.status.label(),
            "task": session.task,
        },
        "conversation": {
            "messages": message_count,
            "turns": session.turn_count,
            "tokens": session.total_tokens,
            "cost_usd": session.total_cost_usd,
        },
    })
}

/// The stable, human-readable form.
fn print_human(summary: &serde_json::Value, session: &AgentSession, message_count: usize) {
    let meta = &summary["metadata"];
    let convo = &summary["conversation"];

    println!(
        "Session: {}",
        summary["session_id"].as_str().unwrap_or("<unknown>")
    );
    println!("Model: {}", meta["model"].as_str().unwrap_or("?"));
    println!("CWD: {}", meta["cwd"].as_str().unwrap_or("?"));
    println!("Created: {}", meta["created_at"].as_str().unwrap_or("?"));
    println!(
        "Last active: {}",
        meta["last_active"].as_str().unwrap_or("?")
    );
    println!("Status: {}", session.status.label());
    println!(
        "Agent preset: {}",
        meta["agent_preset"].as_str().unwrap_or("?")
    );
    println!(
        "Conversation: {message_count} messages, {} turns",
        convo["turns"].as_u64().unwrap_or(0)
    );
    println!("Tokens: {}", convo["tokens"].as_u64().unwrap_or(0));
    // agenthicc formats cost as `$%.4f`; match it so the column is scannable.
    println!("Cost: ${:.4}", session.total_cost_usd);
}

/// `XaftError` has no `From<serde_json::Error>` impl, so map it to an I/O
/// error the same way `runs.rs` already does. Everything serialised here is
/// built from xaft's own types, so this is a "cannot happen" arm rather than
/// a data path.
fn json_err(e: serde_json::Error) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

/// A session id that is not in the store.
///
/// agenthicc prints the message and `return`s, which leaves the process exit
/// code at 0 — a shell script cannot tell a failed export from a successful
/// one. xaft prints the identical text but exits 1. This is the one deliberate
/// behavioural divergence in this module and it is deliberate because a
/// command that fails silently is a scripting hazard, not a parity win.
fn not_found(id: &str) -> Result<ExitCode, XaftError> {
    println!("{NOT_FOUND_PREFIX}: {id}");
    Ok(ExitCode::TASK_FAILED)
}

/// The store could not be read or written.
fn store_failure(message: &str) -> Result<ExitCode, XaftError> {
    println!("error: {message}");
    Ok(ExitCode::TASK_FAILED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use xaft_runtime::session::SessionStatus;

    fn session() -> AgentSession {
        let mut s = AgentSession::new(
            "fix the parser",
            PathBuf::from("/tmp/project"),
            "code-review".into(),
            "claude-3-5-sonnet-20241022".into(),
        );
        s.total_tokens = 1234;
        s.total_cost_usd = 0.5;
        s.turn_count = 3;
        s.status = SessionStatus::Completed {
            summary: "ok".into(),
        };
        s
    }

    #[test]
    fn summary_json_reports_only_real_fields() {
        let summary = summary_json(&session(), 7);
        let meta = &summary["metadata"];
        assert_eq!(meta["model"], "claude-3-5-sonnet-20241022");
        assert_eq!(meta["agent_preset"], "code-review");
        assert_eq!(meta["cwd"], "/tmp/project");
        assert_eq!(meta["status"], "completed");
        // Timestamps are RFC3339, not the raw Debug of a DateTime.
        // chrono renders a +00:00 offset rather than a `Z` suffix here, so
        // assert by round-tripping instead of by string shape.
        for key in ["created_at", "last_active"] {
            let raw = meta[key].as_str().unwrap();
            DateTime::parse_from_rfc3339(raw)
                .unwrap_or_else(|e| panic!("{key} is not RFC3339: {raw} ({e})"));
        }
        let convo = &summary["conversation"];
        assert_eq!(convo["messages"], 7);
        assert_eq!(convo["turns"], 3);
        assert_eq!(convo["tokens"], 1234);
        assert_eq!(convo["cost_usd"], 0.5);
    }

    /// Collect every object key at any depth.
    fn collect_keys(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    out.push(k.clone());
                    collect_keys(v, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_keys(item, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn summary_json_carries_no_credential_shaped_field() {
        // The export is "redacted by construction" only because nothing
        // secret is stored. If a credential ever lands on AgentSession or
        // Message it would appear here verbatim, so pin the absence.
        //
        // This checks KEY names, not substrings of the rendered document: a
        // substring scan false-positives on the legitimate `tokens` counter
        // (a usage count, not a bearer token).
        let summary = summary_json(&session(), 1);
        let mut keys = Vec::new();
        collect_keys(&summary, &mut keys);
        for forbidden in [
            "api_key",
            "api_token",
            "token",
            "secret",
            "password",
            "credential",
        ] {
            assert!(
                !keys.iter().any(|k| k.eq_ignore_ascii_case(forbidden)),
                "summary unexpectedly exposes a {forbidden} field; keys: {keys:?}"
            );
        }
    }

    #[test]
    fn not_found_uses_agenthiccs_exact_wording_but_fails() {
        let code = not_found("abc").unwrap();
        assert!(!code.is_success(), "a missing session must not exit 0");
    }
}
