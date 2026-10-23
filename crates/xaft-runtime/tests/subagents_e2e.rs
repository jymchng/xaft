//! `spawn_subagents` and its communication tools in every interactive turn
//! (agenthicc's `subagents/`, injected by `runners/agent_turn.py`).
//!
//! Every test drives a real `XaftRuntime::run`. The model is scripted per
//! agent: the parent turn answers from one script, and each worker — told
//! apart by a marker in its task `context`, which lands in its system prompt
//! — from its own, so concurrent workers and the parent never race for one
//! queue. Every request is recorded, so the assertions are about what each
//! agent was offered and shown.

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::llm::LlmProvider;
use async_trait::async_trait;
use serde_json::{Value, json};
use support::{ScriptedLlm, last_tool_result, text, tool, tool_results, tool_with};
use tempfile::TempDir;
use xaft_config::XaftConfig;
use xaft_runtime::agent_registry::WorkflowConfig;
use xaft_runtime::dispatch::{ModeToolFilter, RunRequest, RuntimeDispatch};
use xaft_runtime::runtime::XaftRuntime;
use xaft_tools::capabilities::capability_of;

fn provider(llm: &Arc<ScriptedLlm>) -> Option<Arc<dyn LlmProvider>> {
    Some(Arc::clone(llm) as Arc<dyn LlmProvider>)
}

/// A run whose first agent is the coder: the turn with the write tools.
fn request(dir: &Path) -> RunRequest {
    RunRequest {
        task: "use subagents".into(),
        config: XaftConfig::default(),
        working_dir: dir.to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::Dynamic {
            initial_agent: "coder".into(),
            max_handoffs: 2,
            agent_subset: None,
        },
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
        mode_approval_filter: None,
    }
}

/// The two-worker spawn most tests use: an explorer and an implementer.
fn two_worker_spawn() -> Value {
    json!({
        "tasks": [
            {"type": "explorer", "task": "Find the entry point", "context": "ROUTE-EXPLORER"},
            {"type": "implementer", "task": "Create notes.txt", "context": "ROUTE-IMPLEMENTER"},
        ],
        "max_concurrent": 1,
    })
}

fn scripted_two_workers() -> Arc<ScriptedLlm> {
    ScriptedLlm::new()
        .parent([
            tool("spawn_subagents", two_worker_spawn()),
            text("Subagents finished."),
        ])
        .route("ROUTE-EXPLORER", [text("Entry point is src/main.rs.")])
        .route(
            "ROUTE-IMPLEMENTER",
            [
                tool(
                    "write_file",
                    json!({"path": "notes.txt", "content": "hello"}),
                ),
                text("Created notes.txt."),
            ],
        )
}

fn journal(dir: &Path, session: &str) -> Vec<Value> {
    let path = dir.join(".xaft/subagents").join(format!("{session}.jsonl"));
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn spawn_subagents_runs_typed_workers_and_returns_the_labelled_aggregate() {
    let dir = TempDir::new().unwrap();
    let llm = scripted_two_workers();
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    runtime.run(request(dir.path())).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "hello",
        "the implementer's write really ran"
    );
    let parent = llm.requests_for("");
    assert!(parent[0].offers("spawn_subagents"), "{:?}", parent[0].tools);
    for communication in [
        "answer_subagent",
        "send_subagent_message",
        "collect_subagent_results",
    ] {
        assert!(parent[0].offers(communication), "{communication}");
    }
    let result = last_tool_result_of(&parent[1]);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["total"], 2);
    assert_eq!(result["succeeded"], 2);
    assert_eq!(result["failed"], 0);
    let aggregate = result["results"].as_str().unwrap();
    assert!(aggregate.starts_with("=== explorer #1 (✓ "), "{aggregate}");
    assert!(aggregate.contains(") ===\nEntry point is src/main.rs.\n\n=== implementer #1 (✓ "));
    assert!(
        aggregate.ends_with(") ===\nCreated notes.txt."),
        "{aggregate}"
    );
}

/// Parse the newest tool result a recorded request carried.
fn last_tool_result_of(request: &support::Request) -> Value {
    let messages: Vec<agtrs_runtime::transport::Message> =
        serde_json::from_str(&request.transcript).unwrap();
    last_tool_result(&messages)
}

