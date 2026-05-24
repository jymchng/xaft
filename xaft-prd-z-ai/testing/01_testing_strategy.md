# xaft Testing Strategy

## 1. Overview

The xaft testing strategy is a four-tier pyramid designed to maximize confidence while minimizing
feedback latency. Because xaft is built on the `agtrs` framework with compile-time macro validation,
many classes of bugs are eliminated before tests ever run. The remaining logic requires rigorous
testing across unit, integration, end-to-end, and property-based layers.

```
                    ╔══════════════╗
                    ║  Property-  ║   ← Few tests, deep invariants
                    ║ Based Tests  ║     (fuzzing, model checking)
                    ╚══════════════╝
                  ╔══════════════════╗
                  ║   E2E Tests      ║   ← Full pipeline validation
                  ║ (codegen, plan-  ║     (real LLM, real FS)
                  ║  execute, multi) ║
                  ╚══════════════════╝
              ╔════════════════════════╗
              ║  Integration Tests     ║   ← Subsystem interaction
              ║  (AgentExecutor, FS,   ║     (mock LLM, real FS)
              ║   Git, Tool dispatch)  ║
              ╚════════════════════════╝
          ╔══════════════════════════════╗
          ║     Unit Tests                ║   ← Fast, isolated, deterministic
          ║  (mock transport, in-memory  ║     (no I/O, no network)
          ║   stores, macro expansion)   ║
          ╚══════════════════════════════╝
```

### Testing Principles

1. **Determinism First**: Every unit and integration test must be 100% deterministic.
   No flaky tests allowed in CI.
2. **Mock at the Boundary**: All external dependencies (LLM APIs, filesystem, git)
   are mocked at the trait boundary, never at the function level.
3. **State Machine Testing**: Agent execution is modeled as a state machine; every
   state transition is tested.
4. **Compile-Time Elimination**: Use `#[agent]`/`#[tool]` macros to eliminate
   entire categories of runtime errors, reducing the test surface.

---

## 2. Unit Tests

### 2.1 Mock Transport Layer

The `Transport` trait is the primary seam for isolating LLM interactions. The mock
transport records requests and returns pre-configured responses, enabling deterministic
testing of the entire request/response pipeline.

```rust
/// Mock transport for deterministic unit testing
pub struct MockTransport {
    responses: VecDeque<LlmResponse>,
    recorded_requests: Vec<LlmRequest>,
    latency_simulator: Option<Duration>,
    error_injector: Option<ErrorPattern>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            responses: VecDeque::new(),
            recorded_requests: Vec::new(),
            latency_simulator: None,
            error_injector: None,
        }
    }

    /// Enqueue a response to be returned on the next request
    pub fn enqueue_response(mut self, response: LlmResponse) -> Self {
        self.responses.push_back(response);
        self
    }

    /// Enqueue a sequence of responses for multi-turn conversations
    pub fn enqueue_responses(mut self, responses: impl IntoIterator<Item = LlmResponse>) -> Self {
        for r in responses {
            self.responses.push_back(r);
        }
        self
    }

    /// Simulate network latency
    pub fn with_simulated_latency(mut self, duration: Duration) -> Self {
        self.latency_simulator = Some(duration);
        self
    }

    /// Inject errors at a specific pattern (every Nth request, random, etc.)
    pub fn with_error_pattern(mut self, pattern: ErrorPattern) -> Self {
        self.error_injector = Some(pattern);
        self
    }

    /// Retrieve all recorded requests for assertion
    pub fn recorded_requests(&self) -> &[LlmRequest] {
        &self.recorded_requests
    }

    /// Assert that the nth request contains a specific substring
    pub fn assert_request_contains(&self, index: usize, substring: &str) {
        let req = &self.recorded_requests[index];
        assert!(
            req.messages.iter().any(|m| m.content.contains(substring)),
            "Request {} did not contain '{}'",
            index, substring
        );
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&self, request: LlmRequest) -> Result<LlmResponse, TransportError> {
        self.recorded_requests.push(request.clone());

        // Simulate latency if configured
        if let Some(latency) = self.latency_simulator {
            tokio::time::sleep(latency).await;
        }

        // Check for error injection
        if let Some(ref pattern) = self.error_injector {
            if pattern.should_error(self.recorded_requests.len()) {
                return Err(TransportError::RateLimited {
                    retry_after: Some(Duration::from_secs(1)),
                });
            }
        }

        self.responses
            .pop_front()
            .ok_or(TransportError::NoResponseConfigured)
    }
}

/// Error injection patterns for resilience testing
pub enum ErrorPattern {
    /// Fail every Nth request
    EveryNth(usize),
    /// Fail the first N requests, then succeed
    FirstN(usize),
    /// Fail with a specific probability
    Random { probability: f64, seed: u64 },
    /// Fail after a specific request index
    AfterIndex(usize),
}
```

