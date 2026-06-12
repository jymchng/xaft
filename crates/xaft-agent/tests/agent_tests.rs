//! Unit and integration tests for `XaftAgent`.
//!
//! Uses `agtrs_runtime::testing::{AgentTestClient, MockTransport}` to avoid
//! real LLM calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agtrs_runtime::agent::{Agent, AgentConfig, AgentContext, AgentResponse};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::streaming::StreamEvent;
use agtrs_runtime::testing::AgentTestClient;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_runtime::transport::{StopReason, TokenUsage};
use async_trait::async_trait;

use xaft_agent::config::XaftAgentConfig;
use xaft_agent::prompts::{CODER_SYSTEM_PROMPT, build_system_prompt, default_prompt_for};
use xaft_agent::{
    AgentBuilder, AgentRole, CollectSink, CommitPolicy, NopSink, StreamSink, XaftAgent,
    XaftLlmCallStarting,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn test_client() -> AgentTestClient {
    AgentTestClient::new()
}

fn make_agent(name: &str) -> XaftAgent {
    AgentBuilder::new(name)
        .role(AgentRole::Coder)
        .system_prompt("You are a test coder.")
        .max_turns(5)
        .build()
}

// A minimal tool for testing tool invocation
struct EchoTool {
    name: String,
    call_count: Arc<AtomicUsize>,
}

impl EchoTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[allow(dead_code)]
    fn count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

impl std::fmt::Debug for EchoTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoTool")
            .field("name", &self.name)
            .finish()
    }
}

#[async_trait]
impl Tool for EchoTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Echoes back the input value."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"]
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        let val = input["value"].as_str().unwrap_or("nothing");
        Ok(ToolResult::ok(format!("echoed: {val}"), &ctx.tool_use_id))
    }
}

// ── Prompts ───────────────────────────────────────────────────────────────────

#[test]
fn coder_default_prompt_is_non_empty() {
    let p = default_prompt_for(&AgentRole::Coder).unwrap();
    assert!(!p.is_empty());
    assert!(p.contains("engineer") || p.contains("software"));
}

#[test]
fn custom_role_has_no_default_prompt() {
    assert!(default_prompt_for(&AgentRole::Custom("foo".into())).is_none());
}

#[test]
fn build_system_prompt_includes_extra() {
    let p = build_system_prompt(&AgentRole::Coder, Some("Extra rule."));
    assert!(p.contains("Extra rule."));
    assert!(p.contains("Additional Instructions"));
}

#[test]
fn build_system_prompt_without_extra_matches_default() {
    let p = build_system_prompt(&AgentRole::Coder, None);
    assert_eq!(p, CODER_SYSTEM_PROMPT);
}

// ── AgentBuilder ──────────────────────────────────────────────────────────────

#[test]
fn builder_name_and_role() {
    let a = AgentBuilder::new("my-agent")
        .role(AgentRole::Reviewer)
        .build();
    assert_eq!(a.name(), "my-agent");
    assert!(
        a.system_prompt().contains("review")
            || a.system_prompt().contains("Reviewer")
            || !a.system_prompt().is_empty()
    );
}

#[test]
fn builder_override_prompt() {
    let a = AgentBuilder::new("x")
        .system_prompt("Totally custom prompt.")
        .build();
    assert_eq!(a.system_prompt(), "Totally custom prompt.");
}

#[test]
fn builder_max_turns_propagated() {
    let a = AgentBuilder::new("x").max_turns(42).build();
    assert_eq!(a.config().max_turns, 42);
}

#[test]
fn builder_parallel_tools() {
    use agtrs_runtime::agent::ParallelToolPolicy;
    let a = AgentBuilder::new("x").parallel_tools().build();
    // parallel_tools() now sets Annotated or All policy (not a raw bool).
    // Both are non-Sequential.
    assert_ne!(
        a.config().parallel_tool_policy,
        ParallelToolPolicy::Sequential
    );
}

#[test]
fn builder_commit_policy_default_is_never() {
    let a = AgentBuilder::new("x").build();
    assert_eq!(*a.commit_policy(), CommitPolicy::Never);
}

#[test]
fn builder_commit_on_success() {
    let a = AgentBuilder::new("x").commit_on_success().build();
    assert_eq!(*a.commit_policy(), CommitPolicy::OnSuccess);
}

// ── StreamSink ────────────────────────────────────────────────────────────────

#[test]
fn nop_sink_does_not_panic() {
    let s = NopSink;
    s.send(StreamEvent::TextDelta { delta: "hi".into() });
}

#[test]
fn collect_sink_collects_events() {
    let s = CollectSink::new();
    s.send(StreamEvent::TextDelta { delta: "a".into() });
    s.send(StreamEvent::TextDelta { delta: "b".into() });
    assert_eq!(s.len(), 2);
    let drained = s.drain();
    assert_eq!(drained.len(), 2);
    assert!(s.is_empty());
}

