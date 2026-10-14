# Claude-Code Parity Features PRD

## Background

xaft has built strong foundations toward Claude Code parity: PRD 9/10/11 delivered
the single-column borderless TUI, PRD 20/21 produced a 4-agent handoff system, PRD 22
added tiered provider routing, PRD 23 added parallel repo exploration, PRD 24/25 added
memory, and PRD 26 added conversation persistence.

However, several core Claude Code affordances are still missing. The TUI has an input
bar that pipes messages to the agent loop, but there is no slash-command system, no
plan-mode toggle, no `@`-mentions, no bash-mode escape, no multimodal input, and no
session-picker. The agent toolkit has read/write/edit/grep/explore tools, but no todo
list, no web fetch/search, no notebook edit, no background tasks, and no PR creation.
There is no MCP integration, no user hook system, no project subagent files, no
auto-compaction when context fills, and no session forking.

This PRD introduces 20 features that close those gaps. They are organised into four
groups: **Interactive UX** (F1–F8), **Agent Toolkit** (F9–F14), **Extensibility**
(F15–F18), and **Workflow Reliability** (F19–F20). Each feature is independent and
shippable; together they make xaft feel like Claude Code while keeping the
event-sourced, tool-mutates-state architecture mandated by `AGENTS.md`.

---

## Goal

Add the 20 features described below. Each feature:

- Has a small, focused surface (one PRD section).
- Integrates with existing systems: `SignalBus`, `WorkflowConfig`, `AgentRegistry`,
  `MemoryManager`, `ConversationStore`, `ToolRegistry`, `XaftConfig`.
- Emits a signal for every user-visible transition (per `AGENTS.md`).
- Comes with unit + integration tests in the same style as `tests/explore_pool.rs`.
- Can be enabled / disabled independently via `XaftConfig` (no all-or-nothing rollout).

The features target the codebase as of PRDs 22–26. None re-implement work already
done; they extend existing crates (`xaft-runtime`, `xaft-agents`, `xaft-tools`,
`xaft-tui`, `xaft-config`, `xaft-memory`) or add small new modules.

---

## High-Level Architecture

```text
┌──────────────────────────────────────────────────────────────────────────┐
│                            xaft-cli / xaft-tui                           │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌──────────┐ │
│ │ Slash Cmds │ │ Plan Mode  │ │ @-Mentions │ │ Bash Mode  │ │  Image   │ │  F1-F5
│ └────────────┘ └────────────┘ └────────────┘ └────────────┘ └──────────┘ │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐                            │
│ │ Resume UI  │ │ Status Bar │ │ Cost Panel │                            │  F6-F8
│ └────────────┘ └────────────┘ └────────────┘                            │
└──────────────────────────────┬───────────────────────────────────────────┘
                               │ TuiEvent
┌──────────────────────────────▼───────────────────────────────────────────┐
│                            xaft-runtime                                  │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌──────────┐ │
│ │ TodoWrite  │ │ WebFetch   │ │ NotebookEd │ │ BgBash     │ │ PRCreate │ │  F9-F13
│ └────────────┘ └────────────┘ └────────────┘ └────────────┘ └──────────┘ │
│ ┌────────────┐                                                       │
│ │ Hunk Diff  │                                                       │  F14
│ └────────────┘                                                       │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                          Extensibility Layer                             │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐            │
│ │    MCP     │ │   Hooks    │ │   Skills   │ │  Subagents │            │  F15-F18
│ └────────────┘ └────────────┘ └────────────┘ └────────────┘            │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                        Workflow Reliability                              │
│ ┌────────────┐ ┌────────────┐                                            │
│ │  Compact   │ │   Fork     │                                            │  F19-F20
│ └────────────┘ └────────────┘                                            │
└──────────────────────────────────────────────────────────────────────────┘
```

---

# Part 1 — Interactive UX

## [Almost Done] F1. Slash Command System

### Goal
Provide a Claude-Code-style slash command palette. Commands start with `/` and are
intercepted by the TUI input layer before the message reaches the agent loop.

### Command set (initial)

| Command | Description | Implementation |
|---|---|---|
| `/help` | List all commands | `commands/help.rs` |
| `/clear` | Reset current session transcript | signal `SessionReset` |
| `/compact` | Manually trigger context compaction (F19) | calls `Compactor::compact()` |
| `/config` | Open config editor (current value + edit) | runs `ConfigEditor` UI |
| `/cost` | Show per-agent token / cost table | reads `ModelCallComplete` history |
| `/init` | Generate `AGENTS.md` (Claude-Code style project memory) | calls LLM with project scan |
| `/agents` | List loaded agents (built-in + custom F18) | reads `AgentRegistry` |
| `/mcp` | Show MCP server status (F15) | reads `McpRegistry` |
| `/resume [id]` | Resume a prior session | `XaftRuntime::resume_session()` |
| `/rewind [msg]` | Roll back to message N, fork (F20) | calls `SessionForker::fork_to()` |
| `/permissions` | Open permission editor (per-tool rules) | `PermissionEditor` |
| `/model <name>` | Switch model mid-session | rebuilds `LlmProvider` |
| `/vim` / `/emacs` | Toggle input-bar keymap | swaps keybinding layer |
| `/theme <name>` | Cycle TUI theme | `TuiConfig::theme` |
| `/login` / `/logout` | OAuth / API key management | interactive prompt |
| `/doctor` | Run diagnostics (config, deps, connectivity) | `Doctor` toolset |
| `/memory` | Open memory browser | reads `MemoryManager::list` |
| `/diff` | Show working-tree diff against base commit | calls `GitDiffTool` |
| `/commit` | Commit current session changes (override policy) | `WorktreeGuard::commit` |
| `/pr [title]` | Open PR (F13) | calls `PrCreateTool` |

### Architecture

```text
input_buffer: String
  │ on Enter:
  ▼
SlashCommandParser::try_parse(&input) -> Option<SlashCommand>
  │
  ├── None  → forward to user_message_tx (existing path)
  └── Some  → execute via SlashCommandRegistry
                │
                ├── sync handlers (return String for TUI to print)
                ├── async handlers (return Future<CommandResult>)
                └── ui handlers (push a new state onto AppState)
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/slash/mod.rs` | **New** — `SlashCommand` enum, `SlashCommandRegistry` |
| `xaft-tui/src/slash/{help,clear,compact,cost,...}.rs` | One file per command |
| `xaft-tui/src/slash/parser.rs` | `try_parse(&str) -> Option<SlashCommand>` |
| `xaft-tui/src/state.rs` | Intercept `/` in input handler before forwarding |
| `xaft-tui/src/app.rs` | Register commands at startup |

