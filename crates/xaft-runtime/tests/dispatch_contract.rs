//! Behavioural coverage for `xaft_runtime::dispatch`.
//!
//! `dispatch.rs` had no test module at all: `RunRequest`'s hand-written
//! `Debug` impl, `StubRuntime`'s three trait methods and the two *defaulted*
//! trait methods (`load_session` / `save_session`) were entirely uncovered.
//! The defaults matter beyond coverage — they are the compatibility seam that
//! lets a test double or `StubRuntime` keep compiling without implementing them
//! — so the contract "an implementor that skips these methods gets a clear
//! `NotImplemented` error, not a silent success" is asserted here.
//!
//! Four assertions in this file were written against an assumed rendering and
//! corrected to the measured one; each divergence is called out at its site.

use std::path::PathBuf;
use std::sync::Arc;

use xaft_config::XaftConfig;
use xaft_runtime::dispatch::RuntimeDispatch;
use xaft_runtime::error::RuntimeError;
use xaft_runtime::{
    AgentSession, ExitCode, RunRequest, RunResult, SessionId, StubRuntime, WorkflowConfig,
};

/// `RuntimeError`'s `Display` prefixes every variant with a label, so the
/// rendered text of `NotImplemented(msg)` is `"not implemented: {msg}"`.
/// Measured, not assumed — see `DIVERGENCE` notes at the two call sites.
fn not_implemented_text(msg: &str) -> String {
    format!("not implemented: {msg}")
}

fn make_request(task: &str, working_dir: PathBuf) -> RunRequest {
    RunRequest {
        task: task.to_string(),
        config: XaftConfig::default(),
        working_dir,
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    }
}

#[tokio::test]
async fn the_debug_impl_reports_every_field_it_documents() {
    let mut req = make_request("write the thing", PathBuf::from("/tmp/demo"));
    req.headless = false;
    req.dry_run = true;
    req.auto_approve = true;
    req.dangerously_skip_permissions = true;
    req.resume_session_id = Some("sess-42".to_string());
    req.mode_system_patch = Some("patch".to_string());

    let rendered = format!("{req:?}");

    assert!(rendered.starts_with("RunRequest {"), "{rendered}");
    // Every field the impl chooses to surface must actually be surfaced, so a
    // field dropped from the Debug impl shows up as a failing assertion here
    // rather than silently shrinking operator-facing diagnostics.
    for field in [
        "task",
        "working_dir",
        "headless",
        "dry_run",
        "auto_approve",
        "dangerously_skip_permissions",
        "resume_session_id",
        "mode_system_patch",
        "has_mode_tool_filter",
    ] {
        assert!(
            rendered.contains(field),
            "Debug missing `{field}`: {rendered}"
        );
    }

    assert!(rendered.contains("write the thing"), "{rendered}");
    assert!(rendered.contains("/tmp/demo"), "{rendered}");
    assert!(rendered.contains("headless: false"), "{rendered}");
    assert!(rendered.contains("dry_run: true"), "{rendered}");
    assert!(rendered.contains("auto_approve: true"), "{rendered}");
    assert!(
        rendered.contains("dangerously_skip_permissions: true"),
        "{rendered}"
    );
    assert!(rendered.contains("Some(\"sess-42\")"), "{rendered}");
    assert!(rendered.contains("Some(\"patch\")"), "{rendered}");
    assert!(
        rendered.contains("has_mode_tool_filter: false"),
        "{rendered}"
    );
}

#[tokio::test]
async fn debug_reports_the_filter_as_a_presence_flag_not_as_a_closure() {
    // The filter is a boxed `Fn` and cannot be formatted, so the impl records
    // only whether one is attached.
    //
    // DIVERGENCE: this test was first written as `!rendered.contains("mode_tool_filter")`,
    // which fails against the real impl because the key it does emit,
    // `has_mode_tool_filter`, is a superstring of that name. The measured
    // contract is narrower and is what is asserted here: the filter appears
    // ONLY as the derived boolean, and never under its own key.
    let mut req = make_request("t", PathBuf::from("/tmp/demo"));
    let filter: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|name: &str| name == "ls");
    req.mode_tool_filter = Some(filter);

    let rendered = format!("{req:?}");

    assert!(
        rendered.contains("has_mode_tool_filter: true"),
        "{rendered}"
    );
    // DIVERGENCE (second correction): `"mode_tool_filter:"` is ALSO a substring
    // of `"has_mode_tool_filter:"`, so a plain `contains` check can never
    // express "not under its own key". The measurable form of the contract is
    // that every occurrence of the name is part of the derived boolean, i.e.
    // the two counts agree and equal one.
    assert_eq!(
        rendered.matches("mode_tool_filter").count(),
        rendered.matches("has_mode_tool_filter").count(),
        "the filter name must appear only inside the derived flag: {rendered}"
    );
    assert_eq!(
        rendered.matches("mode_tool_filter").count(),
        1,
        "{rendered}"
    );
    // `resume_session_id` and `mode_system_patch` are both None on this fixture,
    // so any `Some(` in the output could only have come from the filter
    // leaking through — assert there is none.
    assert!(
        !rendered.contains("Some("),
        "the boxed closure must not be rendered: {rendered}"
    );
}