#[tokio::test]
async fn each_worker_gets_its_roles_prompt_and_only_its_roles_tools() {
    let dir = TempDir::new().unwrap();
    let llm = scripted_two_workers();
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    runtime.run(request(dir.path())).await.unwrap();

    let explorer = &llm.requests_for("ROUTE-EXPLORER")[0];
    assert!(
        explorer
            .system
            .starts_with("You are a read-only codebase explorer.")
    );
    assert!(explorer.system.contains("## INHERITED RUNTIME POLICY"));
    assert!(explorer.system.contains("## FINAL RESPONSE CONTRACT"));
    assert!(
        explorer
            .system
            .ends_with("[ADDITIONAL CONTEXT]\nROUTE-EXPLORER")
    );
    assert!(explorer.offers("read_file") && explorer.offers("list_files"));
    for absent in [
        "write_file",
        "edit_file",
        "bash_exec",
        "spawn_subagents",
        "answer_subagent",
    ] {
        assert!(
            !explorer.offers(absent),
            "explorer must not have {absent}: {:?}",
            explorer.tools
        );
    }
    for child in [
        "send_parent_message",
        "ask_parent",
        "poll_agent_messages",
        "send_peer_message",
        "ask_peer",
        "answer_peer",
    ] {
        assert!(explorer.offers(child), "workers get {child}");
    }
    assert!(
        explorer.transcript.contains("Find the entry point"),
        "the task is the worker's first message"
    );
    assert!(
        !explorer.transcript.contains("use subagents"),
        "a worker never sees the parent's conversation"
    );
    let implementer = &llm.requests_for("ROUTE-IMPLEMENTER")[0];
    assert!(
        implementer
            .system
            .starts_with("You are a focused implementer.")
    );
    assert!(implementer.offers("write_file") && implementer.offers("edit_file"));
    assert!(!implementer.offers("bash_exec"), "{:?}", implementer.tools);
}

#[tokio::test]
async fn results_are_journaled_and_the_same_task_set_is_served_from_the_journal() {
    let dir = TempDir::new().unwrap();
    let llm = scripted_two_workers();
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    let first = runtime.run(request(dir.path())).await.unwrap();
    let session = first.session.id.to_string();

    let records = journal(dir.path(), &session);
    let kinds: Vec<&str> = records.iter().filter_map(|r| r["kind"].as_str()).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == "subagent_worker_result")
            .count(),
        2
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == "subagent_pool_result")
            .count(),
        1
    );
    assert!(kinds.contains(&"agent_message_event"));
    let worker = records
        .iter()
        .find(|r| r["kind"] == "subagent_worker_result" && r["agent_type"] == "implementer")
        .unwrap();
    assert_eq!(worker["ok"], true);
    assert_eq!(worker["tool_calls"], json!(["write_file"]));
    assert_eq!(worker["changed_paths"], json!(["notes.txt"]));
    assert_eq!(worker["schema_version"], 1);

    // The same session asks for the same work again: no worker runs.
    let again =
        ScriptedLlm::new().parent([tool("spawn_subagents", two_worker_spawn()), text("Reused.")]);
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&again));
    let mut req = request(dir.path());
    req.resume_session_id = Some(session.clone());
    // Resuming needs the session in this runtime's store.
    runtime.save_session(&first.session).await.unwrap();
    runtime.run(req).await.unwrap();
    assert_eq!(
        again.requests().len(),
        2,
        "only the parent called the model"
    );
    let cached = last_tool_result_of(&again.requests()[1]);
    assert_eq!(cached["pool_id"], "cached");
    assert_eq!(cached["succeeded"], 2);
    assert!(
        cached["results"]
            .as_str()
            .unwrap()
            .contains("Entry point is src/main.rs.")
    );
}

/// Records every approval request; approves unless the tool is in `deny`.
struct Gate {
    seen: Mutex<Vec<String>>,
    deny: Vec<&'static str>,
}

#[async_trait]
impl ApprovalGate for Gate {
    async fn request(&self, tool: &str, _id: &str, _input: &Value) -> bool {
        self.seen.lock().unwrap().push(tool.to_string());
        !self.deny.contains(&tool)
    }
}

fn safe() -> ModeToolFilter {
    Arc::new(|name: &str| capability_of(name).is_restricted())
}