**Test Example: Tool Call Parsing**

```rust
#[tokio::test]
async fn test_tool_call_parsing_with_mock_transport() {
    let transport = MockTransport::new()
        .enqueue_response(LlmResponse::assistant()
            .with_content("I'll read the file first.")
            .with_tool_call("file_read", r#"{"path": "/src/main.rs"}"#)
            .build());

    let agent = AgentBuilder::new()
        .with_transport(transport.clone())
        .with_system_prompt("You are a coding assistant.")
        .build();

    let result = agent.execute("Read main.rs").await.unwrap();

    assert!(result.tool_calls().len() == 1);
    assert_eq!(result.tool_calls()[0].name, "file_read");
    transport.assert_request_contains(0, "Read main.rs");
}
```

### 2.2 InMemoryWorkspaceStore

The `InMemoryWorkspaceStore` provides a virtual filesystem for testing file operations
without touching the real disk. It supports all workspace operations including atomic
transactions.

```rust
pub struct InMemoryWorkspaceStore {
    files: RwLock<HashMap<PathBuf, String>>,
    metadata: RwLock<HashMap<PathBuf, FileMetadata>>,
    transaction_log: RwLock<Vec<TransactionEntry>>,
    snapshot_counter: AtomicU64,
}

impl InMemoryWorkspaceStore {
    pub fn new() -> Self {
        Self {
            files: RwLock::new(HashMap::new()),
            metadata: RwLock::new(HashMap::new()),
            transaction_log: RwLock::new(Vec::new()),
            snapshot_counter: AtomicU64::new(0),
        }
    }

    /// Initialize with a pre-populated file tree
    pub fn with_files(self, files: impl IntoIterator<Item = (PathBuf, String)>) -> Self {
        let mut file_map = self.files.write().unwrap();
        for (path, content) in files {
            file_map.insert(path, content);
        }
        drop(file_map);
        self
    }

    /// Create from a real directory (for integration test bootstrapping)
    pub fn from_directory(dir: &Path) -> Result<Self, WorkspaceError> {
        let mut store = Self::new();
        for entry in walkdir::WalkDir::new(dir) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let content = std::fs::read_to_string(entry.path())?;
                store.files.write().unwrap().insert(
                    entry.path().to_path_buf(),
                    content,
                );
            }
        }
        Ok(store)
    }
}

#[async_trait]
impl WorkspaceStore for InMemoryWorkspaceStore {
    async fn read_file(&self, path: &Path) -> Result<String, WorkspaceError> {
        self.files
            .read()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or(WorkspaceError::FileNotFound(path.to_path_buf()))
    }

    async fn write_file(&self, path: &Path, content: &str) -> Result<(), WorkspaceError> {
        self.files.write().unwrap().insert(path.to_path_buf(), content.to_string());
        Ok(())
    }

    async fn delete_file(&self, path: &Path) -> Result<(), WorkspaceError> {
        self.files
            .write()
            .unwrap()
            .remove(path)
            .map(|_| ())
            .ok_or(WorkspaceError::FileNotFound(path.to_path_buf()))
    }

    async fn list_files(&self, prefix: &Path) -> Result<Vec<PathBuf>, WorkspaceError> {
        let files = self.files.read().unwrap();
        Ok(files
            .keys()
            .filter(|p| p.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn begin_transaction(&self) -> Result<TransactionId, WorkspaceError> {
        let id = TransactionId(self.snapshot_counter.fetch_add(1, Ordering::SeqCst));
        // Snapshot current state for rollback
        let snapshot = self.files.read().unwrap().clone();
        self.transaction_log.write().unwrap().push(
            TransactionEntry::Snapshot { id, state: snapshot }
        );
        Ok(id)
    }

    async fn commit_transaction(&self, id: TransactionId) -> Result<(), WorkspaceError> {
        // Remove snapshot, transaction is committed
        self.transaction_log.write().unwrap().retain(
            |e| !matches!(e, TransactionEntry::Snapshot { id: tid, .. } if *tid == id)
        );
        Ok(())
    }

    async fn rollback_transaction(&self, id: TransactionId) -> Result<(), WorkspaceError> {
        let mut log = self.transaction_log.write().unwrap();
        if let Some(TransactionEntry::Snapshot { state, .. }) = log.iter().find(
            |e| matches!(e, TransactionEntry::Snapshot { id: tid, .. } if *tid == id)
        ) {
            *self.files.write().unwrap() = state.clone();
        }
        log.retain(|e| !matches!(e, TransactionEntry::Snapshot { id: tid, .. } if *tid == id));
        Ok(())
    }
}
```

