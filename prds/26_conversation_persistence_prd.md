# PRD 26: Conversation Persistence & Resume

**Status:** Proposal  
**Target:** next patch release  
**Related crates:** `xaft-session`, `xaft-runtime`, `xaft-tui`, `xaft-cli`, `agtrs-store`

---

## Problem

The agent currently has no memory between turns or between sessions. Two separate failures:

**Intra-session amnesia.** When the user sends a second message in the same TUI session (e.g., "now fix the test for that"), the agent starts fresh — it has no access to what it wrote or the user said in the first task. The TUI shows a multi-turn UI but the agents see only the current task string.

**Inter-session amnesia.** When the user closes xaft and opens it again, all prior context is gone. There is no way to pick up where you left off.

The infrastructure is almost entirely already built:
- `xaft-session` owns a `sessions.db` + `conversations.db` (SQLite, WAL mode, via `agtrs-store`)
- `XaftRuntime` has a `conversation_store: Option<Arc<dyn ConversationStore>>` field
- `RunRequest` has a `resume_session_id: Option<String>` field
- The CLI already accepts `--session/-s SESSION_ID`

The missing pieces are wiring and surfacing: the conversation store is never initialized in the default `bootstrap()` path, so the `Option` is always `None` at runtime. Nothing is ever saved. Nothing is ever loaded on resume.

---

## Goals

1. **Every conversation is persisted automatically** — no flags required. Messages (user, assistant, tool calls) are appended to `conversations.db` as they occur.
2. **Resume is a single flag.** `xaft --resume <SESSION_ID>` restores the full message history into agent context before the new task starts.
3. **Exit summary tells the user how to resume.** The final screen clearly shows the session ID and the exact command to use.
4. **Multi-turn TUI memory works.** When the user submits a second task in the same TUI session, the agent sees the prior exchanges as context.
5. **Session list command.** `xaft sessions` prints recent sessions with status, timestamp, and task summary.

## Non-Goals

- Semantic search over past conversations.
- Syncing sessions between machines.
- Pruning or summarizing long histories (deferred to a future memory feature).
- Changing the agent prompt structure — history injects as additional `Message` entries, not as a system prompt rewrite.

---

## Architecture

### What already exists

```
xaft-session/
  SessionManager
    └─ sessions.db      (AgentSession rows via SqliteSessionStore)
    └─ conversations.db (Message rows via SqliteConversationStore from agtrs-store)

XaftRuntime {
    conversation_store: Option<Arc<dyn ConversationStore>>,  ← always None today
}

RunRequest {
    resume_session_id: Option<String>,  ← passed in but never acted on
}

agtrs-store SqliteConversationStore
  create_conversation_with_id(id, metadata)
  append_message(id, message)   ← incremental, no full-save
  get_conversation(id)          → all messages ordered by ordinal
  list_conversations(opts)      → ConversationSummary[]
```

### Conversation key convention

`xaft-session` maps `session_id → conversation_id` as a 1:1 identity:

```rust
fn conversation_key_for(session_id: &SessionId) -> String {
    session_id.as_str().to_string()
}
```

The `ConversationId` stored in `agtrs-store` equals the session UUID. No lookup table needed.

### Metadata stored per conversation

```json
{
  "task":        "fix the type error in src/lib.rs",
  "working_dir": "/home/user/project",
  "agent":       "coder",
  "xaft_version": "0.1.0"
}
```

---

## Changes Required

### 1. Initialize the conversation store in `XaftRuntime::bootstrap()`

**File:** `crates/xaft-runtime/src/runtime.rs`

Currently `conversation_store` is set to `None` after bootstrap. Wire in the `SqliteConversationStore` from `xaft-session`'s `SessionManager`:

```rust
// In XaftRuntime::bootstrap()
let session_manager = SessionManager::new(&data_dir).await?;

let runtime = Self {
    config,
    signals,
    session_store: session_manager.session_store(),
    conversation_store: Some(session_manager.conversation_store()),  // ← was None
    ..
};
```

`SessionManager::conversation_store()` returns `Arc<dyn ConversationStore>` backed by `SqliteConversationStore`. This is the same `agtrs-store` type that `xaft-session` already uses for `load_with_history()`.

---

### 2. Create conversation record at session start

**File:** `crates/xaft-runtime/src/runtime.rs`, inside `run()`

When a new `AgentSession` is created (before the orchestrator is called), create the corresponding conversation row:

```rust
// After session is created, before orchestrator runs
if let Some(ref store) = self.conversation_store {
    let conv_id = ConversationId::from_string(session.id.as_str());
    let mut metadata = Metadata::new();
    metadata.insert("task".into(), request.task.clone().into());
    metadata.insert("working_dir".into(), request.working_dir.display().to_string().into());
    // Idempotent: INSERT OR IGNORE semantics via create_conversation_with_id
    store.create_conversation_with_id(conv_id, Some(metadata)).await
        .map_err(|e| RuntimeError::Session(e.to_string()))?;
}
```

`create_conversation_with_id` uses `INSERT OR REPLACE` (SQLite), so re-running on the same ID (e.g. a resume) is safe.

---

### 3. Pass conversation store into orchestrator and agents

**File:** `crates/xaft-runtime/src/orchestrator.rs`

The orchestrator already accepts `conversation_store: Option<Arc<dyn ConversationStore>>`. This value is then passed to each `AgentContext` via `AgentContextBuilder`. When `Some`, the `AgentContext` appends each message exchange to the store automatically (this is existing `agtrs-runtime` behavior).

The only change here is ensuring the `Some(store)` actually makes it through — it currently does not because `bootstrap()` never sets it.

---

### 4. Load history on resume

**File:** `crates/xaft-runtime/src/runtime.rs`, inside `run()`

When `RunRequest.resume_session_id` is `Some(id)`:

```rust
if let Some(ref resume_id) = request.resume_session_id {
    // 1. Load prior messages
    let session_id = SessionId::from(resume_id.as_str());
    let history = self.session_manager
        .load_with_history(&session_id)
        .await
        .map_err(|e| RuntimeError::Session(format!("resume failed: {e}")))?;

    // 2. Validate session is resumable (Active or Suspended)
    self.session_manager
        .validate_resumable(&session_id)
        .await
        .map_err(|e| RuntimeError::Session(e.to_string()))?;

    // 3. Inject prior messages into the agent context seed
    request.prior_messages = history.messages;  // ← new field on RunRequest
    request.session_id_override = Some(session_id);
}
```

`prior_messages` is a new optional field on `RunRequest`:

```rust
pub struct RunRequest {
    pub task: String,
    pub config: XaftConfig,
    pub working_dir: PathBuf,
    pub headless: bool,
    pub dry_run: bool,
    pub auto_approve: bool,
    pub dangerously_skip_permissions: bool,
    pub resume_session_id: Option<String>,
    pub workflow: WorkflowConfig,
    // ── New fields ──────────────────────
    /// Messages loaded from a prior session for context injection.
    pub prior_messages: Vec<agtrs_runtime::transport::Message>,
    /// Force-use this session ID (for resume). None = generate new UUID.
    pub session_id_override: Option<xaft_session::SessionId>,
}
```

When the orchestrator receives `prior_messages`, it seeds the planner agent's context with those messages before running the new task. The agent sees: prior exchange → new user task → continues.

---

### 5. TUI multi-turn memory

**File:** `crates/xaft-tui/src/app.rs`

Currently when the user submits a second task in the same TUI session (`user_msg_rx.try_recv()`), a fresh `RunRequest` is built with no prior context. Wire in the accumulated conversation:

```rust
if let Ok(user_task) = user_msg_rx.try_recv() {
    state.task = user_task.clone();
    state.reset_for_new_task();

    let _ = task_tx.send(RunRequest {
        task: user_task,
        config: self.config.clone(),
        working_dir: working_dir.clone(),
        resume_session_id: state.session.as_ref().map(|s| s.id.to_string()),  // ← resume within TUI
        prior_messages: vec![],  // runtime loads from store by resume_session_id
        ..
    });
}
```

By setting `resume_session_id` to the current session's ID, the runtime loads the existing conversation history from `conversations.db` before starting the next planner turn. The user's second message gets full context from the first exchange.

---

### 6. Exit summary — show resume command

**File:** `crates/xaft-tui/src/surface.rs`

Extend `render_exit_summary` to show the resume command prominently when a session ID is available:

```rust
pub fn render_exit_summary(
    elapsed: std::time::Duration,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    session_id: Option<&str>,
) {
    let mut out = io::stdout();
    let elapsed_str = format_elapsed(elapsed);
    let in_tok  = format_tokens_compact(input_tokens);
    let out_tok = format_tokens_compact(output_tokens);

    let _ = writeln!(out, "────────────────────────────────────────────────");
    let _ = writeln!(out, "  ✻ Worked for {elapsed_str}");
    let _ = writeln!(out, "  Tokens: {in_tok} in / {out_tok} out  ·  Cost: ${cost_usd:.4}");
    if let Some(sid) = session_id {
        let _ = writeln!(out, "  Session: {sid}");
        let _ = writeln!(out, "  Resume:  xaft --resume {sid}");  // ← NEW
    }
    let _ = writeln!(out, "────────────────────────────────────────────────");
    let _ = out.flush();
}
```

The `Resume:` line only appears when a session ID exists (i.e. the conversation was actually saved). For dry-run or headless runs with no persistence, it is omitted.

---

### 7. `--resume` CLI flag

**File:** `crates/xaft-cli/src/args.rs`

The `--session/-s` flag already exists. Rename or alias it to `--resume` for discoverability, keeping `--session` as a hidden alias for backward compatibility:

```rust
/// Resume a previous conversation by session ID.
/// The agent will load prior context before starting the new task.
#[arg(long = "resume", short = 'r', value_name = "SESSION_ID")]
pub resume: Option<String>,

/// Alias for --resume (deprecated)
#[arg(long = "session", short = 's', value_name = "SESSION_ID", hide = true)]
pub session: Option<String>,
```

In command dispatch, use `args.resume.or(args.session)` so both flags work.

---

### 8. `xaft sessions` command

New top-level command listing recent conversations:

```
xaft sessions [--limit N] [--all]
```

**Output format:**
```
  ID                                    STARTED          TURNS  STATUS    TASK
  a1b2c3d4-e5f6-7890-abcd-ef1234567890  2h ago           12     done      fix the type error in src/lib.rs
  f0e9d8c7-b6a5-4321-fedc-ba9876543210  yesterday        3      done      add tests for the parser
  88776655-4433-2211-0099-887766554433  3 days ago       0      error     refactor the auth module
```

**Implementation:**

New command variant in `xaft-cli/src/commands/`:

```rust
// crates/xaft-cli/src/commands/sessions.rs

pub async fn run(args: &SessionsArgs, config: XaftConfig) -> Result<(), CliError> {
    let data_dir = config.data_dir();
    let manager = SessionManager::new(&data_dir).await?;
    let sessions = manager.list_sessions(ListOptions {
        limit: args.limit.unwrap_or(20),
        offset: 0,
        ..Default::default()
    }).await?;

    if sessions.is_empty() {
        println!("  No sessions found.");
        return Ok(());
    }

    // Header
    println!("  {:<38} {:<16} {:<6} {:<9} TASK",
             "ID", "STARTED", "TURNS", "STATUS");
    println!("  {}", "─".repeat(90));

    for s in sessions {
        let age = format_age(s.created_at);
        let task_preview: String = s.task.chars().take(50).collect();
        let task_display = if s.task.len() > 50 {
            format!("{}…", task_preview)
        } else {
            task_preview
        };
        println!("  {:<38} {:<16} {:<6} {:<9} {}",
                 s.id, age, s.turns, s.status.label(), task_display);
    }
    Ok(())
}
```

---

## Data Flow (complete)

### New conversation (no resume)

```
xaft "fix the bug"
  │
  ├─ XaftRuntime::bootstrap()
  │    └─ SessionManager::new(data_dir)
  │         ├─ sessions.db      (SqliteSessionStore)
  │         └─ conversations.db (SqliteConversationStore)  ← NOW INITIALIZED
  │
  ├─ XaftRuntime::run(RunRequest { resume_session_id: None, .. })
  │    ├─ session = AgentSession::new(uuid)
  │    ├─ conv_store.create_conversation_with_id(session.id, metadata)
  │    └─ orchestrator::run_workflow(task, .., Some(conv_store))
  │         └─ AgentContext { conversation_store: Some(..) }
  │              └─ on each LLM turn: conv_store.append_message(session_id, msg)
  │
  └─ Exit: render_exit_summary(.., session_id: Some("a1b2c3d4-..."))
       "  Session: a1b2c3d4-..."
       "  Resume:  xaft --resume a1b2c3d4-..."
```

### Resume