#[tokio::test]
async fn safe_mode_asks_before_the_spawn_and_before_each_worker_write() {
    let dir = TempDir::new().unwrap();
    let llm = scripted_two_workers();
    let gate = Arc::new(Gate {
        seen: Mutex::new(Vec::new()),
        deny: vec![],
    });
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm))
        .with_approval_gate(Arc::clone(&gate) as Arc<dyn ApprovalGate>);
    let mut req = request(dir.path());
    req.mode_approval_filter = Some(safe());
    req.mode_system_patch = Some("## SAFE MODE".into());
    runtime.run(req).await.unwrap();

    assert_eq!(
        *gate.seen.lock().unwrap(),
        ["spawn_subagents", "write_file"]
    );
    assert!(dir.path().join("notes.txt").exists());
    let explorer = &llm.requests_for("ROUTE-EXPLORER")[0];
    assert!(explorer.system.contains("parent turn's safe mode"));
    assert!(
        explorer.system.contains("## SAFE MODE"),
        "the mode patch is inherited"
    );
}

#[tokio::test]
async fn a_denied_worker_write_fails_the_implementer_on_evidence() {
    let dir = TempDir::new().unwrap();
    let llm = scripted_two_workers();
    let gate = Arc::new(Gate {
        seen: Mutex::new(Vec::new()),
        deny: vec!["write_file"],
    });
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm))
        .with_approval_gate(Arc::clone(&gate) as Arc<dyn ApprovalGate>);
    let mut req = request(dir.path());
    req.mode_approval_filter = Some(safe());
    runtime.run(req).await.unwrap();

    assert!(
        !dir.path().join("notes.txt").exists(),
        "the denied write never ran"
    );
    let result = last_tool_result_of(&llm.requests_for("")[1]);
    assert_eq!(result["ok"], false);
    assert_eq!(result["failed"], 1);
    assert_eq!(result["error"], "1 subagent(s) failed");
    let aggregate = result["results"].as_str().unwrap();
    assert!(
        aggregate.contains(
            "=== implementer #1 (✗ implementer completed without a successful mutating tool call"
        ),
        "{aggregate}"
    );
    // A failed pool is never a resume entry.
    let session_records = std::fs::read_dir(dir.path().join(".xaft/subagents"))
        .unwrap()
        .flatten()
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(
        !session_records
            .iter()
            .any(|l| l.contains("\"subagent_pool_result\"")),
        "{session_records:?}"
    );
}

#[tokio::test]
async fn a_headless_safe_run_denies_the_spawn() {
    let dir = TempDir::new().unwrap();
    let llm = ScriptedLlm::new().parent([
        tool("spawn_subagents", two_worker_spawn()),
        text("Could not spawn."),
    ]);
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    let mut req = request(dir.path());
    req.mode_approval_filter = Some(safe());
    runtime.run(req).await.unwrap();
    assert!(
        llm.requests_for("ROUTE-EXPLORER").is_empty(),
        "no worker ran"
    );
    assert!(
        llm.requests_for("")[1]
            .transcript
            .contains("tool 'spawn_subagents' was rejected")
    );
}

#[tokio::test]
async fn plan_mode_offers_no_spawn_and_no_parent_communication_tools() {
    let dir = TempDir::new().unwrap();
    let llm = ScriptedLlm::new().parent([text("A plan.")]);
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    let mut req = request(dir.path());
    req.mode_tool_filter = Some(Arc::new(|name: &str| !capability_of(name).is_restricted()));
    runtime.run(req).await.unwrap();
    let first = &llm.requests()[0];
    for hidden in [
        "spawn_subagents",
        "answer_subagent",
        "collect_subagent_results",
    ] {
        assert!(!first.offers(hidden), "{hidden} in Plan: {:?}", first.tools);
    }
}

#[tokio::test]
async fn the_standard_planner_turn_gets_spawn_subagents_over_its_read_tools() {
    let dir = TempDir::new().unwrap();
    let llm = ScriptedLlm::new().parent([
        tool(
            "spawn_subagents",
            json!({"tasks": [{"type": "implementer", "task": "edit", "context": "ROUTE-IMPL"}]}),
        ),
        text("Planned."),
    ]);
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    let mut req = request(dir.path());
    req.workflow = WorkflowConfig::Standard;
    runtime.run(req).await.unwrap();
    // The planner's pool is read-only, so an implementer has nothing to
    // write with — agenthicc's message, and no worker model call at all.
    let result = last_tool_result_of(&llm.requests()[1]);
    assert!(
        result["results"]
            .as_str()
            .unwrap()
            .contains("implementer has no write-capable tools after session filtering"),
        "{result}"
    );
    assert!(llm.requests_for("ROUTE-IMPL").is_empty());
}

