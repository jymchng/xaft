cat > ./01_conversation_memory.md << 'EOF'
# Conversation & Memory Model

## Three Memory Layers

| Layer | Scope | Duration | Implementation |
|---|---|---|---|
| Short-term | Per agent run | Single turn sequence | `ShortTermMemory` |
| Conversation | Per session | Session lifetime | `ConversationStore` |
| Long-term user | Per user | Persistent | `UserMemoryStore` |

## Short-Term Memory (Sliding Window)

`ShortTermMemory` maintains a rolling token-bounded message history.

```rust
// CodeAgent::on_start
async fn on_start(&self, ctx: &mut AgentContext) -> Result<(), AgtrsError> {
    let mut mem = ShortTermMemory::new(40_000);

    // Load prior conversation if resuming
    if let Some(conv_id) = ctx.context_state().get("conversation_id") {
        let conv_id = conv_id.as_str().unwrap_or("");
        let history = self.conversation_store.load(conv_id).await?;
        for msg in history { mem.add_message(msg); }
    }

    ctx.set_memory(Box::new(mem));
    Ok(())
}
```

## Summarization

When context usage exceeds `summarize_at` threshold (default 80%):

```rust
// Triggered automatically by AgentExecutor
async fn summarize_if_needed(ctx: &mut AgentContext, llm: &dyn LlmProvider) {
    let mem = ctx.short_term_memory();
    let usage = mem.token_estimate() as f64 / ctx.config().memory_window_tokens as f64;

    if usage > ctx.config().summarize_at.unwrap_or(0.8) {
        let to_summarize = mem.messages_to_summarize(20_000);

        let summary_agent = SummaryAgent::new(llm);
        let summary = summary_agent.summarize(&to_summarize).await?;

        mem.trim_oldest(to_summarize.len());
        mem.set_summary(summary);
    }
}
```

## Conversation Persistence

```rust
// CodeAgent::on_complete
async fn on_complete(&self, ctx: &mut AgentContext, _: &AgentResponse) -> Result<(), AgtrsError> {
    if let Some(conv_id) = ctx.context_state().get("conversation_id") {
        let conv_id = conv_id.as_str().unwrap_or("default");
        self.conversation_store.save(conv_id, ctx.messages()).await?;
    }
    Ok(())
}
```

Backends:
- **Development**: `InMemoryConversationStore`
- **Production**: `SqliteConversationStore` (wraps `agtrs-store/sqlite`)

## Project Memory (Persistent)

`xaft` maintains project-level memory across sessions:

```rust
pub struct ProjectMemory {
    store: Arc<SqliteStore>,
}

impl ProjectMemory {
    /// Remember a fact about the project
    pub async fn remember(&self, fact: &str, category: &str) -> Result<(), XaftError>;

    /// Recall relevant facts for current task
    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryResult>, XaftError>;

    /// Prune stale memories (older than N days, low confidence)
    pub async fn prune(&self, max_age_days: u64) -> Result<usize, XaftError>;
}
```

Example auto-stored facts:
- "Auth module is in `src/auth/`. Last modified by xaft on 2026-01-10."
- "Cargo workspace uses edition 2024, MSRV 1.86."
- "Tests run with `just test`, not `cargo test` directly."

## References

- agtrs: `agtrs-runtime/src/memory.rs`
- agtrs guide: `guides/10-memory.md`
EOF

cat > ./02_context_window.md << 'EOF'
# Context Window Management

## Problem

A `CodeAgent` working on a large refactor may accumulate 60,000+ tokens of conversation history across 20 turns. LLM context windows are finite and expensive. `xaft` manages context proactively.

## Three-Tier Strategy

```
Tier 1: Sliding window (40K tokens)
    Keep recent messages in full fidelity.
    Drop oldest when window fills.

Tier 2: Summarization (at 80% capacity)
    Run SummaryAgent on oldest 20K tokens.
    Replace with 500-token summary.
    Retain system prompt + recent messages.

Tier 3: Selective context injection
    Use semantic index to inject only relevant context.
    Instead of: "here is the whole codebase"
    Use: "here are the 5 most relevant files for this step"
```

## Context Budget Allocation

```
Total context window: 200,000 tokens (claude-3-5-sonnet)
────────────────────────────────────────────────
System prompt:         2,000 tokens (static)
Workspace context:     3,000 tokens (dynamic per step)
Tool schemas:          2,000 tokens (tool count dependent)
Conversation history: 40,000 tokens (sliding window)
Tool results:         remaining (LLM responses are max_tokens)
────────────────────────────────────────────────
Reserved for response: 4,096 tokens (max_tokens_per_turn)
```

## Workspace Context Injection

Before each agent run, inject only relevant workspace context:

```rust
pub async fn build_workspace_context(
    step: &PlanStep,
    index: &RepoIndex,
    workspace: &WorkspaceEditor,
    token_budget: usize,
) -> Result<String, XaftError> {
    // Search index for relevant files
    let relevant = index.search(&step.description, 5).await?;

    let mut context = String::new();
    let mut tokens_used = 0;

    context.push_str("## Relevant Files\n\n");

    for result in &relevant {
        let content = workspace.read(&result.path).await?;
        let file_tokens = estimate_tokens(&content);

        if tokens_used + file_tokens > token_budget { break; }

        context.push_str(&format!("### {}\n```rust\n{content}\n```\n\n", result.path.display()));
        tokens_used += file_tokens;
    }

    Ok(context)
}
```

## Token Estimation

```rust
pub fn estimate_tokens(text: &str) -> usize {
    // Rough estimate: 1 token ≈ 4 characters for English/code
    // Use tiktoken-rs for precise counting when available
    text.len() / 4
}
```