```
xaft --resume a1b2c3d4-e5f6-7890-abcd-ef1234567890 "now add docstrings"
  │
  ├─ RunRequest { resume_session_id: Some("a1b2c3d4-..."), task: "now add docstrings" }
  │
  ├─ XaftRuntime::run()
  │    ├─ session_manager.load_with_history("a1b2c3d4-...")
  │    │    └─ returns SessionWithHistory { messages: [prior 12 turns] }
  │    ├─ session_manager.validate_resumable(..) → Ok
  │    └─ orchestrator::run_workflow(.., prior_messages: [12 turns], ..)
  │         └─ AgentContext seeded with prior messages
  │              └─ planner sees: [full prior context] + "now add docstrings"
  │
  └─ Exit: same session ID shown (session row updated, not recreated)
```

### Multi-turn TUI (second message)

```
User: "fix the bug"        → RunRequest { resume_session_id: None }
Agent: [runs, saves 8 turns]
User: "now write tests"    → RunRequest { resume_session_id: Some(current_session_id) }
                                ↑ runtime loads 8 turns, agent continues with full context
```

---

## Configuration

No new user-facing config keys are needed. The conversation store uses the same `data_dir` as sessions:

```toml
# ~/.config/xaft/xaft.toml
[session]
data_dir = "~/.local/share/xaft"   # existing field — conversations.db lives here
```

Optional per-project override in `.xaft.toml`:
```toml
[session]
data_dir = ".xaft"   # project-local sessions and conversations
```

---

## Testing Plan

### Unit tests (no I/O)

**`xaft-runtime/tests/resume.rs`:**
- `run_request_with_resume_id_loads_history` — mock `SessionManager`, assert prior messages injected
- `run_request_without_resume_id_starts_fresh` — assert empty prior messages
- `invalid_resume_id_returns_error` — assert `RuntimeError::Session`

**`xaft-tui/tests/`:**
- `second_task_in_tui_session_uses_resume_id` — send two tasks via `user_msg_rx`, assert second `RunRequest.resume_session_id` equals session ID from first task's completion signal

### Integration tests (`xaft-session`)

**`crates/xaft-session/tests/conversation_persistence.rs`:**
```rust
#[tokio::test]
async fn messages_persisted_across_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path()).await.unwrap();

    // Simulate first task
    let session = AgentSession::new("task one");
    mgr.save(&session).await.unwrap();
    let msg = Message::user("hello");
    mgr.append_message(&session.id, &msg).await.unwrap();

    // Load back
    let history = mgr.load_with_history(&session.id).await.unwrap();
    assert_eq!(history.messages.len(), 1);
    assert_eq!(/* content */, "hello");
}

#[tokio::test]
async fn resume_loads_prior_messages_in_order() { ... }

#[tokio::test]
async fn multi_task_tui_session_accumulates_messages() { ... }
```

### End-to-end tests (mock LLM)

**`crates/xaft-runtime/tests/e2e_resume.rs`:**
- Uses `MockLlmProvider` (already exists in `agtrs-runtime`)
- First run: 3-turn conversation, verify messages written to `conversations.db`
- Second run with `--resume`: verify prior 3 turns appear in agent context, verify new turns appended
- Verify exit summary contains session ID and resume command

---

## Acceptance Criteria

1. After `xaft "first task"` completes, `sessions.db` contains a session row and `conversations.db` contains all agent turns.
2. `xaft --resume <SESSION_ID> "second task"` starts with all messages from the first task visible to the planner agent.
3. Sending a second task in the TUI (without closing it) gives the agent full context from the first task.
4. The exit summary always shows:
   ```
     Session: <uuid>
     Resume:  xaft --resume <uuid>
   ```
   when a session was created.
5. `xaft sessions` prints a list of up to 20 recent sessions with ID, age, turn count, status, and task preview.
6. `xaft --resume <bad-id>` prints a clear error: `session not found: <bad-id>`.
7. Zero changes to the existing `agtrs-store` or `xaft-session` public APIs — this is purely wiring.

---

## Open Questions

1. **Context window management for long resumes.** A conversation with 50+ turns may exceed the model's context window. Should the resume path silently truncate to the last N tokens, or should it error and ask the user to start fresh? The `ShortTermMemory` type in `agtrs-runtime` already implements `trim_to_fit()` — wire that here for long histories.

2. **Session ownership and multi-user.** Should sessions be namespaced by a `user_id` field for multi-user environments, or is the file-system-level isolation (each user has their own `data_dir`) sufficient for now?

3. **`--resume` without a task string.** `xaft --resume <id>` with no positional task arg — should this re-run the same task, open the TUI in interactive mode with prior context loaded, or return an error?
