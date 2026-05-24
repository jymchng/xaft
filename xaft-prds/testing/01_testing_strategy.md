# Testing Strategy

## Test Pyramid

```
                    ╱──────────────╲
                   ╱  E2E Tests     ╲     xaft-test/scenarios/
                  ╱  (real LLM API)  ╲    slow, expensive, ~5 tests
                 ╱────────────────────╲
                ╱  Integration Tests   ╲  xaft-test/integration/
               ╱  (mock LLM, real git)  ╲ medium, ~50 tests
              ╱────────────────────────── ╲
             ╱     Unit Tests              ╲ each crate's tests/
            ╱  (all mocked, deterministic)  ╲ fast, ~500 tests
           ╱────────────────────────────────╲
```

## Unit Test Infrastructure

### MockTransport (from agtrs-runtime)

```rust
// All unit tests use MockTransport — no real API calls
let transport = Arc::new(MockTransport::new());
transport.queue_text("I'll read the file first.").await;
transport.queue_tool_call("read_file", serde_json::json!({"path": "src/auth.rs"})).await;
transport.queue_text("Now I'll update the file.").await;
transport.queue_tool_call("write_file", serde_json::json!({
    "path": "src/auth.rs",
    "content": "// updated content"
})).await;
transport.queue_text("Done. The auth module has been updated.").await;

let llm = Arc::new(MockLlmProvider::new(transport));
```

### TestHarness (xaft-test)

```rust
pub struct TestHarness {
    /// Temp directory with git-initialized project
    pub dir: TempDir,
    /// WorkspaceEditor rooted at temp dir
    pub workspace: Arc<WorkspaceEditor>,
    /// Real GitRepo (uses temp dir)
    pub git: Arc<GitRepo>,
    /// Mock LLM provider
    pub llm: Arc<MockLlmProvider>,
    /// Mock cheap LLM (for planner)
    pub cheap_llm: Arc<MockLlmProvider>,
    /// Session store
    pub store: Arc<InMemoryTaskStore>,
    /// Built XaftSession
    pub session: Arc<XaftSession>,
}

impl TestHarness {
    pub async fn new() -> Result<Self, XaftError>;

    /// Create a file in the test workspace
    pub async fn create_file(&self, path: &str, content: &str) -> PathBuf;

    /// Run a task with queued LLM responses
    pub async fn run_with_responses(
        &self,
        goal: &str,
        responses: Vec<MockResponse>,
    ) -> Result<SessionResult, XaftError>;

    /// Assert file content equals expected
    pub fn assert_file_content(&self, path: &str, expected: &str);

    /// Assert file was modified
    pub fn assert_file_modified(&self, path: &str);

    /// Assert test count
    pub async fn assert_cargo_test_passes(&self) -> Result<(), XaftError>;
}
```

### Example Unit Test

```rust
#[tokio::test]
async fn code_agent_reads_and_writes_file() {
    let harness = TestHarness::new().await.unwrap();
    harness.create_file("src/lib.rs", "pub fn hello() -> &'static str { \"world\" }");

    // Queue: model reads file, then writes updated version
    harness.llm.queue_tool_call("read_file", json!({"path": "src/lib.rs"})).await;
    harness.llm.queue_tool_call("write_file", json!({
        "path": "src/lib.rs",
        "content": "pub fn hello() -> &'static str { \"hello, world!\" }"
    })).await;
    harness.llm.queue_text("Updated the greeting.").await;

    let result = harness.run_with_responses(
        "Change the greeting in src/lib.rs to 'hello, world!'",
        vec![],
    ).await.unwrap();

    assert!(result.success);
    harness.assert_file_content("src/lib.rs",
        "pub fn hello() -> &'static str { \"hello, world!\" }");
}
```

## Integration Test Scenarios

Tests that use a real git repo + real file system but mock LLM:

```
xaft-test/integration/
├── basic_edit.rs           — read, modify, commit single file
├── multi_file_refactor.rs  — modify multiple related files
├── fixer_loop.rs           — intentional failure → FixerAgent
├── session_resume.rs       — simulate crash, resume from checkpoint
├── parallel_agents.rs      — two non-conflicting steps in parallel
├── approval_gate.rs        — approval dialog flow
└── cost_budget.rs          — exceed budget mid-task
```

## End-to-End Tests

Full runs against real LLM API (gated by `XAFT_E2E_TESTS=1`):

```
xaft-test/e2e/
├── hello_world.rs          — simplest possible edit task
├── add_function.rs         — add a new function to an existing file
├── fix_compilation.rs      — broken code → FixerAgent → passes cargo check
└── swe_bench_mini.rs       — 5 SWE-bench Lite tasks (correctness benchmark)
```

## Signal/Event Assertions

```rust
#[tokio::test]
async fn emits_plan_created_signal() {
    let harness = TestHarness::new().await.unwrap();
    // ... queue responses ...

    let signal_count = Arc::new(AtomicU32::new(0));
    let sc = Arc::clone(&signal_count);
    harness.session.signal_bus.on::<PlanCreated>(move |_| {
        sc.fetch_add(1, Ordering::Relaxed);
    });

    harness.run("add a test").await.unwrap();

    assert_eq!(signal_count.load(Ordering::Relaxed), 1);
}
```

## Coverage Targets

| Crate | Target line coverage |
|---|---|
| `xaft-core` | > 90% |
| `xaft-orchestrator` | > 85% |
| `xaft-agents` | > 80% |
| `xaft-tools` | > 90% |
| `xaft-tui` | > 60% (UI testing is harder) |
| `xaft-index` | > 85% |
| `xaft-plugin` | > 80% |

## References

- agtrs: `agtrs-runtime/src/testing.rs`
- agtrs: `agtrs-runtime/tests/basic_agent_loop.rs`
- agtrs guide: `guides/07-testing-agents.md`