#[tokio::test]
async fn the_stub_runtime_reports_the_task_it_would_have_run() {
    let runtime = StubRuntime;
    let req = make_request("summarise the repo", PathBuf::from("/tmp/demo"));

    let RunResult {
        exit_code,
        session,
        summary,
    } = runtime
        .run(req)
        .await
        .expect("StubRuntime::run must succeed");

    assert_eq!(exit_code, ExitCode::SUCCESS);
    assert_eq!(summary, "stub: would run task 'summarise the repo'");

    // The stub returns a real session record seeded from the request, so the
    // caller can still show and resume something.
    assert_eq!(session.task, "summarise the repo");
    assert_eq!(session.workspace_root, PathBuf::from("/tmp/demo"));
    assert_eq!(session.agent_preset, "default");
}

#[tokio::test]
async fn the_stub_runtime_seeds_the_session_from_the_default_preset() {
    // DIVERGENCE: this test was first written on the assumption that
    // `XaftConfig::default()` carries no `default` agent entry, so the stub's
    // `config.agent.get("default")` lookup would return `None` and the session
    // model would fall back to `""`. MEASURED: the default config DOES ship a
    // `default` preset, so the lookup succeeds and the model is the preset's.
    // Assert what the stub actually propagates, and assert the two lookups
    // agree — that equality is the property worth pinning, not the specific
    // model string, which is free to change as defaults evolve.
    let runtime = StubRuntime;
    let req = make_request("no preset configured", PathBuf::from("/tmp/demo"));

    let preset = req
        .config
        .agent
        .get("default")
        .cloned()
        .expect("measured: XaftConfig::default() ships a `default` preset");

    let result = runtime.run(req).await.expect("stub must still succeed");

    assert_eq!(result.session.model, preset.model);
    assert_eq!(result.exit_code, ExitCode::SUCCESS);
    // The provider is read on the same lookup, so it too comes from the preset
    // rather than from the `(unknown)` fallback the eprintln! block uses.
    assert!(!preset.provider.is_empty());
}

#[tokio::test]
async fn the_stub_runtime_lists_no_sessions() {
    let runtime = StubRuntime;
    let sessions = runtime
        .list_sessions(&PathBuf::from("/tmp/demo"))
        .await
        .expect("StubRuntime::list_sessions must succeed");
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn the_stub_runtime_refuses_resume_and_names_the_session() {
    let runtime = StubRuntime;
    let err = runtime
        .resume_session("sess-7", XaftConfig::default())
        .await
        .expect_err("StubRuntime has no resume implementation");

    match &err {
        RuntimeError::NotImplemented(msg) => assert_eq!(
            msg,
            "session resume not yet implemented (session_id: sess-7)"
        ),
        other => panic!("expected NotImplemented, got {other:?}"),
    }
    assert_eq!(err.exit_code(), ExitCode::TASK_FAILED);
}

#[tokio::test]
async fn an_implementor_that_skips_the_optional_methods_gets_a_named_error() {
    // The two session methods have trait defaults precisely so existing
    // implementors keep compiling. Assert the default is *loud*: a runtime that
    // does not override them must not look like it succeeded.
    struct Minimal;

    #[async_trait::async_trait]
    impl RuntimeDispatch for Minimal {
        async fn run(&self, _request: RunRequest) -> Result<RunResult, RuntimeError> {
            unreachable!("not exercised")
        }
        async fn list_sessions(
            &self,
            _working_dir: &std::path::Path,
        ) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(Vec::new())
        }
        async fn resume_session(
            &self,
            _session_id: &str,
            _config: XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            unreachable!("not exercised")
        }
    }

    let id = SessionId::from_string("sess-1");

    let load = Minimal
        .load_session(&id)
        .await
        .expect_err("the load_session default must not succeed");
    assert_eq!(
        load.to_string(),
        not_implemented_text("load_session is not implemented by this runtime")
    );

    let session = AgentSession::new(
        "t".to_string(),
        PathBuf::from("/tmp/demo"),
        "default".to_string(),
        String::new(),
    );
    let save = Minimal
        .save_session(&session)
        .await
        .expect_err("the save_session default must not succeed");
    assert_eq!(
        save.to_string(),
        not_implemented_text("save_session is not implemented by this runtime")
    );
}

#[tokio::test]
async fn the_stub_inherits_the_optional_method_defaults() {
    // `StubRuntime` overrides only the three required methods, so it exercises
    // the defaults in production code paths rather than just in a test double.
    let runtime = StubRuntime;
    let id = SessionId::from_string("sess-1");

    assert_eq!(
        runtime
            .load_session(&id)
            .await
            .expect_err("StubRuntime does not override load_session")
            .to_string(),
        not_implemented_text("load_session is not implemented by this runtime")
    );
    assert_eq!(
        runtime
            .save_session(&AgentSession::new(
                "t".to_string(),
                PathBuf::from("/tmp/demo"),
                "default".to_string(),
                String::new(),
            ))
            .await
            .expect_err("StubRuntime does not override save_session")
            .to_string(),
        not_implemented_text("save_session is not implemented by this runtime")
    );
}