#[tokio::test]
async fn malformed_spawns_get_agenthiccs_errors() {
    let cases = [
        (
            json!({"tasks": "x"}),
            "tasks must be a list of task objects",
        ),
        (json!({"tasks": []}), "tasks list is empty"),
        (
            json!({"tasks": [1]}),
            "tasks[0] must be a dict with 'type' and 'task' keys",
        ),
        (
            json!({"tasks": [{"task": "x"}]}),
            "tasks[0] missing 'type' field",
        ),
        (
            json!({"tasks": [{"type": "explorer"}]}),
            "tasks[0] missing 'task' field",
        ),
        (
            json!({"tasks": [{"type": "wizard", "task": "x"}]}),
            "Unknown agent type 'wizard'. Known types: explorer, planner, implementer, executor, tester, reviewer, documenter, verifier, researcher, research",
        ),
        (
            json!({"tasks": [{"type": "explorer", "task": "x"}], "timeout_s": 0}),
            "timeout_s must be a finite number greater than zero",
        ),
        (
            json!({"tasks": [{"type": "explorer", "task": "x"}], "max_concurrent": "two"}),
            "max_concurrent must be an integer",
        ),
    ];
    for (input, message) in cases {
        let dir = TempDir::new().unwrap();
        let llm = ScriptedLlm::new().parent([tool("spawn_subagents", input.clone()), text("ok")]);
        let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
        runtime.run(request(dir.path())).await.unwrap();
        let result = last_tool_result_of(&llm.requests()[1]);
        assert_eq!(result, json!({"ok": false, "error": message}), "{input}");
    }
}

#[tokio::test]
async fn a_workers_question_returns_awaiting_clarification_and_the_answer_resumes_it() {
    let dir = TempDir::new().unwrap();
    let llm = ScriptedLlm::new()
        .parent([
            tool(
                "spawn_subagents",
                json!({"tasks": [{"type": "research", "task": "Pick a database", "context": "ROUTE-R"}]}),
            ),
            tool_with("answer_subagent", |messages| {
                let pending = last_tool_result(messages);
                json!({
                    "pool_id": pending["pool_id"],
                    "question_id": pending["pending_questions"][0]["question_id"],
                    "answer": "SQLite, it is embedded",
                })
            }),
            tool_with("collect_subagent_results", |messages| {
                let answered = last_tool_result(messages);
                json!({"pool_id": answered["pool_id"]})
            }),
            text("Picked a database."),
        ])
        .route(
            "ROUTE-R",
            [
                tool("ask_parent", json!({"question": "Postgres or SQLite?"})),
                text("Asked the parent."),
                text("Use SQLite."),
            ],
        );
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    let result = runtime.run(request(dir.path())).await.unwrap();
    assert!(
        result.summary.contains("Picked a database."),
        "{}",
        result.summary
    );

    let parent = llm.requests_for("");
    let pending = last_tool_result_of(&parent[1]);
    assert_eq!(pending["status"], "awaiting_clarification", "{pending}");
    assert_eq!(pending["ok"], false);
    assert_eq!(
        pending["pending_questions"][0]["question"],
        "Postgres or SQLite?"
    );
    assert_eq!(pending["pending_questions"][0]["worker_id"], "task-0");

    let answered = last_tool_result_of(&parent[2]);
    assert_eq!(answered["ok"], true);
    assert_eq!(answered["status"], "answered");

    let collected = last_tool_result_of(&parent[3]);
    assert_eq!(collected["ok"], true, "{collected}");
    assert_eq!(
        collected["results"].as_str().unwrap().lines().last(),
        Some("Use SQLite."),
        "the worker continued with the answer: {collected}"
    );
    let follow_up = &llm.requests_for("ROUTE-R")[2];
    assert!(
        follow_up
            .transcript
            .contains("A parent or peer answered your clarification request."),
        "{}",
        follow_up.transcript
    );
    assert!(follow_up.transcript.contains("SQLite, it is embedded"));
}