**Test Example: Transactional Edit**

```rust
#[tokio::test]
async fn test_workspace_transaction_rollback() {
    let store = InMemoryWorkspaceStore::new()
        .with_files(vec![
            (PathBuf::from("/src/main.rs"), "fn main() {}".to_string()),
        ]);

    let tx = store.begin_transaction().await.unwrap();

    // Modify file within transaction
    store.write_file(&PathBuf::from("/src/main.rs"), "fn main() { /* modified */ }").await.unwrap();
    assert_eq!(
        store.read_file(&PathBuf::from("/src/main.rs")).await.unwrap(),
        "fn main() { /* modified */ }"
    );

    // Rollback should restore original
    store.rollback_transaction(tx).await.unwrap();
    assert_eq!(
        store.read_file(&PathBuf::from("/src/main.rs")).await.unwrap(),
        "fn main() {}"
    );
}
```

### 2.3 InMemoryTaskStore

The `InMemoryTaskStore` provides a virtual task/state store for testing agent execution
planning without persistence infrastructure.

```rust
pub struct InMemoryTaskStore {
    tasks: RwLock<HashMap<TaskId, Task>>,
    execution_log: RwLock<Vec<ExecutionEvent>>,
    state_transitions: RwLock<Vec<(TaskId, AgentState, AgentState)>>,
}

impl InMemoryTaskStore {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            execution_log: RwLock::new(Vec::new()),
            state_transitions: RwLock::new(Vec::new()),
        }
    }

    /// Assert that a task went through a specific sequence of states
    pub fn assert_state_sequence(&self, task_id: &TaskId, expected: &[AgentState]) {
        let transitions = self.state_transitions.read().unwrap();
        let task_transitions: Vec<_> = transitions
            .iter()
            .filter(|(id, _, _)| id == task_id)
            .collect();

        for (i, expected_state) in expected.iter().enumerate() {
            assert!(
                task_transitions.len() > i,
                "Expected state transition {} but only {} transitions recorded",
                i, task_transitions.len()
            );
            assert_eq!(
                task_transitions[i].2, *expected_state,
                "State transition {} mismatch: expected {:?}, got {:?}",
                i, expected_state, task_transitions[i].2
            );
        }
    }

    /// Get all execution events for inspection
    pub fn execution_events(&self) -> Vec<ExecutionEvent> {
        self.execution_log.read().unwrap().clone()
    }
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    async fn create_task(&self, task: Task) -> Result<TaskId, StoreError> {
        let id = task.id;
        self.tasks.write().unwrap().insert(id, task);
        Ok(id)
    }

    async fn update_task_state(
        &self,
        task_id: TaskId,
        from: AgentState,
        to: AgentState,
    ) -> Result<(), StoreError> {
        let mut tasks = self.tasks.write().unwrap();
        let task = tasks.get_mut(&task_id).ok_or(StoreError::TaskNotFound(task_id))?;
        assert_eq!(
            task.state, from,
            "CAS violation: expected state {:?} but found {:?}",
            from, task.state
        );
        task.state = to.clone();
        drop(tasks);

        self.state_transitions.write().unwrap().push((task_id, from, to));
        Ok(())
    }

    async fn log_execution_event(&self, event: ExecutionEvent) -> Result<(), StoreError> {
        self.execution_log.write().unwrap().push(event);
        Ok(())
    }

    async fn get_task(&self, task_id: TaskId) -> Result<Task, StoreError> {
        self.tasks
            .read()
            .unwrap()
            .get(&task_id)
            .cloned()
            .ok_or(StoreError::TaskNotFound(task_id))
    }
}
```

