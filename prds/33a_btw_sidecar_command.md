# PRD 33a — `/btw`: Non-Interrupting Sidecar Question

## Status: Draft
## Depends on: PRD 32a (slash command system), PRD 23 (ExploreAgentPool)

---

## 1. Problem

When an xaft agent pipeline is running (Planner → Coder → QA cycle), the
TUI input bar is locked — the user cannot type anything without the input
being queued or the pipeline being interrupted.

Users frequently want to ask quick questions mid-run without stopping the
pipeline:

- "What's the name of the token refill function again?"
- "Is `asyncio.Lock` safe from multiple threads?"
- "What does OWASP say about rate-limit headers?"

These are **low-stakes informational queries** unrelated to the agent's
current task. Forcing the user to either interrupt the pipeline or wait
until completion creates friction.

Claude Code solves this with `/btw`: a sidecar command that spawns a
short-lived, read-only agent answering the question independently while
the main pipeline continues uninterrupted.

---

## 2. Goals

1. Typing `/btw <question>` while a pipeline is running spawns a
   lightweight sidecar agent that answers the question.
2. The sidecar agent does NOT block or interrupt the main pipeline.
3. The sidecar answer appears in the transcript labeled
   `[btw]` to distinguish it from main-pipeline output.
4. The sidecar agent has access to the same `working_dir` and
   `XaftConfig` but gets a **separate, blank conversation context** —
   it does not see the main pipeline's messages.
5. The sidecar agent has a limited tool set: read-only file tools and
   shell (`bash_exec` is excluded).
6. At most one sidecar agent runs at a time; a second `/btw` while one
   is running queues behind it (FIFO, max queue depth 3).
7. The input bar remains **active** while the sidecar runs so the user
   can ask follow-up questions or start another sidecar.

---

## 3. Non-Goals

- Sidecar agents that write files or commit code.
- Sidecar agents persisted to the session store (ephemeral only).
- Multi-turn sidecar conversations (one question → one answer).
- `/btw` when no pipeline is running falls through to normal agent
  dispatch — no special case.
- Cost attribution to a separate "sidecar budget" (sidecar usage accrues
  to the session total via `ModelCallComplete` like any other agent call).

---

## 4. Architecture

```
User types: /btw is asyncio.Lock thread-safe?
                │
                ▼
  AppState::handle_submit
    └── detects /btw prefix, pipeline may be active or idle
    └── SlashCommand::Btw { question: String }
                │
                ▼
  BtwHandler::execute_async(ctx)
    ├── ctx.btw_queue.enqueue(question)
    └── CommandResult::Lines(["[btw] queued: is asyncio.Lock…"])

  BtwQueue::run_loop (background tokio task, single-consumer)
    ├── dequeues question
    ├── SidecarRunRequest { question, working_dir, read_only_tools }
    ├── XaftRuntime::run_sidecar(req, signal_bus)
    │     └── single-agent HandoffOrchestrator, max_turns = 5
    └── emits XaftBtwAnswerReady { question, answer, duration_ms }

  AppState::handle_event(TuiEvent::BtwAnswer)
    └── mutations.push(CommitLine([btw] …))
```

### 4.1 `SlashCommand::Btw`

Add to the existing `SlashCommand` enum:

```rust
Btw { question: String },
```

Parser: `/btw <anything after the space>` → `SlashCommand::Btw { question }`.
`/btw` with no args → `CommandResult::Error("/btw requires a question.")`.

### 4.2 `SidecarRunRequest`

```rust
pub struct SidecarRunRequest {
    pub question:      String,
    pub working_dir:   PathBuf,
    pub config:        Arc<XaftConfig>,
    pub terminal_cols: u16,
    /// Cloned signal bus — sidecar shares the bus so cost signals
    /// reach the session tracker, but it uses its own agent IDs.
    pub signal_bus:    Arc<SignalBus>,
}
```

### 4.3 `BtwQueue` — serialising sidecar execution

```rust
pub struct BtwQueue {
    sender:   mpsc::Sender<SidecarRunRequest>,
    receiver: Arc<Mutex<mpsc::Receiver<SidecarRunRequest>>>,
    /// Number of queued (not yet started) requests.
    pending:  Arc<AtomicUsize>,
}

impl BtwQueue {
    pub const MAX_QUEUE: usize = 3;

    /// Returns Err if queue is full.
    pub fn enqueue(&self, req: SidecarRunRequest) -> Result<(), BtwQueueFull> {
        if self.pending.load(Ordering::Relaxed) >= Self::MAX_QUEUE {
            return Err(BtwQueueFull);
        }
        self.pending.fetch_add(1, Ordering::Relaxed);
        self.sender.try_send(req).map_err(|_| BtwQueueFull)
    }
}
```

`TuiApp::run` spawns one `btw_queue_worker` tokio task:

```rust
async fn btw_queue_worker(
    receiver: Arc<Mutex<mpsc::Receiver<SidecarRunRequest>>>,
    pending: Arc<AtomicUsize>,
    signal_bus: Arc<SignalBus>,
) {
    loop {
        let req = { receiver.lock().await.recv().await };
        let Some(req) = req else { break };
        pending.fetch_sub(1, Ordering::Relaxed);
        // Run sidecar — serial, one at a time.
        match XaftRuntime::run_sidecar(req, Arc::clone(&signal_bus)).await {
            Ok(answer) => { /* XaftBtwAnswerReady emitted inside run_sidecar */ }
            Err(e)     => signal_bus.emit(XaftBtwAnswerReady {
                question: "unknown".into(),
                answer: format!("[btw error] {e}"),
                duration_ms: 0.0,
            }),
        }
    }
}
```

### 4.4 `XaftRuntime::run_sidecar`

```rust
pub async fn run_sidecar(
    req: SidecarRunRequest,
    signal_bus: Arc<SignalBus>,
) -> Result<String, RuntimeError> {
    let start = Instant::now();

    // Read-only tool registry — no bash_exec, no write_file, no delete_file.
    let tools = ToolRegistry::builder()
        .add(ReadFileTool::new(&req.working_dir))
        .add(ListDirTool::new(&req.working_dir))
        .add(GrepSearchTool::new(&req.working_dir))
        .build();

    let system_prompt = format!(
        "You are a concise read-only assistant. Answer the following question \
         about the repository at {:?}. Do not suggest edits. Maximum 150 words.",
        req.working_dir
    );

    let agent = AgentBuilder::new("btw-sidecar")
        .system_prompt(system_prompt)
        .tools(tools)
        .max_turns(5)
        .build(req.config.agent("default").clone())?;

    // Blank conversation — no shared history.
    let mut conv = InMemoryConversationStore::new();
    conv.append(Message::user(req.question.clone())).await?;

    let orchestrator = HandoffOrchestrator::single(agent);
    let result = orchestrator.run(&mut conv).await?;

    let answer = result.final_text.unwrap_or_else(|| "(no answer)".into());
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

    signal_bus.emit(XaftBtwAnswerReady {
        question: req.question,
        answer: answer.clone(),
        duration_ms,
    });

    Ok(answer)
}
```

### 4.5 Transcript rendering

The answer is committed as a `StyledLine` with:
- `LineKind::System`
- Prefix `[btw]` in theme accent colour (cyan in the default theme)
- Body: the answer text, word-wrapped to `terminal_cols - 7`

```
[btw]  asyncio.Lock is thread-safe: it uses the event loop's
       internal lock, which is not re-entrant but is safe across
       coroutines. It is NOT safe across OS threads.
```

If the sidecar was queued (not run immediately), the queued-confirmation
line is printed when `/btw` is typed, and the answer line replaces the
waiting indicator when the answer arrives.

### 4.6 Queued state display

When the queue already has one pending item:
```
[btw] queued (1 pending) — "is asyncio.Lock thread-safe?"
```

When queue is full (3 pending):
```
[btw] queue full (3 pending). Try again after the current question finishes.
```

### 4.7 `/btw` when no pipeline is running

No special case needed: `run_sidecar` creates a throwaway
`HandoffOrchestrator` regardless. The only difference is the user doesn't
see a "queue" message — the sidecar starts immediately and the answer
arrives within a few seconds.

### 4.8 Cost tracking

The sidecar agent uses the shared `SignalBus`. Its `ModelCallComplete`
signals are handled by the cost tracker exactly like main-pipeline calls.
The session total includes sidecar tokens. The `/cost` table shows them
under agent name `"btw-sidecar"`.

### 4.9 Error handling

| Condition | Behaviour |
|-----------|-----------|
| Sidecar hits `max_turns = 5` | Emit `XaftBtwAnswerReady` with whatever partial text was generated |
| Provider returns error | Emit `XaftBtwAnswerReady { answer: "[btw error] <message>" }` |
| `working_dir` read permission denied | `read_file` tool returns `ToolResult { is_error: true }` — sidecar can still answer from knowledge |
| Queue full | `BtwHandler` returns `CommandResult::Error("btw queue full…")` — nothing queued |
| TUI exits while sidecar is running | Worker task is dropped; JoinHandle is aborted — sidecar answer may not appear |

---

## 5. Key Invariants

- **No shared state mutation between sidecar and main pipeline.** The
  sidecar gets a cloned `Arc<XaftConfig>` and its own blank
  `InMemoryConversationStore`. It must not write to the shared
  `SessionManager` or the session `ConversationStore`.
- **Main pipeline signals continue normally.** `SlashCommandExecuted`,
  `LlmCallComplete`, etc. from the main pipeline are unaffected.
- **Sidecar signals use `XaftBtwAnswerReady`**, not `AgentOutputChunk`,
  so the TUI can label them distinctly.
