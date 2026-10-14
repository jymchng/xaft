# [Done] PRD 33d — Background Agent Execution (Ctrl+B)

## Status: Draft
## Depends on: PRD 09, PRD 10, PRD 33a (/btw sidecar)

---

## 1. Problem

xaft's TUI is fully modal during agent execution: the main event loop
processes agent signals and renders output, but the user is locked out of
the input bar (except for sending Ctrl+C to cancel). There is no way to
start a second task or explore the codebase while the agent works.

From the Claude Code output pasted by the user:

```
(ctrl+b to run in background)
```

Claude Code allows the user to detach a running agent to background mode,
freeing the TUI for a new prompt. The background agent continues running,
and its output is collected and surfaced when it finishes (or when the user
re-attaches).

---

## 2. Goals

1. `Ctrl+B` while an agent pipeline is running detaches it to background.
2. A background pipeline:
   - Continues running and processing normally.
   - Emits signals to the `SignalBus` as usual (signals are recorded).
   - Does NOT write to the TUI transcript while detached.
3. After `Ctrl+B`, the TUI input bar becomes active and the user can
   start a new task or use slash commands.
4. When a background pipeline finishes (or errors), the TUI shows an
   inline notification: `[bg done] <task summary>  · N s`.
5. Pressing `Ctrl+B` again (when no foreground pipeline is running)
   **re-attaches** the most recently detached background pipeline,
   replaying buffered output and resuming live streaming.
6. At most **2 concurrent background pipelines** (configurable). A third
   `Ctrl+B` press is rejected with a status-bar message
   `"Max background tasks reached (2)"`.
7. If multiple background pipelines exist, `Ctrl+B` shows a numbered
   selection list; the user presses a digit to choose which to re-attach.

---

## 3. Non-Goals

- Persistent background tasks that survive TUI exit (those are for
  `xaft run --detach` headless mode, a separate feature).
- Scheduling background tasks in advance.
- Background tasks that can acquire the approval gate (blocked tools
  will hang until re-attached — documented behavior).
- Parallel output interleaving (background output only replays on
  re-attach or at completion, never inline with the foreground task).

---

## 4. Architecture

### 4.1 `PipelineHandle` — foreground pipeline wrapper

```rust
/// Owns the running pipeline task and its signal subscriptions.
pub struct PipelineHandle {
    pub id:            u64,
    pub task_summary:  String,      // first ~60 chars of the user prompt
    pub started_at:    Instant,
    pub task:          JoinHandle<RunResult>,
    /// Channel sender — the running pipeline reads from the receiver side
    /// to accept control messages (e.g., detach, cancel).
    pub ctrl_tx:       mpsc::Sender<PipelineControl>,
}

pub enum PipelineControl {
    Detach,
    Cancel,
}
```

### 4.2 `BackgroundPipeline` — detached pipeline record

```rust
pub struct BackgroundPipeline {
    pub id:              u64,
    pub task_summary:    String,
    pub started_at:      Instant,
    pub task:            JoinHandle<RunResult>,
    pub ctrl_tx:         mpsc::Sender<PipelineControl>,
    /// Accumulated `RenderMutation`s produced while detached.
    pub buffered:        Vec<RenderMutation>,
    pub status:          BackgroundPipelineStatus,
}

pub enum BackgroundPipelineStatus {
    Running,
    Completed(RunResult),
    Failed(String),
}
```

**Buffer size cap**: `buffered` is capped at `MAX_BUFFERED_MUTATIONS =
2000`. If the pipeline produces more, older mutations are dropped with a
`CommitLine("[bg] … earlier output truncated …")` sentinel. This prevents
memory blowout on long-running background tasks.

### 4.3 `AppState` changes

```rust
pub struct AppState {
    // ... existing fields ...
    /// The currently active foreground pipeline, if any.
    pub foreground_pipeline:   Option<PipelineHandle>,
    /// Detached background pipelines (ordered oldest first).
    pub background_pipelines:  Vec<BackgroundPipeline>,
}
```

### 4.4 Signal buffering mechanism

When a pipeline is in background, its signal bridge channel still drains
into the `AppState` event loop (it has no way not to — it's the same
channel). The difference is in how `AppState::handle_event` processes
events: a **pipeline ID tag** on each `TuiEvent` lets `AppState` route
events to the correct pipeline's buffer.

Every `TuiEvent` variant gains an optional `pipeline_id: Option<u64>`:

```rust
pub enum TuiEvent {
    AgentChunk        { pipeline_id: u64, text: String, .. },
    ToolStarted       { pipeline_id: u64, .. },
    ToolFinished      { pipeline_id: u64, .. },
    AgentFinished     { pipeline_id: u64, result: RunResult },
    // … etc.
    // Events not tied to a pipeline (key presses, terminal resize) have no id.
    Key(crossterm::event::KeyEvent),
    Resize(u16, u16),
    BtwAnswer { .. },
    BackgroundPipelineComplete { id: u64, result: RunResult },
}
```

