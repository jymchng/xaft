# PRD 32c — Slash Command System: Registry & Handler Lifecycle

## Status: Draft
## Depends on: PRD 32a, PRD 32b

---

## 1. Scope

This document specifies `SlashCommandRegistry`, the handler-trait contract,
async command lifecycle, the spinner mechanism, signal emission, and how the
registry is assembled at startup.

---

## 2. Registry (`slash/registry.rs`)

### 2.1 Type

```rust
pub struct SlashCommandRegistry {
    handlers: HashMap<std::mem::Discriminant<SlashCommand>, Arc<dyn SlashHandler>>,
}
```

The key is the discriminant of the `SlashCommand` enum variant — not a string —
so the compiler ensures every variant has exactly one handler registered.

### 2.2 Builder

```rust
impl SlashCommandRegistry {
    pub fn builder() -> SlashCommandRegistryBuilder { … }
}

pub struct SlashCommandRegistryBuilder {
    handlers: HashMap<…>,
}

impl SlashCommandRegistryBuilder {
    pub fn register<H>(mut self, example: SlashCommand, handler: H) -> Self
    where
        H: SlashHandler + 'static,
    {
        self.handlers.insert(
            std::mem::discriminant(&example),
            Arc::new(handler),
        );
        self
    }

    pub fn build(self) -> SlashCommandRegistry {
        // Check every SlashCommand variant has a handler (debug_assert in tests)
        SlashCommandRegistry { handlers: self.handlers }
    }
}
```

### 2.3 Lookup

```rust
impl SlashCommandRegistry {
    pub fn get(&self, cmd: &SlashCommand) -> Arc<dyn SlashHandler> {
        self.handlers
            .get(&std::mem::discriminant(cmd))
            .expect("every SlashCommand variant must have a registered handler")
            .clone()
    }
}
```

`panic!` on missing handler is intentional — this is a programming error
detectable at startup or in tests, not a runtime user error.

---

## 3. Handler Trait

```rust
/// Implemented by every slash command handler.
#[async_trait]
pub trait SlashHandler: Send + Sync {
    fn description(&self) -> &'static str;
    fn args_hint(&self) -> Option<&'static str> { None }
    fn is_async(&self) -> bool { false }
    fn execute(&self, ctx: CommandContext) -> CommandResult { … }
    async fn execute_async(&self, ctx: CommandContext) -> CommandResult { … }
}
```

Rules:
- If `is_async()` returns `false`, only `execute` is called.
- If `is_async()` returns `true`, only `execute_async` is called.
- Handlers MUST NOT block the async executor in `execute`.  Any I/O goes in
  `execute_async`.
- Handlers must not panic — return `CommandResult::Error` instead.

---

## 4. `CommandContext`

```rust
pub struct CommandContext {
    /// Raw args string (everything after the trigger word, trimmed).
    pub args: String,

    /// Parsed `SlashCommand` (for handlers that need to inspect their own args).
    pub command: SlashCommand,

    /// Application-wide signal bus.
    pub signals: Arc<SignalBus>,

    /// Current resolved config (read-only snapshot).
    pub config: Arc<XaftConfig>,

    /// Active session ID, if any.
    pub session_id: Option<String>,

    /// Working directory for the current session.
    pub working_dir: PathBuf,

    /// Terminal width in columns (for table/line-wrapping).
    pub terminal_cols: u16,

    /// Accumulated per-agent LLM stats (for /cost).
    pub llm_stats: Arc<RwLock<AgentStatsMap>>,

    /// Access to the conversation store (for /resume, /rewind, /memory).
    pub conversation_store: Option<Arc<dyn ConversationStore>>,

    /// Access to the session store (for /resume listing).
    pub session_store: Option<Arc<dyn SessionStore>>,
}
```

`CommandContext` is constructed by `AppState::build_command_context()` and is
cheap to clone (all heavy fields are `Arc`).

---

## 5. `CommandResult` Processing

```rust
impl AppState {
    fn apply_command_result(&mut self, result: CommandResult, cmd_name: &str) {
        match result {
            CommandResult::Lines(lines) => {
                for line in lines {
                    self.mutations.push(RenderMutation::CommitLine(
                        StyledLine::new(format!("  {line}"), LineKind::System),
                    ));
                }
            }
            CommandResult::StyledLines(styled) => {
                for sl in styled {
                    self.mutations.push(RenderMutation::CommitLine(sl));
                }
            }
            CommandResult::Handled => { /* mutations pushed directly by handler */ }
            CommandResult::Error(msg) => {
                self.mutations.push(RenderMutation::CommitLine(
                    StyledLine::new(format!("    ✗ {msg}"), LineKind::Error),
                ));
            }
        }
        // Always emit the execution signal (even on error, with success=false)
        // The signal emission is done by the caller (execute_slash_command).
    }
}
```

---

## 6. Async Command Lifecycle

### 6.1 Spawning

```rust
fn spawn_command_future(
    &mut self,
    fut: BoxFuture<'static, CommandResult>,
    cmd_name: &str,
) {
    let cmd_name = cmd_name.to_string();
    let result_tx = self.internal_event_tx.clone();

    let handle = tokio::spawn(async move {
        let result = fut.await;
        let _ = result_tx.send(InternalEvent::SlashCommandDone {
            cmd_name,
            result,
        });
    });

    self.pending_slash_command = Some(PendingSlashCommand {
        name: cmd_name.to_string(),
        handle,
    });
}
```

### 6.2 Spinner

While `pending_slash_command.is_some()`, the ephemeral refresh loop renders:

```
  ⣷ /compact running…
```

The spinner character cycles through `⣷ ⣯ ⣟ ⣻ ⢿ ⡿ ⣿ ⣽` at ~100 ms.

### 6.3 Completion

On `InternalEvent::SlashCommandDone { cmd_name, result }`:
1. Clear `pending_slash_command`.
2. Call `apply_command_result(result, &cmd_name)`.
3. Emit `SlashCommandExecuted` signal.

### 6.4 Cancellation

If the user presses `Ctrl+C` while a slash command is running:
- The `tokio::task::JoinHandle` is aborted.
- An `Error("Command cancelled.")` result is applied.

---

## 7. Signal Emission

```rust
// In execute_slash_command, after result is applied:
let started = Instant::now();
// ... handler runs ...
let duration_ms = started.elapsed().as_secs_f64() * 1000.0;

let _ = ctx.signals.emit(SlashCommandExecuted {
    name: cmd_name.to_string(),
    args: ctx.args.clone(),
    success: !matches!(result, CommandResult::Error(_)),
    duration_ms,
}).await;
```

---

## 8. Registry Assembly (`app.rs`)

At `TuiApp::run_inner` startup, before entering the event loop:

```rust
let registry = SlashCommandRegistry::builder()
    .register(SlashCommand::Help,        HelpHandler::new(&registry_for_help))
    .register(SlashCommand::Clear,       ClearHandler)
    .register(SlashCommand::Compact,     CompactHandler::new(Arc::clone(&runtime_handle)))
    .register(SlashCommand::Config,      ConfigHandler::new(Arc::clone(&config)))
    .register(SlashCommand::Cost,        CostHandler)
    .register(SlashCommand::Init,        InitHandler::new(Arc::clone(&runtime_handle)))
    .register(SlashCommand::Agents,      AgentsHandler::new(Arc::clone(&agent_registry)))
    .register(SlashCommand::Mcp,         McpHandler::new(Arc::clone(&mcp_registry)))
    .register(SlashCommand::Resume { id: None }, ResumeHandler::new(
        Arc::clone(&session_store),
        Arc::clone(&signals),
    ))
    .register(SlashCommand::Rewind { msg_index: None }, RewindHandler::new(
        Arc::clone(&conversation_store),
    ))
    .register(SlashCommand::Permissions, PermissionsHandler::new(Arc::clone(&config)))
    .register(SlashCommand::Model { name: String::new() }, ModelHandler::new(
        Arc::clone(&runtime_handle),
    ))
    .register(SlashCommand::Vim,         VimHandler)
    .register(SlashCommand::Emacs,       EmacsHandler)
    .register(SlashCommand::Theme { name: None }, ThemeHandler::new(Arc::clone(&config)))
    .register(SlashCommand::Login,       LoginHandler)
    .register(SlashCommand::Logout,      LogoutHandler)
    .register(SlashCommand::Doctor,      DoctorHandler::new(Arc::clone(&config)))
    .register(SlashCommand::Memory,      MemoryHandler::new(Arc::clone(&memory_manager)))
    .register(SlashCommand::Diff,        DiffHandler)
    .register(SlashCommand::Commit,      CommitHandler)
    .register(SlashCommand::Pr { title: None }, PrHandler)
    .build();
```

The registry is placed on `AppState` as `Arc<SlashCommandRegistry>` so it is
accessible from event handlers without borrow-checker issues.

---

## 9. `AgentStatsMap`

The `/cost` command needs accumulated per-agent token stats.  These are built
from `LlmCallComplete` events which `AppState` already processes for the status
bar.  Extend the existing accumulation to a named map:

```rust
pub struct AgentStats {
    pub input_tokens:  u64,
    pub output_tokens: u64,
    pub cache_read:    u64,
    pub cache_write:   u64,
    pub cost_usd:      f64,
    pub calls:         u32,
}

pub type AgentStatsMap = IndexMap<String, AgentStats>;
```

`IndexMap` preserves insertion order (first-seen = first-printed in `/cost`).

---

## 10. Rendering Helper: Table Builder

Many commands produce tabular output.  A lightweight `CliTable` type is provided
in `slash/table.rs`:

```rust
pub struct CliTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    col_widths: Vec<usize>,  // auto-computed
}

impl CliTable {
    pub fn new(headers: impl IntoIterator<Item = impl Into<String>>) -> Self;
    pub fn push_row(&mut self, row: impl IntoIterator<Item = impl Into<String>>);
    pub fn render(&self, cols: u16) -> Vec<StyledLine>;
}
```

`render` returns one `StyledLine` per row plus one separator line under the
header.  Column widths are computed as `max(header.len(), max_value.len()) + 2`.
Rows are truncated with `…` when they exceed `cols`.

---

## 11. Startup Validation

In debug builds and in tests, `SlashCommandRegistry::build()` walks all
`SlashCommand` variants (via a hand-maintained list in the same file) and
`debug_assert!` that every variant's discriminant has a handler.  This catches
the common mistake of adding a new `SlashCommand` variant without registering
its handler.

```rust
#[cfg(debug_assertions)]
fn validate(&self) {
    for variant in SlashCommand::all_discriminants() {
        assert!(
            self.handlers.contains_key(&variant),
            "SlashCommandRegistry: no handler registered for {:?}",
            variant
        );
    }
}
```

`SlashCommand::all_discriminants()` is generated by a `macro_rules!` or by
hand in the same enum file, returning `Vec<Discriminant<SlashCommand>>`.