### Tests

- `slash_parser_recognises_known_commands`
- `slash_parser_returns_none_for_plain_text`
- `slash_help_returns_command_list`
- `slash_clear_emits_session_reset_signal`
- `slash_cost_renders_per_agent_table`
- `slash_unknown_command_returns_error_message`

### Acceptance criteria

1. Typing `/` opens an inline autocomplete with all 20 commands.
2. `Tab` completes partial commands; `↑`/`↓` cycles history.
3. Unknown commands return `Unknown command: /foo. Type /help for list.` and do NOT
   reach the agent.
4. All commands emit a `SlashCommandExecuted { name, args }` signal.

---

## F2. Interactive Plan Mode

### Goal
A toggleable mode that requires the planner to produce a plan, present it to the user,
and wait for approval before any write tools may be invoked.

### Behaviour

1. User types `/plan` (or starts session with `--plan`) → `PlanMode::Active`.
2. All agents in the session are constructed with `plan_mode: true`.
3. `AgentExecutor` is wrapped: if an agent in plan-mode emits a tool call whose
   `requires_confirmation()` is true OR whose name is in the write-tool set, the
   executor returns a `PlanApprovalRequired { plan: String, tools_blocked: Vec<String> }`
   response and pauses.
4. TUI renders a plan card with `[a]pprove  [r]evise  [c]ancel`.
5. On approve, the blocked tool calls are re-invoked in a fresh executor run.
6. On revise, the planner is re-prompted with the user's feedback string.
7. On cancel, the session terminates with `ExitCode::CANCELLED`.

### Architecture

```text
PlanMode flag in XaftAgentConfig
  │
  ▼
PlanGateHook (injects into AgentContext)
  │
  ▼
Wraps write-tools (edit_file, write_file, bash_exec, …)
  │
  ▼
On first invocation → emit PlanApprovalRequired → await decision
  │
  ▼
Decision (Approve/Revise/Cancel) → continue or abort
```

### Files

| File | Change |
|---|---|
| `xaft-agent/src/plan_mode.rs` | Extend with `PlanGateHook` |
| `xaft-tui/src/state.rs` | Render plan card + handle keys |
| `xaft-cli/src/args.rs` | Add `--plan` / `--no-plan` flag |
| `xaft-config/src/types.rs` | Add `plan_mode_default: bool` |

### Tests

- `plan_mode_blocks_first_write_tool_invocation`
- `plan_mode_unblocks_after_user_approval`
- `plan_mode_revise_loops_planner`
- `plan_mode_cancel_terminates_session`

### Acceptance criteria

1. `xaft run --plan "Add pagination"` shows the plan before any edits.
2. `AppState` renders the plan in a `⎿  Plan` block with the three choices.
3. After approval, the session proceeds without re-prompting until next plan.

---

## [DONE] F3. @-Mention File Inputs

### Goal
Allow users to reference files inline: `@src/main.rs` or `@./README.md` should be
expanded into file-content attachments before the message reaches the LLM.

### Behaviour

1. Input parser scans for `@<path>` tokens.
2. Each token is resolved (relative to `working_dir`, rejected if `..` escapes root).
3. File content is read; if file > 2000 lines, only first 2000 + line count note.
4. Image files (`.png`, `.jpg`, `.gif`, `.webp`) are base64-embedded as `Image` content
   blocks (multimodal — see F5).
5. Path-not-found tokens are left as literal text but produce a warning line in TUI.
6. The original `@path` is preserved in the user message for traceability.

### Architecture

```text
User: "Fix the bug in @src/lib.rs and update @README.md"
                       │              │
                       ▼              ▼
              MentionResolver::expand(&msg, &workspace)
                       │
                       ▼
Message::user_with_parts(vec![
    Text("Fix the bug in "),
    FileRef{ path: "src/lib.rs", content: "..." },
    Text(" and update "),
    FileRef{ path: "README.md", content: "..." },
])
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/mention.rs` | **New** — `MentionResolver` |
| `xaft-runtime/src/dispatch.rs` | Apply resolver to `RunRequest::task` and follow-up messages |
| `xaft-tools/src/fs/read_file.rs` | Add a "head" mode (first N lines) |

### Tests

- `mention_resolver_expands_relative_path`
- `mention_resolver_rejects_path_traversal`
- `mention_resolver_truncates_large_files`
- `mention_resolver_preserves_unknown_tokens`
- `mention_resolver_embeds_images_as_base64`

### Acceptance criteria

1. `@<tab>` opens a file picker (powered by `list_files`).
2. Multiple `@` mentions in one message expand correctly.
3. Large files truncate with `... (truncated, 4232 more lines)`.

---

## F4. Bash Mode (`!` Prefix)

### Goal
Lines starting with `!` are executed directly in the host shell, bypassing the agent
loop entirely. Output is printed in the TUI as `! <command>` followed by stdout/stderr.

### Behaviour

1. Input parser detects `!<cmd>` prefix.
2. Command runs in the configured `Sandbox` (F-14-style policy).
3. Streaming output rendered line-by-line (`!` prefix on every output line).
4. Exit code shown as `! exit=0 (0.42s)`.
5. Non-zero exit / timeout / sandbox violation → render in error color, no agent call.
6. Commands still appear in the session transcript (with role `User` + role `Tool`).

### Architecture

```text
input "  !ls -la"
  │ trim
  ▼
BashModeHandler::handle(&cmd, &sandbox, &policy) -> BashOutput
  │
  ▼
CommandExecutor::execute(Bash{ command: cmd, use_shell: true }, cancel_token)
  │
  ▼
StreamCommandEvent → TUI ephemeral line
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/bash_mode.rs` | **New** — parser + handler |
| `xaft-tui/src/state.rs` | Intercept `!` in input handler |
| `agtrs-shell` | Already provides `Bash` + `CommandExecutor` (no changes) |

### Tests

- `bash_mode_runs_command_and_captures_stdout`
- `bash_mode_respects_sandbox_policy`
- `bash_mode_streams_long_output`
- `bash_mode_records_in_transcript`
- `bash_mode_kills_on_timeout`
- `bash_mode_blocks_rm_via_default_policy`

### Acceptance criteria

1. `!git status` runs and renders output without an LLM call.
2. Output streams line-by-line (no buffering).
3. Exit code 0 shows in `✓` color; non-zero in `✗` color.