In `AppState::handle_event`:

```rust
TuiEvent::AgentChunk { pipeline_id, .. } => {
    if Some(pipeline_id) == self.foreground_pipeline.as_ref().map(|p| p.id) {
        // Route to transcript normally.
        self.mutations.push(RenderMutation::CommitLine(…));
    } else if let Some(bg) = self.background_pipelines
                                .iter_mut()
                                .find(|p| p.id == pipeline_id) {
        // Buffer the mutation.
        if bg.buffered.len() < MAX_BUFFERED_MUTATIONS {
            bg.buffered.push(RenderMutation::CommitLine(…));
        } else {
            // Mark truncation once.
            if !bg.truncated {
                bg.truncated = true;
                bg.buffered.push(RenderMutation::CommitLine(
                    StyledLine::new("[bg] earlier output truncated", LineKind::System)
                ));
            }
        }
    }
    // Events from an unknown pipeline_id are discarded (pipeline already gone).
}
```

### 4.5 `Ctrl+B` — detach to background

```rust
KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
    // Check capacity.
    if self.background_pipelines.len() >= self.config.tui.max_background_tasks {
        self.push_status_message(format!(
            "Max background tasks reached ({})",
            self.config.tui.max_background_tasks
        ));
        return InputAction::NoOp;
    }
    if let Some(handle) = self.foreground_pipeline.take() {
        // Signal the pipeline to switch its output routing to buffer mode.
        let _ = handle.ctrl_tx.try_send(PipelineControl::Detach);
        self.background_pipelines.push(BackgroundPipeline {
            id:           handle.id,
            task_summary: handle.task_summary.clone(),
            started_at:   handle.started_at,
            task:         handle.task,
            ctrl_tx:      handle.ctrl_tx,
            buffered:     Vec::new(),
            status:       BackgroundPipelineStatus::Running,
            truncated:    false,
        });
        // Unlock input bar and reset phase.
        self.workflow_phase = WorkflowPhase::Idle;
        self.mutations.push(RenderMutation::CommitLine(
            StyledLine::new(
                format!("Agent moved to background  ·  Ctrl+B to re-attach  ·  {} running",
                    self.background_pipelines.len()),
                LineKind::System,
            )
        ));
        self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
    }
    // If no foreground pipeline, fall through to re-attach logic below.
    InputAction::NoOp
}
```

### 4.6 `Ctrl+B` — re-attach

When `Ctrl+B` is pressed and `foreground_pipeline.is_none()`:

```rust
match self.background_pipelines.len() {
    0 => self.push_status_message("No background pipelines."),
    1 => self.reattach_pipeline(0),
    n => {
        // Show selection list.
        let lines: Vec<_> = self.background_pipelines.iter().enumerate()
            .map(|(i, p)| format!("  [{}]  {}  ·  {}",
                i + 1,
                &p.task_summary,
                format_elapsed(p.started_at.elapsed())))
            .collect();
        self.push_status_message(
            format!("Background pipelines — press 1-{n} to re-attach:\n{}", lines.join("\n"))
        );
        self.awaiting_bg_select = true;
    }
}
```

`reattach_pipeline(idx)`:
1. Remove from `background_pipelines[idx]`.
2. Promote to `foreground_pipeline`.
3. Drain `bg.buffered` → apply all mutations in order.
4. Switch `workflow_phase` back to `Active`.
5. Signal the pipeline via `ctrl_tx` to resume foreground routing.

### 4.7 Completion notification

`TuiApp::run` polls background pipeline `JoinHandle`s at 10fps:

```rust
for bg in &mut self.background_pipelines {
    if bg.status == BackgroundPipelineStatus::Running {
        if let Ok(result) = bg.task.try_join() {
            bg.status = BackgroundPipelineStatus::Completed(result.clone());
            event_tx.send(TuiEvent::BackgroundPipelineComplete {
                id: bg.id,
                result,
            }).ok();
        }
    }
}
```

`AppState::handle_event(TuiEvent::BackgroundPipelineComplete)`:
```rust
let bg = self.background_pipelines.iter().find(|p| p.id == id);
let summary = bg.map(|p| p.task_summary.as_str()).unwrap_or("unknown task");
let elapsed = bg.map(|p| format_elapsed(p.started_at.elapsed())).unwrap_or_default();
let ok = matches!(result, RunResult::Success(_));
self.mutations.push(RenderMutation::CommitLine(
    StyledLine::new(
        format!("[bg {}]  {}  ·  {}",
            if ok { "done" } else { "failed" },
            summary, elapsed),
        if ok { LineKind::System } else { LineKind::Error },
    )
));
// Remove from background list.
self.background_pipelines.retain(|p| p.id != id);
```

### 4.8 Approval gate interaction

A background pipeline that calls a tool requiring approval sends
`request_approval` which blocks on the `ApprovalQueue` channel. The
`ApprovalQueue` is shared with the foreground, so the request will sit
indefinitely until the user re-attaches. This is correct and documented.

