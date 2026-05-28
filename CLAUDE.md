# xaft — CLAUDE.md

xaft is a Rust-native autonomous coding agent runtime. It plans, edits, verifies, and commits code changes via a multi-agent pipeline, with a Ratatui TUI, SQLite session persistence, and a `SignalBus` event system.

---

## Workspace layout

```
xaft (binary)
 ├── src/main.rs              — entry point: ConfigLoader → XaftRuntime → xaft_cli::run
 ├── crates/xaft-cli          — clap arg parsing, command dispatch, tracing init
 ├── crates/xaft-config       — XaftConfig, loader, hot-reload, validation
 ├── crates/xaft-runtime      — XaftRuntime, orchestrator, session store, providers
 ├── crates/xaft-agent        — XaftAgent, lifecycle hooks, signals, stream sinks
 ├── crates/xaft-tools        — file/git/shell tool implementations
 ├── crates/xaft-tui          — Ratatui TUI: conversation, approval, dashboard
 └── crates/xaft-session      — SQLite-backed session + conversation persistence
```

Dependencies flow strictly downward. `xaft-runtime` does NOT depend on `xaft-tui`; the TUI is wired in by the binary layer via `XaftRuntime::with_approval_gate`.

---

## Agent workflow

Standard pipeline (default):

```
Planner → (coding task) → Coder → QA ↔ Fixer
```

All four agents run inside one `HandoffOrchestrator`. Planner can answer info tasks inline (no handoff). Coder calls `handoff_to_agent("qa")`. QA calls `request_fix` (→ Fixer) or outputs `APPROVED`. Max 14 handoffs. See `crates/xaft-runtime/src/orchestrator.rs`.

Dynamic workflow: `WorkflowConfig::Dynamic { initial_agent, max_handoffs, agent_subset }` — any agent in `AgentRegistry` can hand off to any other.

---

## Key types

| Type | Crate | Purpose |
|------|-------|---------|
| `XaftRuntime` | xaft-runtime | Top-level runtime; `bootstrap()` → `run(RunRequest)` |
| `RunRequest` | xaft-runtime | Task + working_dir + flags |
| `XaftConfig` | xaft-config | Root config struct, six-layer precedence |
| `XaftAgent` | xaft-agent | Production agent implementing agtrs `Agent` trait |
| `HandoffOrchestrator` | agtrs-runtime | Coordinates multi-agent handoffs |
| `SignalBus` | agtrs-runtime | Type-safe broadcast event bus |
| `ToolRegistry` / `ToolRegistryBuilder` | xaft-tools | Builds per-role tool sets |
| `SessionManager` | xaft-session | SQLite session + conversation stores |
| `AgentSession` | xaft-runtime | Persisted session record |

---

## Build & test

```bash
cargo build --release -p xaft          # binary
cargo test --workspace                 # all crates
cargo test -p xaft-runtime             # single crate
RUST_LOG=xaft=debug xaft run "task"   # verbose run
cargo fmt -- --check
cargo clippy --workspace
```

Rust ≥ 1.86, edition 2024.

---

## Configuration

Six-layer precedence (highest → lowest): CLI flags → env vars (`XAFT_*`) → session config → project `.xaft.toml` → user `~/.config/xaft/xaft.toml` → built-in defaults.

Minimal config:
```toml
[provider.anthropic]
type        = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[agent.default]
provider  = "anthropic"
model     = "claude-3-5-sonnet-20241022"
max_turns = 25
```

`XaftConfig` is deep-merged with `null` preservation. Every new config key needs a `Default` impl — missing defaults break existing config files.

---

## Naming conventions

| Thing | Convention | Example |
|-------|-----------|---------|
| Crate | `xaft-<domain>` kebab-case | `xaft-git-ops` |
| Tool struct | `<Verb><Noun>Tool` | `WriteFileTool` |
| Tool name (LLM) | `snake_case` | `write_file` |
| Agent struct | `<Role>Agent` | `PlannerAgent` |
| Provider struct | `<Vendor>Provider` | `AnthropicProvider` |
| Signal | `Xaft<VerbNoun>` | `XaftCommitCreated` |
| Config keys (TOML) | `snake_case` | `cost_limit_config` |
| Capability trait | `<Adjective>able` | `Interruptable` |
| Role trait | noun | `Provider`, `Dispatch` |

---

## Error handling

- Library crates: `thiserror` typed error enum named `<Crate>Error`. **Never `anyhow` in library code.**
- `AgtrsError`: named variant per crate error (no catch-all `Internal` variant).
- `RuntimeError`: each variant maps to a distinct exit code.
- Soft errors (tool failed, agent can retry): `ToolResult { is_error: true }`.
- Hard errors (cancel, corrupt session): `Err(...)` propagated up.
- After every `.await` on a tool/agent call, check `.is_cancelled()` before proceeding.

---

## Testing conventions

- Unit tests: `#[cfg(test)] mod tests` in the same file.
- Integration tests: `crates/<name>/tests/`.
- Filesystem tests: `tempfile::TempDir` per test. Never write to project dir or `/tmp`.
- LLM tests: use `XaftRuntime::for_testing(config, Some(mock_llm))` or `MockLlmProvider`. No real API calls in automated tests.
- Async tests: `#[tokio::test]`. Never `#[test]` on async fn.
- `InMemorySessionStore` / `InMemoryConversationStore` for store tests.

---

## Safety invariants (never break)

1. Path traversal: all file tools reject paths escaping `working_dir`.
2. Git isolation: agent edits happen in a `xaft/session-<uuid>` worktree branch; HEAD is untouched.
3. Approval gate: tools with `requires_confirmation() = true` must block on `ApprovalGate` before executing. `bash_exec` always requires confirmation.
4. Cost accuracy: every `ModelCallComplete` signal updates the session's `total_cost_usd`. Never skip signal subscription.
5. Config null semantics: deep-merge preserves explicit `null`. Don't use `Option::unwrap_or_default` on merged config fields.

---

## Adding things

**New tool**: implement `Tool` in xaft-tools, add to `ToolRegistryBuilder`, write unit + integration tests including path traversal test.

**New agent**: use `AgentBuilder` / `NamedAgent`, define system prompt, assign minimal tool set, test lifecycle with mock LLM.

**New provider**: implement `LlmProvider`, add `ProviderType` variant, handle in `ProviderFactory::build()`, add config section with defaults.

**New signal**: name `Xaft<EventName>`, emit via `SignalBus::emit()`, subscribe in TUI event loop, bridge to a widget.

**New workflow**: register agents in `AgentRegistry`, configure `WorkflowConfig`, test handoff context transfer.

---

## Key files for common tasks

| Task | File |
|------|------|
| Runtime boot sequence | `crates/xaft-runtime/src/runtime.rs` |
| Agent workflow / orchestration | `crates/xaft-runtime/src/orchestrator.rs` |
| Agent lifecycle hooks | `crates/xaft-agent/src/agent.rs` |
| Config types | `crates/xaft-config/src/types.rs` |
| Config loading / merge | `crates/xaft-config/src/loader.rs`, `merge.rs` |
| Tool implementations | `crates/xaft-tools/src/fs/`, `git/`, `shell/` |
| TUI app loop | `crates/xaft-tui/src/app.rs` |
| Signal definitions | `crates/xaft-agent/src/signals.rs` |
| Session persistence | `crates/xaft-session/src/manager.rs` |
| CLI commands | `crates/xaft-cli/src/commands/` |