---

## F5. Image / PDF Paste (Multimodal Input)

### Goal
Accept pasted or dropped images and PDFs in the TUI input bar; embed them as
multimodal content blocks for the LLM.

### Behaviour

1. TUI listens for bracketed paste (`\x1b[200~...~\x1b[201~`) and OSC 52 (`\x1b]52;c;...`).
2. Image bytes are detected via magic bytes (PNG, JPEG, GIF, WebP, PDF).
3. Stored in a per-session temp dir `~/.xaft/sessions/<id>/attachments/`.
4. Inlined into the next user message as `ContentBlock::Image { ... }` or
   `ContentBlock::Document { ... }`.
5. Anthropic provider already supports `Image` blocks; OpenAI similar.

### Files

| File | Change |
|---|---|
| `xaft-tui/src/paste.rs` | **New** — paste handler, format detection |
| `xaft-runtime/src/dispatch.rs` | Inject attachments into `Message::user_with_parts` |
| `xaft-tui/src/state.rs` | Surface "📎 1 image attached" hint |

### Tests

- `paste_detects_png_magic_bytes`
- `paste_writes_to_session_attachment_dir`
- `paste_constructs_image_content_block`
- `paste_handles_pdf_via_document_block`

### Acceptance criteria

1. Pasting a PNG into the input bar shows a thumbnail + "📎" hint.
2. The next user message includes the image as a multimodal block.
3. Provider capability check (F-12) gracefully degrades: if `supports_vision()=false`,
   fall back to saving the file and telling the LLM the path.

---

## F6. Resume Picker UI

### Goal
When `xaft` is launched with no task, show an interactive list of prior sessions and
let the user pick one to resume.

### Behaviour

1. `XaftRuntime::bootstrap` queries `SessionStore::list(working_dir)`.
2. Sessions rendered as: `[<short-id>] <task-prefix>  <date>  <cost>  <status>`.
3. Keys: `↑/↓` navigate, `Enter` resume, `d` delete, `Esc` exit picker.
4. Search filter via `/` (incremental).
5. Default selection: most recent session in current `working_dir`.

### Architecture

```text
Main loop:
  ├── if --task given → run task
  ├── elif --resume <id> → resume_session(id)
  └── else → SessionPicker::pick(&sessions) → resume_session(picked.id)
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/picker.rs` | **New** — picker widget |
| `xaft-cli/src/commands/run.rs` | Branch on no-task in interactive mode |

### Tests

- `picker_lists_sessions_in_reverse_chronological`
- `picker_filter_narrows_results`
- `picker_delete_removes_session_from_store`
- `picker_resume_calls_runtime_resume`

### Acceptance criteria

1. `xaft` (no args) shows last 20 sessions.
2. `Enter` resumes the highlighted session; transcript loads from
   `ConversationStore`.

---

## F7. Status Bar (Always-Visible)

### Goal
A single-line status bar at the bottom of the TUI showing the always-relevant
context: model, working dir, git branch, current agent, total cost, elapsed time.

### Layout

```text
[claude-sonnet-4-6] [/home/user/proj: main*]  coder ◉  3 turns  $0.124  00:01:42
```

### Behaviour

- Updates on every `LlmCallComplete`, `AgentHandoff`, `SessionUpdate`.
- Git status suffix: `*` = dirty, `+` = staged, ` ` = clean.
- Cost formatted via existing `format_tokens_compact`/`format_duration_ms`.
- `Ctrl+T` collapses the bar to a single line (saves a row for the prompt).

### Files

| File | Change |
|---|---|
| `xaft-tui/src/surface.rs` | Add `StatusBar` widget |
| `xaft-tui/src/state.rs` | Track `git_status_suffix`, `elapsed_secs` |

### Tests

- `status_bar_renders_all_fields`
- `status_bar_updates_on_handoff`
- `status_bar_collapses_via_ctrl_t`

### Acceptance criteria

1. Status bar present in both interactive and (read-only) headless modes.
2. Cost / token / elapsed values match `RunCostAccumulator`.

---

## F8. Cost / Budget Dashboard Panel

### Goal
A toggleable side panel (or full-screen with `Ctrl+B`) showing per-agent cost and
token usage as a table, with sparklines for the last 10 model calls.

### Layout

```text
┌─ Cost ────────────────────────────────────────────┐
│ Agent     Calls  In Tok   Out Tok  Cost USD       │
│ planner      1   12.3k     0.4k      $0.034       │
│ coder        4   44.1k     8.7k      $0.186       │
│ qa           1    3.2k     0.1k      $0.007       │
│ ─────────────────────────────────────────────────│
│ TOTAL        6   59.6k     9.2k      $0.227       │
│                                                  │
│ Budget:  $0.50 / $5.00  (4.5% used)              │
└──────────────────────────────────────────────────┘
```

### Behaviour

- `Ctrl+B` opens/closes.
- Live-updates on `ModelCallComplete`.
- `r` resets run cost (writes a checkpoint to session log).
- Sparkline: 10-cell row of token volume bars per agent.

### Files

| File | Change |
|---|---|
| `xaft-tui/src/dashboard.rs` | **New** — cost panel |
| `xaft-runtime/src/orchestrator.rs` | Add `RunCostAccumulator::per_agent_history()` |

### Tests

- `dashboard_aggregates_by_agent_name`
- `dashboard_sparkline_renders_10_cells`
- `dashboard_budget_progress_bar`

### Acceptance criteria

1. Cost numbers match those in `RunResult` (single source of truth).
2. Panel survives `AppState::handle_event` re-renders.

---

# Part 2 — Agent Toolkit

## F9. TodoWrite / TodoRead Tool

### Goal
A persistent, observable todo list that the planner and coder update as they work.
Mirrors Claude Code's `TodoWrite` / `TodoRead` and renders a `[TASKS]` block in TUI.

### Tool definition

```rust
pub struct TodoWriteTool;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TodoInput {
    pub items: Vec<TodoItem>,  // replaces entire list
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TodoItem {
    pub content: String,        // imperative: "Fix divide-by-zero in auth.py"
    pub status: TodoStatus,     // Pending | InProgress | Completed
    pub active_form: String,    // present-continuous: "Fixing divide-by-zero"
}

pub enum TodoStatus { Pending, InProgress, Completed }
```

### Behaviour