However, to avoid a confusing user experience, the `TuiApprovalGate`
should check if the requesting pipeline is backgrounded and, if so,
immediately push an indicator message to the transcript:
```
[bg] Task paused — waiting for approval. Press Ctrl+B to re-attach.
```

### 4.9 TUI exit with background pipelines

When the user presses `q` / `Esc`-confirm with running background
pipelines, the escape confirmation dialog (already implemented) gains
an extra line:
```
  Warning: 2 background pipelines are still running and will be cancelled.
  Press Enter to quit anyway, Esc to return.
```

On confirm, all background `JoinHandle`s are aborted.

### 4.10 `/btw` (PRD 33a) interaction

`/btw` uses a separate `BtwQueue` with its own worker task. It is
unaffected by `Ctrl+B`. The `/agents` slash command (PRD 37) lists both
background pipelines and active `/btw` sidecars in separate sections.

---

## 5. `XaftConfig` knob

```toml
[tui]
max_background_tasks = 2   # default: 2; max: 5
```

---

## 6. Signals

```rust
/// Emitted when a background pipeline finishes (success or failure).
#[derive(Debug, Clone)]
pub struct XaftBackgroundPipelineComplete {
    pub id:           u64,
    pub task_summary: String,
    pub success:      bool,
    pub duration_ms:  f64,
}

/// Emitted when a pipeline is detached to background.
#[derive(Debug, Clone)]
pub struct XaftPipelineDetached {
    pub id:           u64,
    pub task_summary: String,
}

/// Emitted when a background pipeline is re-attached to foreground.
#[derive(Debug, Clone)]
pub struct XaftPipelineReattached {
    pub id:           u64,
    pub buffered_mutations: usize,
}
```

---

## 7. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/state.rs` | `background_pipelines`, `foreground_pipeline`, `Ctrl+B` handler, event routing by `pipeline_id` |
| `crates/xaft-tui/src/app.rs` | `PipelineHandle` construction, background poll loop, quit guard |
| `crates/xaft-tui/src/bridge.rs` | Tag all `TuiEvent`s with `pipeline_id` |
| `crates/xaft-agent/src/signals.rs` | `XaftBackgroundPipelineComplete`, `XaftPipelineDetached`, `XaftPipelineReattached` |
| `crates/xaft-config/src/types.rs` | `TuiConfig::max_background_tasks` |
| `crates/xaft-tui/tests/background_pipeline_integration.rs` | New tests |

---

## 8. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | `Ctrl+B` while a pipeline is running detaches it and unlocks the input bar. |
| AC2 | The detached pipeline continues running (signals still processed, stored in `buffered`). |
| AC3 | `Ctrl+B` when one background pipeline exists re-attaches it, replaying all buffered mutations. |
| AC4 | `Ctrl+B` when multiple background pipelines exist shows a numbered selection list. |
| AC5 | When a background pipeline finishes, a `[bg done]` notification appears in the active transcript. |
| AC6 | `Ctrl+B` when `max_background_tasks` is reached shows "Max background tasks reached (N)". |
| AC7 | A background pipeline that requires tool approval prints a re-attach hint rather than silently hanging. |
| AC8 | Quitting with background pipelines shows a warning naming the count. |
| AC9 | Background pipeline mutations beyond `MAX_BUFFERED_MUTATIONS` are silently truncated (sentinel line added). |
| AC10 | `XaftBackgroundPipelineComplete` is emitted when a background pipeline finishes. |

---

## 9. Tests

```rust
#[tokio::test]
async fn ctrl_b_detaches_pipeline_and_unlocks_input() {
    // Assert workflow_phase → Idle after Ctrl+B.
    // Assert input bar accepts new characters.
}

#[tokio::test]
async fn detached_events_routed_to_buffer_not_transcript() {
    // Assert mutations from bg pipeline id go to bg.buffered, not AppState.
}

#[tokio::test]
async fn ctrl_b_reattaches_and_replays_buffered_output() {
    // Buffer 5 mutations, re-attach.
    // Assert all 5 applied to transcript in order.
}

#[tokio::test]
async fn background_pipeline_completion_emits_notification() {
    // Mock pipeline finishes.
    // Assert [bg done] line in transcript.
}

#[tokio::test]
async fn exceeding_max_background_tasks_rejected() {
    // Detach max_background_tasks pipelines.
    // Assert next Ctrl+B pushes status message, does not add to vec.
}

#[tokio::test]
async fn buffer_truncation_at_cap() {
    // Send MAX_BUFFERED_MUTATIONS + 10 events to background pipeline.
    // Assert buffer.len() == MAX_BUFFERED_MUTATIONS (no growth past cap).
    // Assert one truncation sentinel line present.
}

#[tokio::test]
async fn approval_request_from_background_prints_hint() {
    // Background pipeline requests approval.
    // Assert [bg] hint line in transcript.
}
```