### 2.4 Macro Testability: `#[agent]` and `#[tool]`

The `#[agent]` and `#[tool]` macros generate code that must itself be testable. Each macro
expansion produces a companion test module that validates the generated impl blocks.

```rust
// What the user writes:
#[agent(name = "CodeReviewer", model = "claude-sonnet-4-20250514")]
impl CodeReviewer {
    #[system_prompt]
    fn system_prompt() -> &'static str {
        "You are an expert code reviewer."
    }

    #[tool(description = "Read a file from the workspace")]
    async fn read_file(&self, path: String) -> Result<String, ToolError> {
        self.workspace.read_file(Path::new(&path)).await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    #[tool(description = "Post a review comment")]
    async fn post_comment(&self, file: String, line: u32, comment: String) -> Result<(), ToolError> {
        // ...
        Ok(())
    }
}

// What the macro generates (simplified):
mod __xaft_code_reviewer_tests {
    use super::*;

    #[test]
    fn test_agent_name() {
        assert_eq!(CodeReviewer::agent_name(), "CodeReviewer");
    }

    #[test]
    fn test_tool_signatures() {
        let tools = CodeReviewer::tool_definitions();
        assert!(tools.iter().any(|t| t.name == "read_file"));
        assert!(tools.iter().any(|t| t.name == "post_comment"));

        let read_file_tool = tools.iter().find(|t| t.name == "read_file").unwrap();
        assert_eq!(read_file_tool.parameters.len(), 1);
        assert_eq!(read_file_tool.parameters[0].name, "path");
    }

    #[test]
    fn test_system_prompt_not_empty() {
        assert!(!CodeReviewer::system_prompt().is_empty());
    }
}
```

**Testing Macro Error Cases:**

```rust
// The macro should reject invalid agent definitions at compile time.
// These are negative compilation tests.

// ❌ REJECTED: Tool with no description
// #[agent(name = "Bad")]
// impl Bad {
//     #[tool]  // error: #[tool] requires `description = "..."`
//     async fn do_thing(&self) -> Result<(), ToolError> { Ok(()) }
// }

// ❌ REJECTED: Missing system_prompt
// #[agent(name = "Bad")]
// impl Bad {
//     #[tool(description = "do thing")]
//     async fn do_thing(&self) -> Result<(), ToolError> { Ok(()) }
// }
// // error: agent "Bad" must have exactly one #[system_prompt] function

// ❌ REJECTED: Tool returning non-Result type
// #[agent(name = "Bad")]
// impl Bad {
//     #[system_prompt] fn sp() -> &'static str { "" }
//     #[tool(description = "bad return")]
//     async fn do_thing(&self) -> String { String::new() }
// }
// // error: #[tool] methods must return Result<T, ToolError>
```

---

## 3. Integration Tests

### 3.1 AgentExecutor Integration

Tests the full agent execution loop with a mock LLM and in-memory workspace.

```
┌──────────────────────────────────────────────────────────────────┐
│                    AgentExecutor Integration Test                 │
│                                                                   │
│  ┌────────────┐    ┌──────────────┐    ┌────────────────────┐   │
│  │ MockTransport├───►│AgentExecutor │───►│InMemoryWorkspace   │   │
│  │             │    │              │    │   Store             │   │
│  │ responses[] │    │ plan→execute │    │                    │   │
│  │ recorded[]  │    │ loop         │    │ /src/main.rs       │   │
│  └────────────┘    └──────┬───────┘    │ /src/lib.rs         │   │
│                           │            └────────────────────┘   │
│                    ┌──────▼───────┐                             │
│                    │ ToolRegistry │                             │
│                    │ file_read    │                             │
│                    │ file_write   │                             │
│                    │ file_delete  │                             │
│                    │ shell_exec   │                             │
│                    └──────────────┘                             │
└──────────────────────────────────────────────────────────────────┘
```