- `TodoWrite` replaces the list atomically (single source of truth).
- `TodoRead` returns the current list.
- On every `TodoWrite`, emit `TodosUpdated { items, agent_name }` signal.
- TUI subscribes and renders `[TASKS]` block at top of transcript.
- Planner / coder system prompts: "Use TodoWrite to track multi-step work. Update
  status as you progress."

### Files

| File | Change |
|---|---|
| `xaft-tools/src/todo/mod.rs` | **New** — `TodoWriteTool`, `TodoReadTool`, `TodoStore` |
| `xaft-agent/src/signals.rs` | Add `TodosUpdated` |
| `xaft-tui/src/state.rs` | Render `[TASKS]` block on `TodosUpdated` |
| `xaft-agents/src/{planner,coder,qa}.rs` | Update system prompts to mention `TodoWrite` |
| `xaft-runtime/src/orchestrator.rs` | Wire tools into agent toolset |

### Tests

- `todo_write_replaces_entire_list`
- `todo_read_returns_current_list`
- `todo_write_emits_signal`
- `todo_persists_across_agent_handoff`
- `todo_renders_as_task_block_in_tui`

### Acceptance criteria

1. Planner automatically calls `TodoWrite` at the start of any non-trivial task.
2. `[TASKS]` block updates live in TUI.
3. `TodoRead` from a later agent returns the same list (persistence).

---

## F10. WebFetch + WebSearch Tools

### Goal
Two tools that let agents reach the web for documentation, error lookups, and
research. Mirrors Claude Code's `WebFetch` and `WebSearch`.

### `WebFetchTool`

```rust
pub struct WebFetchTool {
    client: reqwest::Client,
    cache: Arc<MokaCache<Url, FetchedPage>>,
    max_bytes: usize,
}

#[derive(Deserialize, JsonSchema)]
pub struct WebFetchInput {
    pub url: String,
    pub prompt: String,  // "What does this page say about X?"
}
```

Returns: HTML → markdown via `html2md` crate, then LLM-summarised answer.

### `WebSearchTool`

```rust
pub struct WebSearchTool {
    provider: Arc<dyn SearchProvider>,
}

#[derive(Deserialize, JsonSchema)]
pub struct WebSearchInput {
    pub query: String,
    pub max_results: Option<u8>,  // default 5
}
```

`SearchProvider` trait allows pluggable backends (Brave, Serper, Tavily, custom).

### Behaviour

- Both tools require explicit user permission for each domain (`PermissionRules`).
- Results cached for 1h by URL.
- 30s timeout, 1 MiB max response.
- Domain allowlist config: `[tool].web.allowed_domains = ["docs.rs", "stackoverflow.com"]`.
- Emit `WebFetched { url, status, bytes }` / `WebSearched { query, results_count }` signals.

### Files

| File | Change |
|---|---|
| `xaft-tools/src/web/{fetch,search,provider}.rs` | **New** |
| `xaft-tools/src/web/cache.rs` | Moka-backed response cache |
| `xaft-config/src/types.rs` | Add `[tool].web` section |
| `xaft-agent/src/signals.rs` | Add `WebFetched`, `WebSearched` |

### Tests

- `web_fetch_converts_html_to_markdown`
- `web_fetch_caches_response`
- `web_search_returns_top_n_results`
- `web_fetch_respects_domain_allowlist`
- `web_fetch_blocks_disallowed_domain`

### Acceptance criteria

1. `WebFetch` and `WebSearch` appear in the agent's toolset when `[tool].web.enabled = true`.
2. Results stream into the transcript as `◆ WebFetch(...)` / `◆ WebSearch(...)`.

---

## F11. NotebookEdit Tool

### Goal
Cell-level edit / insert / delete for Jupyter notebooks (`.ipynb`). Mirrors Claude
Code's `NotebookEdit`.

### Tool definition

```rust
#[derive(Deserialize, JsonSchema)]
pub struct NotebookEditInput {
    pub notebook_path: String,
    pub cell_id: Option<String>,        // if None, insert at cell_type index
    pub cell_type: NewCellType,         // Markdown | Code
    pub source: String,                 // cell content
    pub edit_mode: EditMode,            // Replace | Insert | Delete
}
```

### Behaviour

- Parses notebook JSON via `nbformat` crate.
- Validates cell IDs and types.
- `Delete` mode: omit `source`.
- Atomic write: serialize full JSON, write to temp, rename.
- Emits `NotebookEdited { path, cell_id, edit_mode }` signal.

### Files

| File | Change |
|---|---|
| `xaft-tools/src/notebook/mod.rs` | **New** — `NotebookEditTool` |
| `xaft-tools/Cargo.toml` | Add `nbformat`, `serde_json` deps |

### Tests

- `notebook_edit_replaces_existing_cell`
- `notebook_edit_inserts_at_index`
- `notebook_edit_deletes_cell`
- `notebook_edit_rejects_invalid_notebook`
- `notebook_edit_preserves_outputs_on_markdown`

### Acceptance criteria

1. Agent can edit a notebook cell-by-cell without rewriting the whole file.
2. Cell outputs are preserved on `Replace` for `Code` cells (only `source` updated).

---

## F12. Background Bash Tasks

### Goal
Long-running commands (`npm run dev`, `cargo watch`, `python manage.py runserver`)
can be launched in the background, polled, and killed — without blocking the agent
loop.

### Tool definition

```rust
pub struct BashRunTool;     // starts task, returns task_id
pub struct BashPollTool;    // checks status / reads new output
pub struct BashKillTool;    // terminates task
pub struct BashListTool;    // lists all running tasks for session

#[derive(Deserialize, JsonSchema)]
pub struct BashRunInput {
    pub command: String,
    pub timeout_ms: Option<u32>,         // cap total runtime (default 1h)
    pub run_in_background: bool,         // explicit user opt-in
}
```

### Architecture

```text
BashRunTool
  │
  ▼
BackgroundTaskRegistry::spawn(id, command, sandbox)
  │
  ├── spawns CommandExecutor with cancellable CancellationToken
  ├── writes stdout/stderr to a ring buffer (last 10k lines)
  ├── emits BashTaskSpawned / BashTaskCompleted signals
  └── returns task_id
```

### Behaviour

- `BashPoll` returns new output since last poll (caller-supplied cursor).
- `BashKill` cancels the token; if the process doesn't exit in 5s, sends SIGKILL.
- Tasks auto-cleaned on session end.
- TUI surfaces running tasks as `[BG: 2 active]` in the status bar (F7).

### Files

