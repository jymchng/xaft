# PRD 45 — `/model` Command: Runtime Model Override

## Status: Draft
## Depends on: PRD 32a–32e, PRD 22 (costed provider)

---

## 1. Problem

`ModelHandler` stores the chosen model name in an `Arc<RwLock<String>>` and
returns `"Model switched to: {name}"`, but that string is never wired to the
running `HandoffOrchestrator` or `LlmProvider`. The override has no effect.

Additionally, `/model` with no arg should list available models; today it
returns an error.

---

## 2. Goals

1. `/model <name>` — switch the model used for all subsequent agent calls in
   the active session. The change takes effect on the next `LlmProvider::complete`
   call, without restarting the session.
2. `/model` (no arg) — list available models for the configured provider
   with a marker on the currently active one.
3. The active model name is shown in the session status line (ephemeral bar).
4. `XaftModelOverrideSet` signal is emitted so other subscribers (cost
   tracker, status bar) can react.

---

## 3. Non-Goals

- Per-agent model overrides (session-wide only in v1).
- Switching providers mid-session (model name must be valid for the current
  provider).
- Caching model lists for offline use.

---

## 4. Technical Design

### 4.1 Signal: `XaftModelOverrideSet`

```rust
#[derive(Clone, Debug)]
pub struct XaftModelOverrideSet {
    pub model: String,
    pub session_id: Option<Uuid>,
}
```

`ModelHandler::execute` emits this signal via `CommandContext::signals`
instead of (or in addition to) mutating the local `Arc<RwLock<String>>`.

### 4.2 Runtime subscription

`XaftRuntime` (or the orchestrator) subscribes to `XaftModelOverrideSet`.
On receipt it updates a session-scoped `Arc<RwLock<Option<String>>>` 
called `model_override`. `LlmProvider::complete` checks `model_override`
before using the config-specified model:

```rust
let model = self.model_override.read().unwrap()
    .as_deref()
    .unwrap_or(&self.config_model)
    .to_string();
```

### 4.3 No-arg list path

`ModelHandler::is_async() = true`. No-arg execution:

```rust
let models = self.provider.list_models().await?;
```

`LlmProvider` gains a `list_models() → Result<Vec<ModelInfo>>` method.
`AnthropicProvider` implements it by calling `GET /v1/models`.

`ModelInfo`:
```rust
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_window: usize,
}
```

Output table:
```
MODEL                            CONTEXT   ACTIVE
───────────────────────────────────────────────────
claude-opus-4-8                  200 000   
claude-sonnet-4-6                200 000   ✓
claude-haiku-4-5-20251001         32 000
```

### 4.4 Model validation

Before emitting `XaftModelOverrideSet`, validate the model name:
- Check it matches `^[a-z][a-z0-9._-]{3,80}$`.
- Optionally call `list_models()` and verify the name is in the list
  (async path only; skip in offline mode).
- Return `CommandResult::Error` if invalid.

### 4.5 Ephemeral bar update

`AppState::ephemeral` reads the current model from `AppState::model_override`
(new field, `Option<String>`). If set, the ephemeral status chip shows:
`model: claude-sonnet-4-6` in dim cyan.

---

## 5. Implementation Steps

1. Define `XaftModelOverrideSet` in `xaft-agent/src/signals.rs`.
2. Add `list_models()` to `LlmProvider` trait; implement for
   `AnthropicProvider`.
3. Add `model_override: Arc<RwLock<Option<String>>>` to runtime/orchestrator.
4. Subscribe to `XaftModelOverrideSet` in runtime; update override.
5. Pass override into each `complete()` call.
6. Update `ModelHandler`: validate → emit signal + no-arg list path.
7. Add `model_override: Option<String>` to `AppState`; subscribe to signal.
8. Update ephemeral bar to show current model override.
9. Tests (see §6).

---

## 6. Tests

```
test model_handler_empty_arg_lists_models
test model_handler_valid_name_emits_signal
test model_handler_invalid_name_returns_error
test runtime_model_override_used_in_next_completion
test runtime_model_override_resets_on_session_end
test ephemeral_bar_shows_model_override
test list_models_returns_known_anthropic_models
```