```rust
#[tokio::test]
async fn test_agent_executor_multi_tool_workflow() {
    let workspace = InMemoryWorkspaceStore::new()
        .with_files(vec![
            (PathBuf::from("/src/main.rs"), r#"fn main() { println!("hello"); }"#.into()),
        ]);

    let transport = MockTransport::new()
        // Turn 1: Agent decides to read the file
        .enqueue_response(LlmResponse::assistant()
            .with_tool_call("file_read", r#"{"path": "/src/main.rs"}"#)
            .build())
        // Turn 2: Agent decides to edit the file
        .enqueue_response(LlmResponse::assistant()
            .with_tool_call("file_write", json!({
                "path": "/src/main.rs",
                "content": "fn main() { println!(\"hello, world!\"); }"
            }).to_string())
            .build())
        // Turn 3: Agent reports completion
        .enqueue_response(LlmResponse::assistant()
            .with_content("I've updated the greeting to 'hello, world!'")
            .build());

    let task_store = InMemoryTaskStore::new();

    let executor = AgentExecutor::new()
        .with_transport(transport.clone())
        .with_workspace(Arc::new(workspace.clone()))
        .with_task_store(Arc::new(task_store.clone()))
        .with_max_turns(10)
        .build();

    let result = executor.run("Change the greeting to 'hello, world!'").await.unwrap();

    // Verify final file state
    let content = workspace.read_file(&PathBuf::from("/src/main.rs")).await.unwrap();
    assert!(content.contains("hello, world!"));

    // Verify LLM was called 3 times
    assert_eq!(transport.recorded_requests().len(), 3);

    // Verify state transitions
    let task_id = result.task_id;
    task_store.assert_state_sequence(&task_id, &[
        AgentState::Planning,
        AgentState::Executing,
        AgentState::Executing,
        AgentState::Completed,
    ]);
}
```

### 3.2 FileEditor Integration

Tests the `FileEditor` subsystem with real filesystem operations in a temp directory.

```rust
#[tokio::test]
async fn test_file_editor_atomic_replace() {
    let temp = tempfile::tempdir().unwrap();
    let file_path = temp.path().join("src/main.rs");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, "fn main() { old_code(); }").unwrap();

    let editor = FileEditor::new(temp.path().to_path_buf());

    // Perform atomic replacement
    let change = FileChange::Replace {
        path: "src/main.rs".into(),
        search: "old_code()".to_string(),
        replace: "new_code()".to_string(),
    };

    let result = editor.apply(change).await.unwrap();
    assert!(result.success);

    // Verify file was modified
    let content = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "fn main() { new_code(); }");

    // Verify backup was created
    let backup = temp.path().join(".xaft/backups/src/main.rs.0");
    assert!(backup.exists());
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "fn main() { old_code(); }");
}

#[tokio::test]
async fn test_file_editor_failed_search_no_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let file_path = temp.path().join("src/main.rs");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, "fn main() { original(); }").unwrap();

    let editor = FileEditor::new(temp.path().to_path_buf());

    // Try to replace something that doesn't exist
    let change = FileChange::Replace {
        path: "src/main.rs".into(),
        search: "nonexistent()".to_string(),
        replace: "something()".to_string(),
    };

    let result = editor.apply(change).await;
    assert!(result.is_err());

    // File must be unchanged
    let content = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(content, "fn main() { original(); }");

    // No backup should exist
    let backup_dir = temp.path().join(".xaft/backups");
    assert!(!backup_dir.exists() || std::fs::read_dir(&backup_dir).unwrap().next().is_none());
}
```

### 3.3 GitRepo Integration

Tests git operations with a real (temporary) git repository.