| File | Change |
|---|---|
| `xaft-tools/src/bash/{run,poll,kill,list}.rs` | **New** |
| `xaft-tools/src/bash/registry.rs` | `BackgroundTaskRegistry` |
| `xaft-agent/src/signals.rs` | `BashTaskSpawned`, `BashTaskOutput`, `BashTaskCompleted` |
| `xaft-tui/src/state.rs` | Render `[BG: N active]` |

### Tests

- `bash_run_spawns_task_and_returns_id`
- `bash_poll_returns_new_output_since_cursor`
- `bash_kill_terminates_long_running_process`
- `bash_kill_sigkills_after_grace_period`
- `bash_list_filters_by_session`
- `background_task_survives_agent_handoff`

### Acceptance criteria

1. `BashRun(command="npm run dev", run_in_background=true)` returns within 200ms.
2. Agent can then `BashPoll` from a later turn to check progress.
3. Session-end cleans up all running tasks.

---

## F13. PR Creation Tool (GitHub CLI Integration)

### Goal
Create a GitHub Pull Request (or GitLab MR) from the session's branch via `gh` CLI.
Mirrors Claude Code's PR-creation flow.

### Tool definition

```rust
pub struct PrCreateTool;

#[derive(Deserialize, JsonSchema)]
pub struct PrCreateInput {
    pub title: String,
    pub body: Option<String>,
    pub base: Option<String>,           // default: main
    pub draft: Option<bool>,            // default: false
    pub reviewers: Option<Vec<String>>,
}
```

### Behaviour

1. Detect `gh` (or `glab` if `core.gitlab = true`).
2. Push branch if not yet pushed.
3. Run `gh pr create --title ... --body ... --base ...`.
4. Capture PR URL from stdout.
5. Emit `PrCreated { url, number, branch }` signal.
6. TUI renders `✓ PR opened: <url>`.

### Files

| File | Change |
|---|---|
| `xaft-tools/src/pr.rs` | **New** — `PrCreateTool` |
| `xaft-agent/src/signals.rs` | Add `PrCreated` |
| `xaft-cli/src/commands/run.rs` | Add `/pr` slash command (F1) |

### Tests

- `pr_create_runs_gh_with_correct_args`
- `pr_create_pushes_branch_first`
- `pr_create_captures_url_from_stdout`
- `pr_create_handles_missing_gh_binary`
- `pr_create_emits_signal_with_url`

### Acceptance criteria

1. `PrCreate` appears in coder's toolset.
2. Successful call returns PR URL; TUI prints it as a clickable link.
3. Configurable to use `glab` for GitLab.

---

## F14. Hunk-Level Diff Operations

### Goal
The agent can stage, unstage, and view individual hunks within a file (not just
whole-file diffs). Mirrors `git add -p` semantics exposed to the LLM.

### Tools

```rust
pub struct DiffHunksTool;     // list hunks in working-tree or staged
pub struct ApplyHunkTool;     // apply a single hunk from a generated patch
pub struct RejectHunkTool;    // discard a hunk from the working tree
```

### `DiffHunksInput`

```rust
pub struct DiffHunksInput {
    pub path: String,
    pub staged: bool,           // default false (working tree)
}
```

Returns:
```rust
pub struct HunkList {
    pub hunks: Vec<Hunk> {
        pub index: usize,                  // 0-based within file
        pub header: String,                // @@ -12,5 +12,7 @@
        pub added_lines: u32,
        pub removed_lines: u32,
        pub preview: String,               // first 3 lines of each side
    },
}
```

### `ApplyHunkInput`

```rust
pub struct ApplyHunkInput {
    pub path: String,
    pub hunk_index: usize,
    pub direction: ApplyDirection,         // Take | Skip
    pub new_content: Option<String>,       // for Take: optional edit before apply
}
```

### Behaviour

- Built on `similar::TextDiff` (already in deps) for hunk generation.
- `ApplyHunk` uses `agtrs_workspace::FileEditor::apply_diff` for hunk-level atomic edits.
- `RejectHunk` reverts a specific hunk via `git checkout -p`.
- TUI shows hunk list inline with `[t]ake [s]kip` shortcuts.

### Files

| File | Change |
|---|---|
| `xaft-tools/src/diff/{hunks,apply,reject}.rs` | **New** |
| `xaft-tools/src/diff/hunk.rs` | Hunk model + serialization |
| `xaft-tui/src/state.rs` | Render hunk list with shortcuts |

### Tests

- `diff_hunks_lists_all_hunks_in_file`
- `apply_hunk_takes_selected_hunk`
- `apply_hunk_skips_hunk`
- `apply_hunk_with_new_content_edits_lines`
- `reject_hunk_reverts_to_base`

### Acceptance criteria

1. `DiffHunks` returns stable, ordered hunks matching `git diff` output.
2. `ApplyHunk` produces a single commit-ready change.

---

# Part 3 — Extensibility

## F15. MCP Server Integration

### Goal
Connect xaft to Model Context Protocol servers (e.g. filesystem, postgres, github).
Mirrors Claude Code's MCP support.

### Architecture

```text
XaftRuntime::with_mcp(config)
  │
  ├── spawn N subprocess (one per server in config.mcp.servers)
  │     └── JSON-RPC over stdio (or HTTP+SSE if config.mcp.transport = "sse")
  ├── for each remote tool discovered:
  │     └── McpTool { server_id, name, schema, transport }
  │         └── exposed as Arc<ErasedTool> in ToolRegistry
  └── McpRegistry { by_server, by_name, connection_status }
```

### Config

```toml
[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }

[mcp.servers.filesystem]
command = "mcp-server-filesystem"
args = ["/home/user/projects"]

[mcp]
auto_start = true
startup_timeout_secs = 10
```

### Behaviour

- `McpRegistry::discover_all()` connects, lists tools, registers them.
- Each `McpTool::call()` forwards to the server via JSON-RPC, awaits response.
- If a server disconnects, emit `McpServerDisconnected { server_id, reason }`; show
  inline `✗ mcp: github disconnected`.
- Per-server toggle via `mcp.servers.<name>.enabled`.
- TUI `/mcp` (F1) shows server status table.

### Files

| File | Change |
|---|---|
| `xaft-mcp/src/lib.rs` | **New** crate |
| `xaft-mcp/src/registry.rs` | `McpRegistry` |
| `xaft-mcp/src/transport/{stdio,sse}.rs` | Transport impls |
| `xaft-mcp/src/tool.rs` | `McpTool` (wraps remote tool as `Tool`) |
| `xaft-config/src/types.rs` | `McpConfig` (already exists — extend) |
| `xaft-runtime/src/lib.rs` | `pub mod mcp;` re-export |
| `xaft-runtime/src/orchestrator.rs` | Wire McpRegistry into ToolRegistry |