## References

- agtrs: `agtrs-runtime/src/memory.rs` (ShortTermMemory)
- agtrs guide: `guides/10-memory.md`
EOF

cat > ./03_persistence_layer.md << 'EOF'
# Persistence Layer

## Storage Architecture

```
.xaft/sessions/{session_id}.db    ← SQLite per session
.xaft/index/symbols.db            ← symbol index
.xaft/index/content.db            ← content index
.xaft/audit/{date}.jsonl          ← append-only audit log
~/.config/xaft/memory.db          ← user-level long-term memory
```

## SQLite Session Store

```rust
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn save_session(&self, session: &SessionSnapshot) -> Result<(), XaftError>;
    async fn load_session(&self, session_id: Uuid) -> Result<Option<SessionSnapshot>, XaftError>;
    async fn save_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), XaftError>;
    async fn load_checkpoint(&self, task_id: Uuid) -> Result<Option<Checkpoint>, XaftError>;
    async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, XaftError>;
}
```

## Store Backends (from agtrs-store)

| Backend | Use case | Implementation |
|---|---|---|
| `MemoryStore` | Testing, ephemeral | HashMap in Arc<RwLock<>> |
| `FsStore` | Simple local persistence | JSON files per key |
| `SqliteStore` | Production session data | SQLite via sqlx |

## Session Snapshot

```rust
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: Uuid,
    pub session_state: SessionState,
    pub intent: Intent,
    pub plan: Option<Plan>,
    pub current_task_id: Option<Uuid>,
    pub worktree_branch: Option<String>,
    pub total_cost_usd: f64,
    pub total_tokens: usize,
    pub started_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}
```

## References

- agtrs: `agtrs-store/src/`
- agtrs tests: `agtrs-store/tests/store_integration.rs`
EOF

cat > ./04_session_recovery.md << 'EOF'
# Session Recovery

## Recovery Scenarios

| Scenario | Detection | Recovery action |
|---|---|---|
| Process crash | `xaft resume` command | Load checkpoint, recreate worktree, continue from last step |
| Ctrl-C | Signal handler | Save checkpoint, clean suspend |
| Budget exceeded | `BudgetExceeded` error | Save checkpoint, report remaining steps |
| Step failure (recoverable) | Test failure | Run FixerAgent, retry step |
| Step failure (unrecoverable) | Max retries exceeded | Save checkpoint, await user |
| Provider timeout | `LlmCallFailed` | Retry with exponential backoff (max 3 attempts) |

## Resume Command

```bash
$ xaft resume ses-abc123

Resuming session ses-abc123...
  Intent: "migrate auth to JWT"
  Last checkpoint: Step 3/7 (Edit src/auth.rs)
  Worktree: xaft-wt-abc123 (branch xaft/abc123)
  Modified files: src/auth.rs (staged)
  Cost so far: $0.042

Continue from step 3? [Y/n]
```

## Recovery Implementation

```rust
pub async fn resume_session(session_id: Uuid, config: &XaftConfig) -> Result<(), XaftError> {
    // 1. Load session snapshot
    let store = SqliteSessionStore::open(&config.session_db_path).await?;
    let snapshot = store.load_session(session_id).await?
        .ok_or_else(|| XaftError::Session(format!("session {session_id} not found")))?;

    // 2. Verify worktree still exists
    let repo = GitRepo::open(&config.project_root)?;
    let worktree_exists = repo.worktree_exists(&snapshot.worktree_branch.as_deref().unwrap_or("")).await;

    if !worktree_exists {
        // Worktree was removed — recreate from base
        tracing::info!("recreating worktree from base");
        let wt = repo.create_worktree_from_branch(
            &snapshot.worktree_branch.as_deref().unwrap_or("main"),
            "main",
        ).await?;
        // Re-apply committed changes from worktree branch
        repo.checkout_branch(&snapshot.worktree_branch.unwrap()).await?;
    }

    // 3. Load checkpoint
    let checkpoint = store.load_checkpoint(snapshot.current_task_id.unwrap()).await?
        .ok_or_else(|| XaftError::Session("no checkpoint found".into()))?;

    // 4. Reconstruct session
    let session = XaftSession::from_snapshot(snapshot, config).await?;

    // 5. Resume execution from checkpoint
    let plan_executor = PlanExecutor::new(Arc::clone(&session));
    plan_executor.resume_from_checkpoint(checkpoint).await?;

    Ok(())
}
```

## Exponential Backoff for LLM Retries

```rust
pub async fn llm_call_with_retry<F, T>(
    f: F,
    max_retries: u32,
    cancel_token: &CancellationToken,
) -> Result<T, AgtrsError>
where
    F: Fn() -> BoxFuture<'static, Result<T, AgtrsError>>,
{
    let mut attempt = 0;
    loop {
        tokio::select! {
            result = f() => {
                match result {
                    Ok(v) => return Ok(v),
                    Err(AgtrsError::LlmCallFailed(_)) if attempt < max_retries => {
                        attempt += 1;
                        let delay = Duration::from_millis(1000 * 2u64.pow(attempt));
                        tracing::warn!("LLM call failed, retrying in {}ms (attempt {}/{})", delay.as_millis(), attempt, max_retries);
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => return Err(e),
                }
            }
            _ = cancel_token.cancelled() => {
                return Err(AgtrsError::Cancelled { reason: "cancelled during retry".into() });
            }
        }
    }
}
```

## References

- agtrs: `agtrs-runtime/src/task.rs` (TaskState::Suspended, TaskRunner::resume)
- agtrs: `agtrs-store/src/backends/sqlite.rs`
EOF

echo "Memory docs done"