- **No write tools.** Tool registry is constructed with a whitelist;
  `bash_exec`, `write_file`, `delete_file`, and `git_commit` are never
  added.
- **Signal bus is shared.** The sidecar emits on the same bus as the main
  pipeline. The TUI bridge filters by signal type: `XaftBtwAnswerReady`
  maps to `TuiEvent::BtwAnswer`; the main pipeline's `AgentOutputChunk`
  maps to `TuiEvent::AgentChunk`. No interleaving of transcript lines
  occurs because `AppState` processes events serially.

---

## 6. New Signal

```rust
/// Emitted when a /btw sidecar agent finishes (success or error).
#[derive(Debug, Clone)]
pub struct XaftBtwAnswerReady {
    pub question:    String,
    pub answer:      String,
    pub duration_ms: f64,
    pub is_error:    bool,
}
```

The TUI `EventBridge` converts this to a `TuiEvent::BtwAnswer { answer,
is_error }` that `AppState::handle_event` uses to commit the `[btw]` line.

---

## 7. `CommandContext` addition

```rust
pub btw_queue: Option<Arc<BtwQueue>>,
```

`AppState::build_command_context` provides the shared `Arc<BtwQueue>`.
`BtwHandler` enqueues the `SidecarRunRequest` via this handle.

---

## 8. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/slash/mod.rs` | Add `Btw { question }` variant to `SlashCommand` |
| `crates/xaft-tui/src/slash/parser.rs` | Add `/btw` to `COMMAND_TABLE` |
| `crates/xaft-tui/src/slash/commands/btw.rs` | `BtwHandler`, `BtwQueue`, `BtwQueueFull` |
| `crates/xaft-runtime/src/runtime.rs` | `run_sidecar()` method |
| `crates/xaft-agent/src/signals.rs` | `XaftBtwAnswerReady` signal |
| `crates/xaft-tui/src/bridge.rs` | Handle `XaftBtwAnswerReady` → `TuiEvent::BtwAnswer` |
| `crates/xaft-tui/src/state.rs` | Handle `TuiEvent::BtwAnswer` → commit `[btw]` line |
| `crates/xaft-tui/src/app.rs` | Spawn `btw_queue_worker` task |

---

## 9. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | `/btw what is the rate-limit algorithm?` while a pipeline is running spawns a sidecar and the main pipeline continues. |
| AC2 | The sidecar answer appears in the transcript prefixed with `[btw]` in a visually distinct style. |
| AC3 | The sidecar agent cannot call `bash_exec` or any write tool (ToolRegistry enforced — test that the tool names list does not include forbidden tools). |
| AC4 | `/btw` with no argument returns an error line: `/btw requires a question.` |
| AC5 | Two rapid `/btw` commands: the first starts immediately; the second queues and starts after the first finishes. |
| AC6 | Four rapid `/btw` commands: the fourth is rejected with "queue full". |
| AC7 | `XaftBtwAnswerReady` signal is emitted with `is_error = false` on success. |
| AC8 | `XaftBtwAnswerReady` is emitted with `is_error = true` and the error message when the sidecar fails. |
| AC9 | Sidecar token usage appears under `"btw-sidecar"` in `/cost` output. |

---

## 10. Tests

### Unit tests

`crates/xaft-tui/src/slash/commands/btw.rs`:

```rust
#[tokio::test]
async fn btw_command_parses_question() {
    let result = SlashCommandParser::parse("/btw is asyncio.Lock thread-safe?");
    assert_matches!(result, Some(SlashCommand::Btw { question })
        if question == "is asyncio.Lock thread-safe?");
}

#[tokio::test]
async fn btw_no_args_returns_error() {
    let result = SlashCommandParser::parse("/btw");
    // Parser returns Btw { question: "" } or None; handler converts to error.
}

#[tokio::test]
async fn btw_queue_enqueue_and_full() {
    let queue = BtwQueue::new();
    for _ in 0..BtwQueue::MAX_QUEUE {
        queue.enqueue(dummy_request()).unwrap();
    }
    assert!(queue.enqueue(dummy_request()).is_err());
}
```

### Integration tests

`crates/xaft-tui/tests/btw_integration.rs`:

```rust
#[tokio::test]
async fn btw_sidecar_receives_read_only_tool_registry() {
    // Verify ToolRegistry passed to run_sidecar does not contain
    // bash_exec, write_file, or delete_file.
}

#[tokio::test]
async fn btw_answer_appears_with_btw_prefix() {
    // Mock LLM returns "yes it is safe".
    // Assert committed StyledLine starts with "[btw]".
}

#[tokio::test]
async fn btw_answer_ready_signal_emitted() {
    // Subscribe to XaftBtwAnswerReady before running sidecar.
    // Assert signal received with is_error = false.
}

#[tokio::test]
async fn btw_queue_serialises_two_requests() {
    // Enqueue two requests with a mock that delays 50ms each.
    // Assert second starts only after first XaftBtwAnswerReady is emitted.
}
```