### Tests

- `mcp_registry_connects_to_stdio_server`
- `mcp_tool_forwards_call_to_server`
- `mcp_registry_handles_server_disconnect`
- `mcp_disabled_server_excluded_from_tools`
- `mcp_startup_timeout_aborts_hung_server`

### Acceptance criteria

1. `mcp__github__list_issues` shows up as a tool when `[mcp.servers.github]` configured.
2. Tool calls round-trip through JSON-RPC within 30s timeout.
3. Disconnects are surfaced to TUI and recoverable via `/mcp reconnect`.

---

## F16. Hook System (User Pre/Post-Tool Scripts)

### Goal
User-defined shell commands that run before/after each tool invocation. Mirrors
Claude Code's hook system.

### Config

```toml
[[hook.pre]]
match_tool = "edit_file"            # glob; "*" = all tools
command = "./scripts/format-on-edit.sh"
timeout_secs = 30

[[hook.post]]
match_tool = "bash_exec"
command = "echo $XAFT_TOOL_OUTPUT >> .xaft/audit.log"
env_passthrough = ["XAFT_TOOL_OUTPUT", "XAFT_TOOL_NAME"]

[[hook.pre]]
match_tool = "*"
command = "./scripts/security-scan.sh"
block_on_exit_nonzero = true
```

### Hook decision

Each `pre` hook can return:
- exit 0 → proceed
- exit 2 → block tool (stderr fed back to LLM as tool error)
- other nonzero → abort the agent run

### Behaviour

- Implemented as `agtrs_runtime::tool::ToolHook`.
- Registered in `ToolRegistry` for every tool.
- `XAFT_TOOL_NAME`, `XAFT_TOOL_INPUT`, `XAFT_TOOL_OUTPUT` env vars set.
- TUI shows `⚙ hook: ./scripts/format-on-edit.sh` line per invocation.

### Files

| File | Change |
|---|---|
| `xaft-hooks/src/lib.rs` | **New** crate |
| `xaft-hooks/src/shell_hook.rs` | `ShellHook` impl `ToolHook` |
| `xaft-config/src/types.rs` | `[[hook.pre]]` / `[[hook.post]]` arrays |
| `xaft-runtime/src/orchestrator.rs` | Apply hooks to ToolRegistry |
| `xaft-agent/src/signals.rs` | `HookExecuted { hook, exit, duration }` |

### Tests

- `shell_hook_runs_pre_command`
- `shell_hook_blocks_tool_on_exit_2`
- `shell_hook_runs_post_command_with_output`
- `shell_hook_respects_timeout`
- `shell_hook_glob_matches_multiple_tools`

### Acceptance criteria

1. Hooks from config are wired into the registry before the first LLM call.
2. Block-on-nonzero prevents the tool result from being delivered to the LLM.

---

## F17. Skill / Plugin Loader

### Goal
A directory-based plugin system: drop a `.toml` file in `~/.xaft/skills/` or
`.xaft/skills/`, and it becomes available as a `/skill-name` slash command or as a
new tool.

### Skill manifest

```toml
# .xaft/skills/commit-message.toml
[skill]
name = "commit-message"
description = "Generate a conventional commit message from staged changes"
invoke_via = "/commit-message"           # slash command
# OR
invoke_via = "tool"                       # exposed as a callable tool

[skill.tool]
name = "generate_commit_message"
description = "Generate a conventional commit message"
requires_confirmation = false

[skill.prompt]
system_prompt = """
You are a commit message generator. Given a git diff, produce a Conventional Commit
message in this format:
<type>(<scope>): <description>

<body>

<footer>
"""
tools = ["git_status", "git_diff"]
model = "fast"

[skill.context]
include_staged_diff = true
max_diff_bytes = 50_000
```

### Behaviour

- Loaded on startup, also hot-reloadable via F-watch config.
- Each skill instantiates a sub-agent (reuse F-23 infrastructure).
- `/skills` (F1) lists available skills.
- Skills can be marked `enabled = false` to disable without removing the file.

### Files

| File | Change |
|---|---|
| `xaft-skills/src/lib.rs` | **New** crate |
| `xaft-skills/src/loader.rs` | Directory scanner, TOML parser |
| `xaft-skills/src/registry.rs` | `SkillRegistry` |
| `xaft-skills/src/invocation.rs` | `invoke_skill(name, args)` |
| `xaft-runtime/src/orchestrator.rs` | Register skills at boot |
| `xaft-tui/src/slash/registry.rs` | Register skill commands |

### Tests

- `skill_loader_discovers_toml_files_in_dir`
- `skill_loader_parses_manifest_fields`
- `skill_invocation_runs_subagent`
- `skill_hot_reload_picks_up_new_files`
- `skill_disabled_flag_excludes_from_registry`

### Acceptance criteria

1. Adding a new `.toml` to `.xaft/skills/` and restarting registers the skill.
2. `SkillRegistry` integrates with `SubagentPool` for parallel execution.

---

## F18. Project Subagents (`.xaft/agents/*.md`)

### Goal
Define custom subagents per-project via markdown + YAML frontmatter. Mirrors
Claude Code's custom subagent files. Files in `.xaft/agents/*.md` are auto-loaded
when working in that project.

### File format

```markdown
---
name: code-reviewer
description: Reviews code changes for security and style issues
model: sonnet
tools: [read_file, list_files, grep, git_diff]
---

You are a senior code reviewer. When invoked, you receive a diff and produce:
1. A list of issues with severity (critical / major / minor)
2. Suggested fixes
3. Approval status (APPROVE / REQUEST_CHANGES / COMMENT)

Be specific. Reference line numbers. Be brief.
```

### Behaviour

- Discovered at session start (in addition to project-config).
- Available via `handoff_to_agent("code-reviewer", summary)`.
- `/agents` (F1) lists them alongside built-ins.
- `Task` tool (Claude Code style) can spawn them in parallel.

### Files

| File | Change |
|---|---|
| `xaft-agents/src/project.rs` | **New** — `ProjectAgentLoader` |
| `xaft-agents/src/registry.rs` | Merge loaded agents into `AgentRegistry` |
| `xaft-config/src/types.rs` | Add `agents.search_paths: Vec<PathBuf>` |

### Tests

