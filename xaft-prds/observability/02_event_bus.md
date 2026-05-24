# Event Bus Architecture

## Signal Bus vs Message Bus

Two complementary event systems:

| | SignalBus | AgentMessageBus |
|---|---|---|
| Pattern | Publish/subscribe (broadcast) | P2P + broadcast |
| Direction | One-to-many | One-to-one or one-to-many |
| Purpose | Observability, TUI, metrics | Agent coordination |
| Type safety | Per-type subscriptions | Typed message envelopes |
| Persistence | None (in-memory only) | None (in-memory only) |
| Backpressure | Lagged receiver drops | Lagged receiver drops |

## Event Sourcing Readiness

While v1 does not implement event sourcing, all signals are designed to be serializable and replayable. The audit log (`xaft_audit.jsonl`) is effectively an event log.

```
Future EventLog (v2):
  SqliteEventLog implements EventLog
    ├── append(BoxedSignal) → sequence_number
    ├── replay_since(seq: u64) → Stream<BoxedSignal>
    └── snapshot_at(task_id) → SessionSnapshot

This enables:
  - Full session replay for debugging
  - Deterministic re-execution for testing
  - Cross-session analysis
  - Incident post-mortem
```

## TUI Event Pipeline Details

```
SignalBus (broadcast channels, one per signal type)
    │
    ├─ Subscription: ModelCallComplete
    │      │ map to UiEvent::ModelDone
    │      └─ mpsc::Sender<UiEvent> [capacity: 1024]
    │
    ├─ Subscription: ToolCallStarted
    │      │ map to UiEvent::ToolStart
    │      └─ same sender
    │
    ├─ Subscription: FileWritten
    │      │ map to UiEvent::FileChanged
    │      └─ same sender
    │
    └─ Subscription: ApprovalRequested
           │ map to UiEvent::ShowApproval
           └─ same sender

mpsc::Receiver<UiEvent>
    │
    ▼ (consumed on each TUI tick)
AppState mutation
    │
    ▼
terminal.draw(render_fn)
```

## Signal Delivery Guarantees

- Sync handlers (`bus.on::<T>(fn)`): Called synchronously, inline, in the emitter's task. Must complete quickly.
- Async subscribers (`bus.subscribe::<T>()`): Buffered (capacity 256). If buffer full: oldest event dropped. Subscriber receives `RecvError::Lagged(n)`.
- No ordering guarantee between different signal types.
- Same-type signals are ordered per emitter.

## References

- agtrs: `agtrs-runtime/src/signals.rs`
- agtrs: `agtrs-runtime/src/messaging.rs`
