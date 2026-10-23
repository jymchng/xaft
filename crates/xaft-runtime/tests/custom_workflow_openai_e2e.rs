//! A custom workflow, end to end over the real OpenAI wire format.
//!
//! Everything a user's `.xaft/workflows/<name>.toml` goes through, with no
//! scripted provider in between: discovery from the project directory, the
//! workflow's own transition tool offered to the model and its argument
//! carried into a later phase's prompt, a script phase, the checkpoint, and
//! resuming after a phase failed — with the runtime's real `OpenAiProvider`
//! speaking HTTP to a local stand-in for an OpenAI-compatible server.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use xaft_config::workflows::WorkflowRegistry;
use xaft_config::{AgentPreset, ProviderConfig, XaftConfig};
use xaft_runtime::agent_registry::WorkflowConfig;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::phased::state::CheckpointStore;
use xaft_runtime::runtime::XaftRuntime;

const WORKFLOW: &str = r#"
description = "Draft release notes, check them with a script, summarize"
mode = "phased"

[[phases]]
name = "draft"
agent_type = "planner"
max_turns = 5
next = "check"
prompt = """
Draft release notes for: {intent}
Call notes_ready(notes) with the finished notes."""

[[phases.transitions]]
tool = "notes_ready"
description = "The release notes are drafted."
params = [{ name = "notes", description = "The release notes, as a Markdown list." }]

[[phases]]
name = "check"
agent_type = "script"
command = "printf '{\"approved\": true, \"summary\": \"notes checked\"}'"
timeout_s = 30
next = "summarize"

[[phases]]
name = "summarize"
agent_type = "reviewer"
max_turns = 3
prompt = "Summarize these release notes in one line: {data.draft.notes}"
"#;

const NOTES: &str = "- Added the --verbose flag\n- Fixed the resume crash";
const SUMMARY: &str = "Release notes summary: two changes shipped.";

/// What the stand-in server saw: one entry per request.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    tools: Vec<String>,
    last_user: String,
    system: String,
}

#[derive(Default)]
struct MockState {
    seen: Mutex<Vec<Seen>>,
    /// Answer 400 to every request of this phase.
    fail_phase: Mutex<Option<String>>,
}