#[test]
fn channel_sink_forwards_to_receiver() {
    use futures::StreamExt;
    use futures::executor::block_on;
    use xaft_agent::stream::channel;

    let (sink, rx) = channel();
    sink.send(StreamEvent::TextDelta {
        delta: "stream".into(),
    });
    drop(sink);
    let events: Vec<_> = block_on(rx.collect::<Vec<_>>());
    assert_eq!(events.len(), 1);
}

// ── Lifecycle hooks ───────────────────────────────────────────────────────────

#[tokio::test]
async fn on_start_sets_agent_name_in_state() {
    let llm = Arc::new(agtrs_runtime::testing::MockLlmProvider::new(Arc::new(
        agtrs_runtime::testing::MockTransport::new(),
    )));
    let resolve_ctx = Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
        injectable_runtime::EmptySingletonStore,
    )));
    let agent = make_agent("hook-test");
    let mut ctx = AgentContext::new("hook-test", agent.config().clone(), llm, resolve_ctx);

    use agtrs_runtime::agent::Agent;
    agent.on_start(&mut ctx).await.unwrap();

    let name_val = ctx.context_state().get("xaft_agent_name").cloned();
    assert_eq!(
        name_val,
        Some(serde_json::Value::String("hook-test".into()))
    );
}

#[tokio::test]
async fn before_llm_call_increments_counter_and_emits_signal() {
    let bus = Arc::new(SignalBus::new());
    let mut rx = bus.subscribe::<XaftLlmCallStarting>().await;

    let agent = AgentBuilder::new("signal-test")
        .role(AgentRole::Coder)
        .system_prompt("test")
        .signals(Arc::clone(&bus))
        .build();

    let mut messages = vec![];
    let mut options = agtrs_runtime::llm::LlmOptions::default();

    use agtrs_runtime::agent::Agent;
    agent
        .before_llm_call(&mut messages, &mut options)
        .await
        .unwrap();
    agent
        .before_llm_call(&mut messages, &mut options)
        .await
        .unwrap();

    // Give the spawned task time to emit
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let sig1 = rx.try_recv().unwrap();
    assert_eq!(sig1.call_index, 0);
    assert_eq!(sig1.agent_name, "signal-test");
    let sig2 = rx.try_recv().unwrap();
    assert_eq!(sig2.call_index, 1);
}

#[tokio::test]
async fn on_tool_result_forwards_to_stream_sink() {
    let sink = CollectSink::new();
    let agent = AgentBuilder::new("stream-test")
        .role(AgentRole::Coder)
        .system_prompt("test")
        .stream_sink(sink.clone())
        .build();

    let llm = Arc::new(agtrs_runtime::testing::MockLlmProvider::new(Arc::new(
        agtrs_runtime::testing::MockTransport::new(),
    )));
    let resolve_ctx = Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
        injectable_runtime::EmptySingletonStore,
    )));
    let ctx = AgentContext::new("stream-test", agent.config().clone(), llm, resolve_ctx);
    let result = ToolResult::ok("tool output", "use-id-1");

    use agtrs_runtime::agent::Agent;
    agent.on_tool_result(&result, &ctx).await.unwrap();

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], StreamEvent::ToolResult { .. }));
}

#[tokio::test]
async fn on_finish_emits_done_event() {
    let sink = CollectSink::new();
    let agent = AgentBuilder::new("finish-test")
        .role(AgentRole::Coder)
        .system_prompt("test")
        .stream_sink(sink.clone())
        .build();

    let llm = Arc::new(agtrs_runtime::testing::MockLlmProvider::new(Arc::new(
        agtrs_runtime::testing::MockTransport::new(),
    )));
    let resolve_ctx = Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
        injectable_runtime::EmptySingletonStore,
    )));
    let ctx = AgentContext::new("finish-test", agent.config().clone(), llm, resolve_ctx);
    let response = AgentResponse {
        content: "finished!".into(),
        turns: 2,
        total_usage: TokenUsage::new(100, 200),
        tool_calls_made: vec![],
        stop_reason: StopReason::EndTurn,
        metadata: Default::default(),
        reasoning_traces: vec![],
    };

    use agtrs_runtime::agent::Agent;
    agent.on_finish(&response, &ctx).await.unwrap();

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Done {
            content,
            turns,
            agent_name,
            ..
        } => {
            assert_eq!(content, "finished!");
            assert_eq!(*turns, 2);
            assert_eq!(agent_name, "finish-test");
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

// ── Full executor runs ────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_runs_to_completion_with_mock_llm() {
    let client = test_client();
    client
        .transport()
        .queue_text("I have completed the task.")
        .await;

    let agent = make_agent("run-test");
    let response = client.run(&agent, "Hello, do the task.").await.unwrap();
    assert!(!response.content.is_empty());
    assert_eq!(response.stop_reason, StopReason::EndTurn);
}

