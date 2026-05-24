cat > ./01_testing_strategy.md << 'EOF'
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
EOF

cat > ./02_benchmarking.md << 'EOF'
# Benchmarking Strategy

## Performance Targets

| Metric | Target | Measurement method |
|---|---|---|
| Repository indexing (100K LOC) | < 5s | `criterion` benchmark |
| Time-to-first-token | < 500ms | Signal timestamp delta |
| TUI frame time (render) | < 25ms | `criterion` |
| Checkpoint save | < 50ms | Unit test timing |
| Session resume from checkpoint | < 2s | Integration test |
| File write (atomic) | < 5ms | Benchmark |
| Fuzzy search (10K files) | < 100ms | Benchmark |
| Patch apply (1000 hunks) | < 200ms | Benchmark |

## Criterion Benchmarks

```rust
// xaft-index/benches/indexing.rs
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_index_build(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all().build().unwrap();

    c.bench_function("index_build_10k_lines", |b| {
        b.to_async(&rt).iter(|| async {
            let dir = generate_rust_files(100, 100); // 100 files × 100 lines
            let index = RepoIndex::build(&dir.path()).await.unwrap();
            criterion::black_box(index);
        });
    });
}

fn bench_symbol_search(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all().build().unwrap();

    let index = rt.block_on(async {
        let dir = generate_rust_files(1000, 100);
        RepoIndex::build(dir.path()).await.unwrap()
    });

    c.bench_function("symbol_search", |b| {
        b.to_async(&rt).iter(|| async {
            criterion::black_box(
                index.symbols.search("process_request", 10).await
            );
        });
    });
}
```

## SWE-Bench Evaluation

`xaft` targets 75%+ pass rate on SWE-bench Lite (300 GitHub issues):

```bash
# Run SWE-bench evaluation
XAFT_E2E_TESTS=1 XAFT_BENCH=swe_bench \
  cargo test --test swe_bench -- --nocapture 2>&1 | tee swe_bench_results.json
```

Reporting format:
```json
{
  "total": 300,
  "passed": 228,
  "failed": 72,
  "pass_rate": 0.76,
  "avg_cost_usd": 0.43,
  "avg_duration_secs": 187,
  "avg_turns": 14.2
}
```

## Cost Benchmarks

Track cost per task type:

| Task type | Avg cost (Claude 3.5 Sonnet) | Avg cost (Gemini Flash) |
|---|---|---|
| Single file edit | $0.02–0.05 | $0.002–0.008 |
| Multi-file refactor (5 files) | $0.08–0.20 | $0.010–0.030 |
| Test generation (10 tests) | $0.05–0.10 | $0.008–0.015 |
| Bug fix (with test failure) | $0.10–0.30 | $0.015–0.040 |
| Architecture migration | $0.30–1.00 | $0.050–0.150 |

## References

- Criterion: https://bheisler.github.io/criterion.rs/book/
- SWE-bench: https://www.swebench.com/
EOF

echo "Testing docs done"