/// The scripted model: call `notes_ready` once when it is offered, answer a
/// tool result with text, and otherwise give the final summary.
fn reply(body: &Value) -> Value {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let offered: Vec<&str> = body["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let called = messages.iter().any(|m| {
        m["tool_calls"].as_array().is_some_and(|calls| {
            calls
                .iter()
                .any(|c| c["function"]["name"] == "notes_ready")
        })
    });
    let message = if messages.last().is_some_and(|m| m["role"] == "tool") {
        json!({"role": "assistant", "content": "Done — the step is complete."})
    } else if offered.contains(&"notes_ready") && !called {
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": "call_notes", "type": "function",
            "function": {"name": "notes_ready", "arguments": json!({"notes": NOTES}).to_string()}
        }]})
    } else {
        json!({"role": "assistant", "content": SUMMARY})
    };
    let finish = if message["tool_calls"].is_array() {
        "tool_calls"
    } else {
        "stop"
    };
    json!({
        "id": "chatcmpl-test", "object": "chat.completion", "model": body["model"],
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

async fn handle(mut stream: TcpStream, state: Arc<MockState>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head_end, content_length) = loop {
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (i + 4, len);
        }
    };
    while buf.len() < head_end + content_length {
        let n = stream.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
    let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
    let text_of = |role: &str, last: bool| {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let mut it: Box<dyn Iterator<Item = &Value>> = if last {
            Box::new(messages.iter().rev())
        } else {
            Box::new(messages.iter())
        };
        it.find(|m| m["role"] == role)
            .map(|m| m["content"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    };
    let seen = Seen {
        path,
        tools: body["tools"]
            .as_array()
            .map(|t| {
                t.iter()
                    .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        last_user: text_of("user", true),
        system: text_of("system", false),
    };
    let failing = state
        .fail_phase
        .lock()
        .unwrap()
        .clone()
        .is_some_and(|p| seen.system.contains(&format!("phase '{p}'")));
    state.seen.lock().unwrap().push(seen);
    let (status, payload) = if failing {
        (
            "400 Bad Request",
            json!({"error": {"message": "scripted failure", "type": "invalid_request_error"}}),
        )
    } else {
        assert_ne!(body["stream"], json!(true), "the test server answers JSON only");
        ("200 OK", reply(&body))
    };
    let payload = payload.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Start the stand-in server; returns its base URL.
async fn serve(state: Arc<MockState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(handle(stream, Arc::clone(&state)));
        }
    });
    format!("http://{addr}")
}

fn config(base_url: &str) -> XaftConfig {
    let mut config = XaftConfig::default();
    let provider: ProviderConfig = toml::from_str(&format!(
        "type = \"openai-compatible\"\nbase_url = \"{base_url}\"\napi_key = \"test-key\"\n"
    ))
    .unwrap();
    config.provider.insert("mock".into(), provider);
    let preset: AgentPreset =
        toml::from_str("provider = \"mock\"\nmodel = \"mock-model\"\nmax_turns = 10\n").unwrap();
    config.agent.insert("default".into(), preset);
    config
}

fn request(
    config: &XaftConfig,
    project: &Path,
    intent: &str,
    resume_run_id: Option<String>,
) -> RunRequest {
    let registry = WorkflowRegistry::discover(config, project);
    let entry = registry
        .resolve("release_notes")
        .expect("the project workflow is discovered");
    let mut workflow = xaft_runtime::to_runtime_config(entry, config);
    if let WorkflowConfig::Phased {
        resume_run_id: slot,
        ..
    } = &mut workflow
    {
        *slot = resume_run_id;
    } else {
        panic!("release_notes must dispatch to the phased engine");
    }
    RunRequest {
        task: intent.into(),
        config: config.clone(),
        working_dir: project.to_path_buf(),
        headless: true,
        dry_run: false,
        // The script phase asks for approval; a headless run must opt in.
        auto_approve: true,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow,
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
        mode_approval_filter: None,
    }
}

fn project_with_workflow() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let workflows = dir.path().join(".xaft/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("release_notes.toml"), WORKFLOW).unwrap();
    dir
}

#[tokio::test]
async fn a_project_workflow_runs_over_the_openai_wire_format() {
    let state = Arc::new(MockState::default());
    let base = serve(Arc::clone(&state)).await;
    let project = project_with_workflow();
    let config = config(&base);
    let runtime = XaftRuntime::for_testing(config.clone(), None);

    let result = runtime
        .run(request(&config, project.path(), "v1.4 of the CLI", None))
        .await
        .expect("the workflow completes");
    assert_eq!(result.summary, SUMMARY);

    let seen = state.seen.lock().unwrap().clone();
    assert!(
        seen.iter().all(|s| s.path == "/v1/chat/completions"),
        "{seen:?}"
    );
    // The draft phase offers the workflow's own transition tool (and
    // ask_user); the model's call to it moves the run on.
    assert!(seen[0].tools.iter().any(|t| t == "notes_ready"), "{:?}", seen[0].tools);
    assert!(seen[0].tools.iter().any(|t| t == "ask_user"));
    assert!(seen[0].last_user.contains("Draft release notes for: v1.4 of the CLI"));
    // The script phase makes no model call; the summarize phase's prompt
    // carries the transition's argument.
    let summarize = seen
        .iter()
        .find(|s| s.system.contains("phase 'summarize'"))
        .expect("the summarize phase asked the model");
    assert!(
        summarize.last_user.contains("Fixed the resume crash"),
        "{}",
        summarize.last_user
    );
    assert!(!summarize.tools.iter().any(|t| t == "notes_ready"));

    // The checkpoint records the finished run, and stays out of git.
    let store = CheckpointStore::for_working_dir(project.path());
    let runs = store.list().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status.label(), "complete");
    assert!(project.path().join(".xaft/workflow-runs/.gitignore").exists());
}

#[tokio::test]
async fn a_failed_phase_resumes_where_it_stopped() {
    let state = Arc::new(MockState::default());
    let base = serve(Arc::clone(&state)).await;
    let project = project_with_workflow();
    let config = config(&base);
    let runtime = XaftRuntime::for_testing(config.clone(), None);

    *state.fail_phase.lock().unwrap() = Some("summarize".into());
    let failed = runtime
        .run(request(&config, project.path(), "v1.5", None))
        .await;
    assert!(failed.is_err(), "the summarize phase fails: {failed:?}");
    let store = CheckpointStore::for_working_dir(project.path());
    let stopped = store
        .latest_resumable(Some("release_notes"))
        .unwrap()
        .expect("the failed run is resumable");
    assert_eq!(stopped.status.label(), "failed");

    *state.fail_phase.lock().unwrap() = None;
    let before = state.seen.lock().unwrap().len();
    let resumed = runtime
        .run(request(
            &config,
            project.path(),
            "v1.5",
            Some(stopped.run_id.clone()),
        ))
        .await
        .expect("the resumed run completes");
    assert_eq!(resumed.summary, SUMMARY);
    let after = state.seen.lock().unwrap()[before..].to_vec();
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|s| s.system.contains("phase 'summarize'")),
        "resume repeats no finished phase: {after:?}"
    );
    assert_eq!(
        store.load(&stopped.run_id).unwrap().status.label(),
        "complete"
    );
}
