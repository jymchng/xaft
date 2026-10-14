# PRD 37 — `/agents` Command: Agent Registry Inspector

## Status: Draft
## Depends on: PRD 32a–32e, PRD 21 (xaft-agents crate)

---

## 1. Problem

`/agents` returns `"No agent registry connected."` because `CommandContext`
carries no reference to the `AgentRegistry`. The user has no way to inspect
which agents are loaded, what tools they have, or their current lifecycle
state without reading config files manually.

---

## 2. Goals

1. `/agents` prints a table of all registered agents: name, model, role,
   tool count, and current status (idle / active / error).
2. `/agents <name>` prints detailed info for one agent: system prompt
   excerpt (first 80 chars), full tool list, max_turns, last-run timestamp.
3. Column widths adapt to the terminal width (`CommandContext::terminal_cols`).

### Example output (no args)

```
NAME          MODEL                   ROLE     TOOLS  STATUS
────────────────────────────────────────────────────────────
planner       claude-3-5-sonnet       Planner      3  idle
coder         claude-3-5-sonnet       Coder        8  active
qa            claude-3-5-sonnet       QA           4  idle
fixer         claude-3-5-sonnet       Fixer        6  idle
```

---

## 3. Non-Goals

- Modifying agent configuration at runtime via `/agents`.
- Showing full system prompts (security: may contain sensitive context).
- Starting or stopping individual agents via this command.

---

## 4. Technical Design

### 4.1 `AgentSummary` in `CommandContext`

Add a snapshot type to `xaft-tui/src/slash/mod.rs`:

```rust
#[derive(Clone, Debug)]
pub struct AgentSummary {
    pub name: String,
    pub model: String,
    pub role: String,
    pub tool_names: Vec<String>,
    pub status: AgentStatus,          // reuse from agent_tracker
    pub last_run: Option<SystemTime>,
    pub system_prompt_excerpt: String, // first 80 chars
    pub max_turns: u32,
}
```

`CommandContext` gains:
```rust
pub agents: Vec<AgentSummary>,
```

### 4.2 Populating `AgentSummary` in `AppState`

`AppState` holds an `Arc<AgentTracker>` (already exists). `AgentTracker`
tracks status per agent name. The runtime populates it via `XaftAgentStarted`
/ `XaftAgentFinished` signals.

For the model + tool list: `CommandContext` is built from `AppState` just
before execution. `AppState` needs access to an `Arc<AgentRegistry>` (or a
snapshot). The runtime should pass an `AgentRegistrySnapshot` through the
`TuiApp` constructor (similar to how `Arc<XaftConfig>` is wired).

### 4.3 `AgentsHandler::execute`

No-arg path: render `CliTable::new(["NAME", "MODEL", "ROLE", "TOOLS",
"STATUS"])`. With `<name>` arg: find matching `AgentSummary` and return a
`StyledLines` detail block.

### 4.4 Detail view format

```
Agent: coder
  Model:       claude-3-5-sonnet-20241022
  Role:        Coder
  Max turns:   25
  Tools (8):   read_file, write_file, bash_exec, list_dir, …
  Status:      active
  Last run:    2026-06-09 14:32:05 UTC
  Prompt:      "You are the Coder agent. Your job is to…"
```

---

## 5. Implementation Steps

1. Define `AgentSummary` in `xaft-tui/src/slash/mod.rs`.
2. Add `agents: Vec<AgentSummary>` to `CommandContext`.
3. Wire `AgentRegistrySnapshot` through `TuiApp` → `AppState` →
   `build_command_context`.
4. Implement `AgentsHandler::execute` (table + detail paths).
5. Tests: `agents_handler_table_output`, `agents_handler_detail_known`,
   `agents_handler_detail_unknown`.

---

## 6. Tests

```
test agents_handler_empty_registry_prints_empty_table
test agents_handler_shows_all_registered_agents
test agents_handler_detail_known_name
test agents_handler_detail_unknown_name_returns_error
test agents_table_adapts_to_narrow_terminal
```
