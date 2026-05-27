# Mocking LLM Providers

This document describes how to mock LLM providers in xaft for deterministic, fast, and isolated testing. LLM calls are the most expensive and least deterministic part of the agent runtime — a single integration test against a live provider can take 10-30 seconds and produce different results on every run. Mocking providers eliminates this variability, allowing tests to exercise the full agent pipeline at unit test speed with fully predictable behavior.

---

## The for_testing() Constructor

Every built-in LLM provider in xaft includes a `for_testing()` constructor that creates a pre-configured instance suitable for test environments. This constructor bypasses API key validation, network connectivity checks, and health checks, returning a provider that is ready to use without any external dependencies. The `for_testing()` constructor is gated behind the `test-util` feature flag to prevent accidental use in production code.

```rust
// Only available when the "test-util" feature is enabled
#[cfg(feature = "test-util")]
impl AnthropicProvider {
    pub fn for_testing() -> Self {
        Self {
            api_key: "test-key".to_string(),
            base_url: "http://localhost:0".to_string(), // unreachable port
            client: reqwest::Client::new(),
            models: vec![ModelId::new("claude-sonnet-4-20250514")],
        }
    }
}
```

The `for_testing()` constructor creates a provider with a dummy API key and an unreachable base URL. The provider cannot make real API calls — any attempt to call `complete()` or `stream()` will fail with a connection error. This is intentional: the `for_testing()` constructor is meant to be paired with provider override injection (described below), which replaces the provider's behavior with scripted responses. If you accidentally use a `for_testing()` provider without override injection, the test will fail fast with a clear connection error rather than making an unexpected API call.

The `for_testing()` constructor is also the recommended way to create providers in benchmarks. Benchmarks need to isolate the performance of the agent logic from the latency of the LLM API, so they should never make real API calls. Using `for_testing()` with scripted responses ensures that benchmarks measure only the agent's processing overhead.

---

## The MockProvider

The `MockProvider` is a fully controllable LLM provider implementation designed for testing. It implements the `LlmProvider` trait and returns pre-scripted responses for each call, enabling deterministic testing of any code path that depends on LLM behavior. The `MockProvider` is part of the `xaft-agent` crate and is gated behind the `test-util` feature flag.

### Basic Usage

```rust
use xaft_agent::test_util::MockProvider;
use xaft_agent::{LlmProvider, ChatRequest, ChatResponse, StreamChunk, LlmError};

let mut provider = MockProvider::new();

// Script a response for the next complete() call
provider.expect_complete(ChatResponse::new(
    "I will help you with that task.",
    None,  // no tool calls
    Some(TokenCount { input: 50, output: 10 }),
));

// The next call returns the scripted response
let request = ChatRequest::new("claude-sonnet-4-20250514")
    .with_user_message("Hello, can you help me?");

let response = provider.complete(request).await.unwrap();
assert_eq!(response.content(), "I will help you with that task.");
```

The `MockProvider` maintains an internal queue of expected calls. Each call to `expect_complete()` or `expect_stream()` adds a scripted response to the queue. When the agent calls `complete()` or `stream()`, the `MockProvider` pops the next response from the queue and returns it. If the queue is empty when a call is made, the `MockProvider` panics with a message describing the unexpected call. This fail-fast behavior ensures that tests catch missing expectations immediately rather than producing confusing downstream errors.

### Scripting Tool-Use Responses

The `MockProvider` can script responses that include tool calls, which is essential for testing the agent's tool execution pipeline:

```rust
let mut provider = MockProvider::new();

// First call: the LLM requests a tool call
provider.expect_complete(ChatResponse::new(
    "",
    Some(vec![
        ToolCall {
            id: "call_001".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        },
    ]),
    Some(TokenCount { input: 100, output: 25 }),
));

// Second call: after receiving the tool result, the LLM responds
provider.expect_complete(ChatResponse::new(
    "I've read the file. The main function is empty — shall I add some code?",
    None,
    Some(TokenCount { input: 200, output: 20 }),
));
```

By scripting a sequence of tool-use responses, you can test the full agent loop: the LLM requests a tool, the agent executes the tool and feeds the result back, and the LLM generates a follow-up response. This tests the entire feedback loop without making a real API call, and the test is fully deterministic — it will produce the same result on every run regardless of the LLM's non-determinism.

### Scripting Stream Responses

For testing the streaming pipeline, the `MockProvider` can script streaming responses that emit chunks one at a time:

```rust
let mut provider = MockProvider::new();

provider.expect_stream(vec![
    StreamChunk::TextDelta("Hello".to_string()),
    StreamChunk::TextDelta(", ".to_string()),
    StreamChunk::TextDelta("world".to_string()),
    StreamChunk::Done,
]);

let request = ChatRequest::new("claude-sonnet-4-20250514")
    .with_user_message("Say hello");

let mut stream = provider.stream(request).await.unwrap();

let mut tokens = String::new();
while let Some(chunk) = stream.next().await {
    if let StreamChunk::TextDelta(text) = chunk.unwrap() {
        tokens.push_str(&text);
    }
}

assert_eq!(tokens, "Hello, world");
```

The `expect_stream()` method accepts a `Vec<StreamChunk>` that is converted into a `futures::stream::Iter` — a synchronous iterator wrapped in a stream adapter. The chunks are emitted immediately (no delays), which makes streaming tests run at unit test speed. If you need to test the behavior of slow streams (for example, to verify that the TUI updates incrementally), you can insert `StreamChunk::Delay(duration)` chunks that pause the stream for the specified duration.

### Scripting Errors

The `MockProvider` can also script error responses, which is essential for testing error handling and retry logic:

```rust
let mut provider = MockProvider::new();

// First call: rate limited
provider.expect_complete_err(LlmError::RateLimited {
    retry_after: Some(Duration::from_secs(1)),
});

// Second call: success (the agent should retry)
provider.expect_complete(ChatResponse::new(
    "I'm back after the rate limit.",
    None,
    Some(TokenCount { input: 50, output: 10 }),
));
```

Error scripting is particularly important for testing the `FallbackProvider`. When the primary provider returns a rate limit error, the fallback provider should switch to the secondary provider. The test creates two `MockProvider` instances — one for the primary and one for the secondary — scripts the primary to fail and the secondary to succeed, and verifies that the fallback mechanism works correctly.

---

## Provider Override Injection

Provider override injection is the mechanism by which tests replace the production provider chain with mock providers. The runtime builds the provider chain during `run_task()` by calling `ProviderFactory::build()`, which reads the configuration and constructs the chain. In tests, you bypass this construction by injecting a pre-built provider directly into the agent.

### Direct Agent Injection

The simplest approach is to construct the agent with the mock provider already attached:

```rust
use xaft_agent::test_util::MockProvider;
use xaft_agent::*;

#[tokio::test]
async fn test_agent_handles_tool_error_gracefully() {
    let mut provider = MockProvider::new();

    // First call: request a tool that will fail
    provider.expect_complete(ChatResponse::new(
        "",
        Some(vec![ToolCall {
            id: "call_001".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/nonexistent/file.txt"}),
        }]),
        Some(TokenCount { input: 100, output: 25 }),
    ));

    // Second call: the agent should handle the error and respond
    provider.expect_complete(ChatResponse::new(
        "The file doesn't exist. Let me search for it instead.",
        Some(vec![ToolCall {
            id: "call_002".to_string(),
            name: "search_files".to_string(),
            arguments: serde_json::json!({"pattern": "main.rs"}),
        }]),
        Some(TokenCount { input: 200, output: 20 }),
    ));

    let agent = AgentBuilder::new()
        .name("test-agent")
        .role(Role::builder()
            .system_prompt("You are a test agent that handles errors gracefully.")
            .max_iterations(5)
            .build())
        .provider(Arc::new(provider))  // inject the mock provider
        .commit_policy(CommitPolicy::Never)
        .stream_sink(Arc::new(CollectSink::new()))
        .build()
        .unwrap();

    let workspace = Arc::new(InMemoryWorkspaceStore::new());
    let result = agent.turn(
        TurnInput::user("Read the file at /nonexistent/file.txt"),
        CancellationToken::new(),
    ).await;

    assert!(result.is_ok());
}
```

The `.provider()` method on `AgentBuilder` accepts an `Arc<dyn LlmProvider>` and bypasses the `ProviderFactory` entirely. This is the recommended approach for unit and integration tests where you need fine-grained control over the LLM's responses. The mock provider is injected directly into the agent, and no provider chain (CostedProvider, FallbackProvider) is constructed.

### Runtime-Level Injection

For tests that need to exercise the provider chain (for example, testing the `CostedProvider` or the `FallbackProvider`), you can inject the mock provider at the runtime level:

```rust
use xaft_runtime::test_harness;

#[tokio::test]
async fn test_costed_provider_tracks_token_usage() {
    let mut provider = MockProvider::new();
    provider.expect_complete(ChatResponse::new(
        "Done.",
        None,
        Some(TokenCount { input: 100, output: 50 }),
    ));

    let cost_accumulator = Arc::new(RunCostAccumulator::new());
    let costed = CostedProvider::new(
        Arc::new(provider),
        ModelPricing::new("claude-sonnet-4-20250514", 0.003, 0.015),
        cost_accumulator.clone(),
    );

    let result = test_harness::run_with_provider(
        Arc::new(costed),
        "Say something short",
    ).await;

    assert!(result.is_ok());

    let snapshot = cost_accumulator.snapshot().await;
    assert_eq!(snapshot.total_input_tokens, 100);
    assert_eq!(snapshot.total_output_tokens, 50);
    assert!(snapshot.total_cost_usd > 0.0);
}
```

Runtime-level injection wraps the mock provider in the production provider chain, allowing tests to verify that the wrapping layers (cost tracking, fallback, retry) work correctly with a controlled inner provider. This is more realistic than direct agent injection because it exercises the same code path that runs in production, but it still avoids making real API calls.

---

## InMemorySessionStore

The `InMemorySessionStore` is the test double for the `SessionStore` trait. It implements the full session store interface backed by an in-memory `HashMap`, providing fast, deterministic session operations for tests. Like `InMemoryWorkspaceStore`, it can be substituted seamlessly in any code that accepts `Arc<dyn SessionStore>`.

```rust
use xaft_session::test_util::InMemorySessionStore;

fn create_test_session_store() -> Arc<dyn SessionStore> {
    let store = InMemorySessionStore::new();

    // Pre-populate with a test session
    let session_id = store.create_session("Test prompt").unwrap();
    store.append_message(
        &session_id,
        &ChatMessage::assistant("I've completed the task."),
    ).unwrap();

    Arc::new(store)
}
```

The `InMemorySessionStore` supports all the same operations as `FsSessionStore`: creating sessions, appending messages, loading history, listing sessions, and updating status. It also supports the `list_prefix()` method for range queries, which is used by the plan state persistence system. Internally, the store uses `HashMap<String, serde_json::Value>` for session data and `Vec<ChatMessage>` for conversation history, matching the semantics of the SQLite-backed implementation.

The in-memory store is particularly valuable for testing session resume functionality. A test can create a session, add messages to it, and then simulate a resume by loading the history and passing it to a new agent instance. This verifies that the resume path works correctly without requiring a real SQLite database on disk.

---

## Verification Patterns

### Call Verification

The `MockProvider` tracks how many times each method is called and with what arguments. After the test completes, you can verify that the expected calls were made:

```rust
let provider = MockProvider::new();
// ... run the test ...

// Verify the expected number of calls
assert_eq!(provider.complete_call_count(), 2);
assert_eq!(provider.stream_call_count(), 0);

// Verify the arguments of specific calls
let first_request = provider.complete_requests()[0].clone();
assert_eq!(first_request.model().as_str(), "claude-sonnet-4-20250514");
assert!(first_request.messages().len() > 0);
```

Call verification is useful for ensuring that the agent does not make unnecessary LLM calls — for example, verifying that a cached result is used instead of making a new call, or that an agent stops after reaching its iteration limit rather than making one more call.

### Event Verification

When using a `CollectSink`, you can verify the sequence of events emitted during the test:

```rust
let sink = CollectSink::new();
// ... run the test with the sink ...

let events = sink.events();
assert!(events.iter().any(|e| matches!(e, StreamEvent::Token(_))));
assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolResult { .. })));
assert!(events.last().map_or(false, |e| matches!(e, StreamEvent::Done)));
```

Event verification complements call verification by checking what the agent produced, not just what it consumed. Together, they provide a complete picture of the agent's behavior during the test: the mock provider verifies the inputs (LLM calls), and the collect sink verifies the outputs (stream events).

### Dropping Unconsumed Expectations

When the test ends, the `MockProvider`'s `Drop` implementation checks whether all expected responses were consumed. If any expectations remain in the queue, the `Drop` implementation panics with a message listing the unconsumed responses. This catches a common testing mistake: scripting too many responses and never verifying that they were all used. A test that over-scripts responses may be passing by accident — the extra responses suggest that the test author expected more LLM calls than actually occurred, which could indicate a bug in the code under test.