- `project_loader_discovers_md_files`
- `project_loader_parses_yaml_frontmatter`
- `project_loader_registers_agent_with_tools`
- `project_loader_deduplicates_by_name`
- `project_loader_handles_invalid_yaml_gracefully`

### Acceptance criteria

1. A project with `.xaft/agents/refactor.md` exposes `refactor` as a handoff target.
2. The agent's tools are restricted to the declared set.
3. Reloading requires `/reload-agents` (intentional; no hot-reload of these).

---

# Part 4 — Workflow Reliability

## F19. Auto-Compaction (Context-Window Overflow Handling)

### Goal
When the conversation approaches the model's context window, automatically
summarise older messages while preserving tool-result integrity and recent turns.
Mirrors Claude Code's `/compact` behaviour.

### Trigger

- Manual via `/compact` (F1).
- Automatic when `total_tokens > config.compaction.threshold_pct * context_window`
  (default: 80%).

### Compaction strategy

1. Identify boundary: keep the last N turns intact (default: 4 turns).
2. Summarise older turns in two passes:
   - **Pass 1**: Per-turn summary (decision + tools used + key outputs).
   - **Pass 2**: Hierarchical merge into a single "Earlier context" block.
3. Replace older messages with `[SUMMARY] + [RECENT_TURNS]`.
4. Store the full conversation in `ConversationStore` (lossless).
5. Emit `ContextCompacted { messages_removed, tokens_saved }` signal.

### Architecture

```text
executor (post-LLM-call)
  │
  ▼
if tokens_used / context_window > threshold:
  Compactor::compact(messages, llm, config)
    │
    ├── split at boundary (keep_recent_turns)
    ├── summarize older (llm call with summary-only prompt)
    ├── persist full history to ConversationStore
    └── replace messages in AgentContext
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/compactor.rs` | **New** — `Compactor` |
| `xaft-runtime/src/dispatch.rs` | Call compactor after each `ModelCallComplete` |
| `xaft-config/src/types.rs` | Add `compaction: CompactionConfig` |
| `xaft-agent/src/signals.rs` | Add `ContextCompacted` |

### Tests

- `compactor_summarises_older_turns`
- `compactor_preserves_recent_n_turns`
- `compactor_persists_full_history`
- `compactor_emits_signal_with_token_savings`
- `compactor_handles_empty_history_gracefully`
- `compactor_respects_threshold_config`

### Acceptance criteria

1. Sessions that would exceed 80% of context window auto-compact.
2. The agent retains awareness of the full task (summary preserves decisions).
3. `/compact` works as a manual trigger.

---

## F20. Session Forking

### Goal
At any point in a session, the user can "fork" — copy the session up to message N
into a new session, then explore a different path. Mirrors Claude Code's `/rewind`.

### Behaviour

1. `/rewind [N]` (or `/rewind last`) copies current session up to message N into
   a new session with a new `SessionId`.
2. New session inherits: working_dir, model, tools, memory, but NOT signal bus
   (fresh) and NOT session-scoped variables.
3. The original session remains intact and resumable.
4. TUI shows `[FORK: <new-id>]` line in transcript.
5. `/resume <new-id>` returns to the fork.

### Architecture

```text
SessionForker::fork(source: &AgentSession, up_to_message: usize) -> AgentSession
  │
  ├── clone source up to message N
  ├── generate new SessionId
  ├── copy relevant ConversationStore messages to new key
  ├── write new session file
  └── return new session
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/fork.rs` | **New** — `SessionForker` |
| `xaft-runtime/src/session_store.rs` | Add `fork(&self, source, up_to) -> AgentSession` |
| `xaft-tui/src/slash/rewind.rs` | **New** — `/rewind` slash command |
| `xaft-cli/src/commands/run.rs` | Add `--fork-at <msg>` flag |
| `xaft-agent/src/signals.rs` | Add `SessionForked { source_id, new_id, message_index }` |

### Tests

- `forker_creates_new_session_with_cloned_state`
- `forker_copies_messages_up_to_index`
- `forker_preserves_working_dir_and_model`
- `forker_creates_independent_conversation_store`
- `forked_session_resumes_correctly`
- `original_session_unaffected_by_fork`

### Acceptance criteria

1. `/rewind` shows a message-picker if no N given.
2. Forked session has full transcript, can be resumed.
3. Cost / token counters in the fork start at zero (fresh start).

---

# Cross-Cutting Concerns

## Config additions

```toml
[slash]
enabled = ["help", "clear", "compact", "cost", "init", "agents", "mcp",
            "resume", "rewind", "permissions", "model", "vim", "theme",
            "login", "logout", "doctor", "memory", "diff", "commit", "pr"]

[plan]
default_mode = "off"          # "off" | "auto" | "required"

[mention]
max_inline_bytes = 50_000
max_inline_lines = 2000

[bash_mode]
allowed = true
sandbox = "default"

[image]
enabled = true
max_bytes = 10_000_000
auto_attach = true

[status_bar]
enabled = true
collapsed_default = false

[dashboard]
enabled = true
default_view = "cost"

[todo]
persist_across_handoffs = true
auto_prompt = true            # planner/coder prompted to use it

[tool.web]
enabled = true
allowed_domains = []
provider = "brave"            # brave | serper | tavily | none
api_key_env = "BRAVE_API_KEY"
cache_ttl_secs = 3600

[tool.notebook]
enabled = true

[tool.background_bash]
enabled = true
max_concurrent = 4
default_timeout_secs = 3600

[tool.pr]
backend = "gh"                # gh | glab
push_before_create = true

[tool.diff_hunks]
enabled = true

[mcp]
auto_start = true
startup_timeout_secs = 10

[hook.pre]   # array of tables; see F16
match_tool = "*"
command = ""
timeout_secs = 30

[hook.post]

[skill]
search_paths = [".xaft/skills", "~/.xaft/skills"]

[agents]
search_paths = [".xaft/agents", "~/.xaft/agents"]

[compaction]
threshold_pct = 80
keep_recent_turns = 4
auto_trigger = true

[fork]
enabled = true
```

## Signal additions (single file: `xaft-agent/src/signals.rs`)