```rust
#[tokio::test]
async fn test_git_repo_commit_and_diff() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GitRepo::init(temp.path()).await.unwrap();

    // Create initial commit
    std::fs::write(temp.path().join("main.rs"), "fn main() {}").unwrap();
    repo.add(&["main.rs"]).await.unwrap();
    repo.commit("Initial commit").await.unwrap();

    // Make changes
    std::fs::write(temp.path().join("main.rs"), "fn main() { /* updated */ }").unwrap();
    repo.add(&["main.rs"]).await.unwrap();
    repo.commit("Update main").await.unwrap();

    // Verify diff between commits
    let diff = repo.diff_range("HEAD~1", "HEAD").await.unwrap();
    assert!(diff.contains("updated"));

    // Verify log
    let log = repo.log(2).await.unwrap();
    assert_eq!(log.len(), 2);
    assert!(log[0].message.contains("Update main"));
    assert!(log[1].message.contains("Initial commit"));
}

#[tokio::test]
async fn test_git_repo_branching_workflow() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GitRepo::init(temp.path()).await.unwrap();

    // Setup main
    std::fs::write(temp.path().join("a.txt"), "v1").unwrap();
    repo.add(&["a.txt"]).await.unwrap();
    repo.commit("v1 on main").await.unwrap();

    // Create feature branch
    repo.create_branch("feature/x").await.unwrap();
    repo.checkout("feature/x").await.unwrap();

    std::fs::write(temp.path().join("b.txt"), "new file").unwrap();
    repo.add(&["b.txt"]).await.unwrap();
    repo.commit("Add b.txt on feature").await.unwrap();

    // Merge back
    repo.checkout("main").await.unwrap();
    repo.merge("feature/x").await.unwrap();

    let log = repo.log(3).await.unwrap();
    assert!(log.iter().any(|e| e.message.contains("Add b.txt")));
}
```

---

## 4. End-to-End Tests

### 4.1 Codegen Pipeline E2E

Tests the complete code generation pipeline from prompt to committed code using a real LLM
(in CI, using a recorded/replay transport).

```rust
#[tokio::test]
#[ignore] // Run with --ignored flag or in E2E CI stage
async fn e2e_codegen_pipeline() {
    let temp = tempfile::tempdir().unwrap();
    let repo = GitRepo::init(temp.path()).await.unwrap();

    // Setup a Rust project
    std::fs::write(temp.path().join("Cargo.toml"), r#"
        [package]
        name = "test_proj"
        version = "0.1.0"
        edition = "2021"
    "#).unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();

    repo.add(&["."]).await.unwrap();
    repo.commit("Initial").await.unwrap();

    // Run xaft
    let transport = RecordedTransport::from_file("fixtures/codegen_add_function.jsonl");
    let workspace = RealWorkspaceStore::new(temp.path());

    let executor = AgentExecutor::new()
        .with_transport(transport)
        .with_workspace(Arc::new(workspace))
        .with_git_repo(Arc::new(repo.clone()))
        .with_max_turns(20)
        .build();

    let result = executor
        .run("Add a greet() function that prints 'Hello, xaft!' and call it from main")
        .await
        .unwrap();

    // Verify the code was generated
    let main_rs = std::fs::read_to_string(temp.path().join("src/main.rs")).unwrap();
    assert!(main_rs.contains("greet"));
    assert!(main_rs.contains("Hello, xaft!"));

    // Verify it was committed
    let log = repo.log(2).await.unwrap();
    assert!(log[0].message.contains("greet") || log[0].message.contains("Add"));

    // Verify the code compiles
    let status = Command::new("cargo")
        .args(["check"])
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(status.success());
}
```

### 4.2 Plan-and-Execute E2E

Tests the planner-decomposer-executor pipeline for complex multi-step tasks.

```rust
#[tokio::test]
#[ignore]
async fn e2e_plan_and_execute() {
    let temp = tempfile::tempdir().unwrap();
    // Setup a project with multiple files needing changes
    setup_multi_file_project(temp.path());

    let transport = RecordedTransport::from_file("fixtures/plan_execute_refactor.jsonl");

    let result = XaftRunner::new(temp.path())
        .with_transport(transport)
        .with_mode(ExecutionMode::PlanAndExecute)
        .with_max_planning_turns(3)
        .with_max_execution_turns(30)
        .run("Refactor the error handling to use anyhow throughout the project")
        .await
        .unwrap();

    // Verify plan was created and followed
    assert!(result.plan.is_some());
    assert!(result.plan.unwrap().steps.len() >= 3);

    // Verify all files were modified
    let modified_files = result.modified_files();
    assert!(modified_files.contains(&PathBuf::from("src/main.rs")));
    assert!(modified_files.contains(&PathBuf::from("src/lib.rs")));
    assert!(modified_files.contains(&PathBuf::from("src/error.rs")));

    // Verify anyhow is used
    for file in &modified_files {
        let content = std::fs::read_to_string(temp.path().join(file)).unwrap();
        assert!(
            !content.contains("Box<dyn Error>"),
            "File {:?} still uses Box<dyn Error>",
            file
        );
    }
}
```