#[tokio::test]
async fn a_parent_message_reaches_the_worker_at_its_next_turn() {
    let dir = TempDir::new().unwrap();
    let llm = ScriptedLlm::new()
        .parent([
            tool(
                "spawn_subagents",
                json!({"tasks": [{"type": "explorer", "task": "Survey", "context": "ROUTE-E"}]}),
            ),
            tool_with("send_subagent_message", |messages| {
                let pending = last_tool_result(messages);
                json!({
                    "pool_id": pending["pool_id"],
                    "worker_id": "task-0",
                    "message": "Also check the docs/ folder.",
                })
            }),
            tool_with("answer_subagent", |messages| {
                let spawn = tool_results(messages)
                    .into_iter()
                    .find(|r| r["status"] == "awaiting_clarification")
                    .unwrap();
                json!({
                    "pool_id": spawn["pool_id"],
                    "question_id": spawn["pending_questions"][0]["question_id"],
                    "answer": "No, that is all.",
                })
            }),
            tool_with(
                "collect_subagent_results",
                |messages| json!({"pool_id": last_tool_result(messages)["pool_id"]}),
            ),
            text("Surveyed."),
        ])
        .route(
            "ROUTE-E",
            [
                tool("ask_parent", json!({"question": "Anything else?"})),
                text("Waiting."),
                text("Checked docs too."),
            ],
        );
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    runtime.run(request(dir.path())).await.unwrap();

    let sent = last_tool_result_of(&llm.requests_for("")[2]);
    assert_eq!(sent["status"], "sent", "{sent}");
    let follow_up = &llm.requests_for("ROUTE-E")[2];
    assert!(
        follow_up
            .transcript
            .contains("## AGENT MESSAGES (UNTRUSTED CONTEXT)"),
        "{}",
        follow_up.transcript
    );
    assert!(
        follow_up
            .transcript
            .contains("Also check the docs/ folder.")
    );
    let collected = last_tool_result_of(&llm.requests_for("")[4]);
    assert!(
        collected["results"]
            .as_str()
            .unwrap()
            .ends_with("Checked docs too."),
        "{collected}"
    );
}

#[tokio::test]
async fn a_peer_message_reaches_the_next_worker_and_workers_know_their_peers() {
    let dir = TempDir::new().unwrap();
    let llm = ScriptedLlm::new()
        .parent([
            tool(
                "spawn_subagents",
                json!({
                    "tasks": [
                        {"type": "explorer", "task": "Find the parser", "context": "ROUTE-A"},
                        {"type": "explorer", "task": "Document the parser", "context": "ROUTE-B"},
                    ],
                    "max_concurrent": 1,
                }),
            ),
            text("Done."),
        ])
        .route(
            "ROUTE-A",
            [
                tool(
                    "send_peer_message",
                    json!({"recipient": "task-1", "message": "The parser is in src/parse.rs"}),
                ),
                text("A done."),
            ],
        )
        .route("ROUTE-B", [text("B done.")]);
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    runtime.run(request(dir.path())).await.unwrap();

    let b = &llm.requests_for("ROUTE-B")[0];
    assert!(
        b.system
            .contains("Your worker id is task-1. Your peers: task-0 (explorer #1)."),
        "{}",
        b.system
    );
    assert!(
        b.transcript.contains("The parser is in src/parse.rs"),
        "{}",
        b.transcript
    );
    let result = last_tool_result_of(&llm.requests_for("")[1]);
    assert!(
        result["results"]
            .as_str()
            .unwrap()
            .contains("=== explorer #2 (✓ "),
        "labels count per role: {result}"
    );
}

#[tokio::test]
async fn subagent_cost_reaches_the_session_total() {
    let dir = TempDir::new().unwrap();
    let llm = scripted_two_workers();
    let runtime = XaftRuntime::for_testing(XaftConfig::default(), provider(&llm));
    let result = runtime.run(request(dir.path())).await.unwrap();
    // parent ×2, explorer ×1, implementer ×2 — every model call is counted.
    assert_eq!(llm.requests().len(), 5);
    assert!(
        result.session.total_tokens >= 5 * 30,
        "worker calls are on the run's signal bus: {} tokens",
        result.session.total_tokens
    );
}