| Signal | Added by | Fields |
|---|---|---|
| `SlashCommandExecuted` | F1 | `name, args` |
| `PlanApprovalRequired` | F2 | `plan, tools_blocked` |
| `PlanApproved` / `PlanReviseRequested` / `PlanCancelled` | F2 | `plan_id` |
| `MentionExpanded` | F3 | `path, byte_size, truncated` |
| `BashModeExecuted` | F4 | `command, exit_code, duration_ms` |
| `ImageAttached` | F5 | `path, mime, byte_size` |
| `TodosUpdated` | F9 | `items, agent_name` |
| `WebFetched` / `WebSearched` | F10 | per-tool fields |
| `NotebookEdited` | F11 | `path, cell_id, edit_mode` |
| `BashTaskSpawned` / `BashTaskOutput` / `BashTaskCompleted` | F12 | per-task fields |
| `PrCreated` | F13 | `url, number, branch` |
| `HunksApplied` / `HunksRejected` | F14 | `path, indices` |
| `McpServerConnected` / `McpServerDisconnected` | F15 | `server_id, reason` |
| `HookExecuted` | F16 | `hook, exit, duration` |
| `SkillInvoked` | F17 | `name, args` |
| `ProjectAgentLoaded` | F18 | `name, path` |
| `ContextCompacted` | F19 | `messages_removed, tokens_saved` |
| `SessionForked` | F20 | `source_id, new_id, message_index` |

## Crate additions

| New crate | Owner | Depends on |
|---|---|---|
| `xaft-mcp` | F15 | `tokio`, `serde_json`, `reqwest` (for SSE) |
| `xaft-hooks` | F16 | `tokio`, `agtrs-runtime` |
| `xaft-skills` | F17 | `xaft-agents`, `toml`, `notify` (hot-reload) |
| `xaft-project-agents` | F18 | `xaft-agents`, `serde_yaml` |

## Test infrastructure

- Each new tool gets 3+ unit tests (success, edge case, error).
- Each new slash command gets 1+ integration test using `MockLlmProvider` /
  `MockTransport`.
- All new signals get a `bridge_forwards_<signal>` test in
  `xaft-tui/src/bridge.rs`.
- All e2e paths exercised via the existing `tests/explore_pool.rs` pattern: mock the
  LLM, run the workflow, assert signal order and tool-call counts.

---

# Rollout Plan

The features form 5 independent, shippable milestones. Each milestone can land in a
separate PR without breaking earlier work.

| Milestone | Features | Estimated surface |
|---|---|---|
| **M1 — Interactive UX foundation** | F1, F2, F3, F4, F5 | `xaft-tui` + new modules |
| **M2 — Agent toolkit** | F9, F10, F11, F12, F13, F14 | `xaft-tools` + new signals |
| **M3 — Extensibility layer** | F15, F16, F17, F18 | 4 new crates + registry merge |
| **M4 — Workflow reliability** | F19, F20 | `xaft-runtime` new modules |
| **M5 — Polish** | F6, F7, F8 | `xaft-tui` widgets |

Recommended order: **M4 → M2 → M1 → M3 → M5**, since M4 is the smallest
infrastructure add, M2 adds tools, M1 adds user-facing surfaces that consume the
tools, M3 plugs into the runtime, M5 polishes the UI.

---

# Acceptance Criteria (Whole PRD)

1. All 20 features implemented with unit + integration tests passing.
2. `just test` — full workspace green.
3. `just lint` — no new clippy warnings in the 20 feature files (pre-existing
   warnings in `xaft-config` are out of scope).
4. AGENTS.md invariants preserved: every new mutation emits a signal; no
   structured-output-driven state changes; tools remain the only mutation boundary.
5. Every new tool is registered through `ToolRegistry` (no ad-hoc wiring).
6. Every new config field documented in `xaft-config` with a default.
7. Backward compatibility: `XaftConfig::default()` continues to work unchanged.
8. Zero new runtime dependencies beyond what's declared in each new crate's
   `Cargo.toml`.
9. Each new crate publishes a `lib.rs` quick-start example using `MockLlmProvider`.
10. The whole PRD can be incrementally enabled: every feature gated by its config
    flag, defaulting to a safe value.

---

# Files to create / modify (summary)

| File | Change |
|---|---|
| `prds/27_claude_code_parity_features.md` | This file |
| `crates/xaft-tui/src/slash/{mod,parser,help,...}.rs` | F1 |
| `crates/xaft-tui/src/plan_card.rs` | F2 |
| `crates/xaft-tui/src/mention.rs` | F3 |
| `crates/xaft-tui/src/bash_mode.rs` | F4 |
| `crates/xaft-tui/src/paste.rs` | F5 |
| `crates/xaft-tui/src/picker.rs` | F6 |
| `crates/xaft-tui/src/surface.rs` | F7 (extend) |
| `crates/xaft-tui/src/dashboard.rs` | F8 |
| `crates/xaft-tools/src/todo/{mod,write,read}.rs` | F9 |
| `crates/xaft-tools/src/web/{fetch,search,provider,cache}.rs` | F10 |
| `crates/xaft-tools/src/notebook/mod.rs` | F11 |
| `crates/xaft-tools/src/bash/{run,poll,kill,list,registry}.rs` | F12 |
| `crates/xaft-tools/src/pr.rs` | F13 |
| `crates/xaft-tools/src/diff/{hunks,apply,reject,hunk}.rs` | F14 |
| `crates/xaft-mcp/src/{lib,registry,tool,transport/*}.rs` | F15 |
| `crates/xaft-hooks/src/{lib,shell_hook}.rs` | F16 |
| `crates/xaft-skills/src/{lib,loader,registry,invocation}.rs` | F17 |
| `crates/xaft-agents/src/project.rs` | F18 |
| `crates/xaft-runtime/src/compactor.rs` | F19 |
| `crates/xaft-runtime/src/fork.rs` | F20 |
| `crates/xaft-agent/src/signals.rs` | 20+ new signal types |
| `crates/xaft-config/src/types.rs` | New config sections |
| `crates/xaft-runtime/src/orchestrator.rs` | Wire new tools, hooks, skills, subagents, compactor, forker |
| `crates/xaft-runtime/src/lib.rs` | Re-exports |
| `crates/xaft-agents/src/{planner,coder,qa}.rs` | Update system prompts to mention `TodoWrite`, `WebSearch`, etc. |
| `crates/xaft-cli/src/args.rs` | New flags: `--plan`, `--fork-at`, etc. |
| `crates/xaft-cli/src/commands/run.rs` | Branch on no-task → picker, `/pr` integration |
| `crates/xaft-tui/src/state.rs` | Render all new widgets |
| `crates/xaft-tui/src/bridge.rs` | Forward all new signals |
| `Cargo.toml` (workspace) | Add `xaft-mcp`, `xaft-hooks`, `xaft-skills` members |