### 4.3 Multi-Agent E2E

Tests coordinator-delegator pattern with multiple specialized agents.

```rust
#[tokio::test]
#[ignore]
async fn e2e_multi_agent_code_review() {
    let temp = tempfile::tempdir().unwrap();
    setup_rust_project(temp.path());

    let transport = MultiAgentTransport::new()
        .for_agent("coordinator", RecordedTransport::from_file("fixtures/coordinator.jsonl"))
        .for_agent("reviewer", RecordedTransport::from_file("fixtures/reviewer.jsonl"))
        .for_agent("fixer", RecordedTransport::from_file("fixtures/fixer.jsonl"));

    let result = XaftRunner::new(temp.path())
        .with_transport(transport)
        .with_mode(ExecutionMode::MultiAgent {
            agents: vec![
                AgentConfig::new("coordinator").with_model("claude-sonnet-4-20250514"),
                AgentConfig::new("reviewer").with_model("claude-sonnet-4-20250514"),
                AgentConfig::new("fixer").with_model("claude-sonnet-4-20250514"),
            ],
            max_delegations: 10,
        })
        .run("Review the codebase for common mistakes and fix them")
        .await
        .unwrap();

    // Verify delegation happened
    assert!(result.delegations.len() >= 2);
    assert!(result.delegations.iter().any(|d| d.agent == "reviewer"));
    assert!(result.delegations.iter().any(|d| d.agent == "fixer"));

    // Verify code was modified after fix
    assert!(!result.modified_files().is_empty());
}
```

---

## 5. Property-Based Testing

### 5.1 State Machine Properties

Using `proptest` to verify invariant preservation across agent state transitions.

```rust
proptest! {
    /// Any valid sequence of state transitions must never skip the Planning state
    #[test]
    fn prop_state_transitions_always_start_from_planning(
        transitions in prop::collection::vec(
            any::<AgentState>(),
            0..20
        )
    ) {
        let mut machine = AgentStateMachine::new();

        for state in transitions {
            if machine.can_transition_to(&state) {
                let prev = machine.current_state().clone();
                machine.transition(state).unwrap();

                // Invariant: no transition can skip Planning as first state
                if prev == AgentState::Initialized {
                    prop_assert_eq!(prev, AgentState::Planning);
                }
            }
        }

        // Invariant: terminal states are never left
        if machine.current_state().is_terminal() {
            prop_assert!(machine.current_state() == &AgentState::Completed
                || machine.current_state() == &AgentState::Failed);
        }
    }

    /// Transaction rollback always restores exact previous state
    #[test]
    fn prop_transaction_rollback_restores_state(
        initial_files in prop::collection::hash_map(
            any::<String>().prop_map(|s| PathBuf::from(s)),
            any::<String>(),
            0..10
        ),
        mutations in prop::collection::vec(
            (any::<PathBuf>(), any::<String>()),
            0..10
        )
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let store = InMemoryWorkspaceStore::new()
                .with_files(initial_files.clone());

            let tx = store.begin_transaction().await.unwrap();

            // Apply random mutations
            for (path, content) in &mutations {
                let _ = store.write_file(path, content).await;
            }

            // Rollback
            store.rollback_transaction(tx).await.unwrap();

            // Verify exact restoration
            for (path, content) in &initial_files {
                let restored = store.read_file(path).await.unwrap();
                prop_assert_eq!(&restored, content);
            }
        });
    }
}
```

### 5.2 File Edit Commutativity Properties