#[tokio::test]
async fn agent_executes_tool_call_from_llm() {
    let client = test_client();
    let echo_tool = Arc::new(EchoTool::new("echo"));
    let count = Arc::clone(&echo_tool.call_count);

    // LLM first calls the tool, then returns a text response
    client
        .transport()
        .queue_tool_call("echo", serde_json::json!({"value": "hello"}))
        .await;
    client.transport().queue_text("Done.").await;

    let agent = AgentBuilder::new("tool-test")
        .role(AgentRole::Coder)
        .system_prompt("Use the echo tool.")
        .tool(echo_tool as Arc<ErasedTool>)
        .max_turns(5)
        .build();

    let response = client.run(&agent, "Echo 'hello'.").await.unwrap();
    assert!(!response.content.is_empty());
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "tool should have been called once"
    );
}

#[tokio::test]
async fn agent_with_stream_sink_emits_tool_result() {
    let client = test_client();
    let sink = CollectSink::new();

    client
        .transport()
        .queue_tool_call("echo", serde_json::json!({"value": "world"}))
        .await;
    client.transport().queue_text("Done with tool.").await;

    let echo_tool = Arc::new(EchoTool::new("echo"));
    let agent = AgentBuilder::new("sink-test")
        .role(AgentRole::Coder)
        .system_prompt("Use echo.")
        .tool(echo_tool as Arc<ErasedTool>)
        .stream_sink(sink.clone())
        .max_turns(5)
        .build();

    client.run(&agent, "Echo world.").await.unwrap();

    // on_tool_result + on_finish should have emitted events
    let events = sink.drain();
    assert!(
        events.len() >= 2,
        "expected at least tool_result + done, got {}",
        events.len()
    );

    let has_tool_result = events
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolResult { .. }));
    let has_done = events.iter().any(|e| matches!(e, StreamEvent::Done { .. }));
    assert!(has_tool_result, "expected ToolResult event");
    assert!(has_done, "expected Done event");
}

#[tokio::test]
async fn agent_respects_max_turns() {
    let client = test_client();

    // Keep returning tool calls to force the agent to use turns
    for _ in 0..20 {
        client
            .transport()
            .queue_tool_call("echo", serde_json::json!({"value": "looping"}))
            .await;
    }
    // Final response (won't be reached if max_turns kicks in)
    client.transport().queue_text("Done looping.").await;

    let echo_tool = Arc::new(EchoTool::new("echo"));
    let agent = AgentBuilder::new("turns-test")
        .role(AgentRole::Coder)
        .system_prompt("Keep echoing.")
        .tool(echo_tool as Arc<ErasedTool>)
        .max_turns(3)
        .build();

    let result = client.run(&agent, "Loop.").await;
    // Executor returns Err(MaxTurnsExceeded) when the limit is hit — that's
    // the correct outcome; verify the agent stopped and didn't loop forever.
    match result {
        Ok(response) => assert!(response.turns <= 3),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            assert!(
                msg.contains("turn") || msg.contains("max"),
                "unexpected error: {e}"
            );
        }
    }
}

#[tokio::test]
async fn agent_xaft_config_for_role_sets_commit_policy() {
    let coder_cfg = XaftAgentConfig::for_role(AgentRole::Coder);
    assert_eq!(coder_cfg.commit_policy, CommitPolicy::OnSuccess);

    let reviewer_cfg = XaftAgentConfig::for_role(AgentRole::Reviewer);
    assert_eq!(reviewer_cfg.commit_policy, CommitPolicy::Never);
}

// ── Multiple agents independent ───────────────────────────────────────────────

#[tokio::test]
async fn two_agents_run_independently() {
    let client1 = test_client();
    let client2 = test_client();

    client1.transport().queue_text("Agent 1 done.").await;
    client2.transport().queue_text("Agent 2 done.").await;

    let agent1 = make_agent("agent-1");
    let agent2 = make_agent("agent-2");

    let (r1, r2) = tokio::join!(
        client1.run(&agent1, "Task 1"),
        client2.run(&agent2, "Task 2"),
    );
    assert!(r1.unwrap().content.contains("Agent 1"));
    assert!(r2.unwrap().content.contains("Agent 2"));
}

// ── Signal bus integration ────────────────────────────────────────────────────

#[tokio::test]
async fn signal_bus_receives_llm_call_starting_signals() {
    let bus = Arc::new(SignalBus::new());
    let mut rx = bus.subscribe::<XaftLlmCallStarting>().await;

    let client = test_client();
    // Two LLM turns: first tool call, then text
    client
        .transport()
        .queue_tool_call("echo", serde_json::json!({"value": "x"}))
        .await;
    client.transport().queue_text("done").await;

    let echo_tool = Arc::new(EchoTool::new("echo"));
    let agent = AgentBuilder::new("bus-agent")
        .role(AgentRole::Coder)
        .system_prompt("test")
        .tool(echo_tool as Arc<ErasedTool>)
        .signals(Arc::clone(&bus))
        .max_turns(5)
        .build();

    client.run(&agent, "test signal").await.unwrap();

    // Give spawned tasks time
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Should have at least 2 signals (one per LLM call turn)
    let mut count = 0;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    assert!(
        count >= 2,
        "expected at least 2 XaftLlmCallStarting signals, got {count}"
    );
}