```rust
proptest! {
    /// Non-overlapping edits should be commutative
    #[test]
    fn prop_non_overlapping_edits_commutative(
        original in "fn main() { let a = 1; let b = 2; let c = 3; }",
        edit_a_search in "[a-z]+",
        edit_a_replace in "[a-z]+",
        edit_b_search in "[a-z]+",
        edit_b_replace in "[a-z]+"
    ) {
        let mut text_a_first = original.clone();
        let mut text_b_first = original.clone();

        // Apply A then B
        let a_ok = apply_edit(&mut text_a_first, &edit_a_search, &edit_a_replace);
        let b_ok_after_a = apply_edit(&mut text_a_first, &edit_b_search, &edit_b_replace);

        // Apply B then A
        let b_ok = apply_edit(&mut text_b_first, &edit_b_search, &edit_b_replace);
        let a_ok_after_b = apply_edit(&mut text_b_first, &edit_a_search, &edit_a_replace);

        // If both orders succeed, results must be identical
        if a_ok && b_ok_after_a && b_ok && a_ok_after_b {
            // Check that edits don't overlap (they target different text)
            let a_pos = original.find(&edit_a_search);
            let b_pos = original.find(&edit_b_search);
            if let (Some(ap), Some(bp)) = (a_pos, b_pos) {
                let a_range = ap..(ap + edit_a_search.len());
                let b_range = bp..(bp + edit_b_search.len());
                if !a_range.contains(&b_range.start) && !b_range.contains(&a_range.start) {
                    prop_assert_eq!(text_a_first, text_b_first);
                }
            }
        }
    }
}
```

---

## 6. Test Infrastructure

### 6.1 RecordedTransport (LLM Replay)

For deterministic E2E tests without API costs, we record real LLM interactions
and replay them.

```rust
pub struct RecordedTransport {
    recordings: Vec<RecordedTurn>,
    request_index: usize,
}

struct RecordedTurn {
    expected_request_pattern: Option<String>,
    response: LlmResponse,
}

impl RecordedTransport {
    pub fn from_file(path: impl AsRef<Path>) -> Self {
        let content = std::fs::read_to_string(path).unwrap();
        let recordings: Vec<RecordedTurn> = content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Self { recordings, request_index: 0 }
    }

    /// Record a live session for later replay
    pub async fn record_session(
        real_transport: &impl Transport,
        prompt: &str,
        agent: &Agent,
        output_path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = std::fs::File::create(output_path)?;
        // ... execute and serialize each turn
        Ok(())
    }
}
```

### 6.2 CI Pipeline

```
┌─────────────────────────────────────────────────────────────────┐
│                        CI Pipeline                               │
│                                                                  │
│  ┌──────────┐   ┌───────────┐   ┌───────────┐   ┌───────────┐ │
│  │  Lint &   │──►│ Unit Tests│──►│ Integration│──►│  E2E Tests │ │
│  │  Format   │   │ (1 min)   │   │ Tests      │   │ (10 min)  │ │
│  │  (30 sec) │   │           │   │ (3 min)    │   │           │ │
│  └──────────┘   └───────────┘   └───────────┘   └───────────┘ │
│       │                                               │         │
│       ▼                                               ▼         │
│  ┌──────────┐                                   ┌───────────┐  │
│  │ Clippy + │                                   │ Property  │  │
│  │ Macro    │                                   │ Tests     │  │
│  │ Checks   │                                   │ (5 min)   │  │
│  └──────────┘                                   └───────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 6.3 Test Coverage Targets

| Layer                | Target Coverage | Max Duration |
|----------------------|-----------------|--------------|
| Unit Tests           | ≥ 90%           | 60 seconds   |
| Integration Tests    | ≥ 80%           | 180 seconds  |
| E2E Tests            | ≥ 60% (paths)   | 600 seconds  |
| Property Tests       | N/A (invariant) | 300 seconds  |
| Macro Compile Tests  | 100% (error cases) | 120 seconds |

---

## 7. Summary

The xaft testing strategy leverages Rust's type system and macro framework to eliminate
entire categories of bugs at compile time. The remaining test surface is covered by a
structured pyramid: fast unit tests with mock transport and in-memory stores, integration
tests for subsystem boundaries, E2E tests for full pipeline validation, and property-based
tests for invariant verification. The `RecordedTransport` system enables deterministic
replay of real LLM interactions, making E2E tests both reliable and cost-free in CI.
