# PRD 32d — Slash Command System: Command Catalogue

## Status: Draft
## Depends on: PRD 32a, PRD 32b, PRD 32c

---

This document fully specifies every slash command — inputs, outputs,
implementation steps, edge cases, and the exact text the user sees.

---

## Command Index

| # | Trigger | File | Async |
|---|---------|------|-------|
| 1 | `/agents` | `commands/agents.rs` | No |
| 2 | `/clear` | `commands/clear.rs` | No |
| 3 | `/compact` | `commands/compact.rs` | Yes |
| 4 | `/commit` | `commands/commit.rs` | Yes |
| 5 | `/config` | `commands/config.rs` | No |
| 6 | `/cost` | `commands/cost.rs` | No |
| 7 | `/diff` | `commands/diff.rs` | Yes |
| 8 | `/doctor` | `commands/doctor.rs` | Yes |
| 9 | `/emacs` | `commands/vim.rs` | No |
| 10 | `/help` | `commands/help.rs` | No |
| 11 | `/init` | `commands/init.rs` | Yes |
| 12 | `/login` | `commands/login.rs` | Yes |
| 13 | `/logout` | `commands/login.rs` | No |
| 14 | `/mcp` | `commands/mcp.rs` | No |
| 15 | `/memory` | `commands/memory.rs` | No |
| 16 | `/model` | `commands/model.rs` | No |
| 17 | `/permissions` | `commands/permissions.rs` | No |
| 18 | `/pr` | `commands/pr.rs` | Yes |
| 19 | `/resume` | `commands/resume.rs` | Yes |
| 20 | `/rewind` | `commands/rewind.rs` | Yes |
| 21 | `/theme` | `commands/theme.rs` | No |
| 22 | `/vim` | `commands/vim.rs` | No |

---

## 1. `/agents`

**Description**: List all loaded agents in the registry with their roles and
available tool counts.

**Args**: None.

**Handler struct**: `AgentsHandler { registry: Arc<AgentRegistry> }`

**Output** (example):

```
  ╌╌ /agents ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  AGENT      ROLE      TOOLS   MAX_TURNS
  ─────────────────────────────────────────
  planner    planner   5       25
  coder      coder     12      100
  qa         qa        6       100
  fixer      fixer     12      100
  4 agents loaded.
```

**Implementation**:

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    let agents = self.registry.all_agents();
    let mut table = CliTable::new(["AGENT", "ROLE", "TOOLS", "MAX_TURNS"]);
    for a in &agents {
        table.push_row([
            a.name(),
            a.role_name(),
            &a.tools().len().to_string(),
            &a.max_turns().to_string(),
        ]);
    }
    let mut lines = table.render(ctx.terminal_cols);
    lines.push(StyledLine::new(
        format!("  {} agents loaded.", agents.len()),
        LineKind::System,
    ));
    CommandResult::StyledLines(lines)
}
```

**Edge cases**:
- Empty registry: print `No agents loaded.` (should never happen in production).

---

## 2. `/clear`

**Aliases**: `/cls`

**Description**: Clear the current session transcript visible in the TUI.
The underlying conversation history in SQLite is NOT deleted — only the display
is reset.

**Args**: None.

**Handler struct**: `ClearHandler`

**Output**:

```
  ╌╌ /clear ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Session display cleared.
```

**Implementation**:

Pushes `RenderMutation::ClearTranscript` (a new variant) which tells
`IncrementalRenderer` to reprint only from an empty transcript.

```rust
fn execute(&self, _ctx: CommandContext) -> CommandResult {
    CommandResult::Handled  // handler pushes mutation directly
}
```

But since handlers don't have direct mutation access, the cleaner approach is
a dedicated signal:

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    // Emit a signal the event loop converts to ClearTranscript mutation.
    let _ = ctx.signals.emit_sync(SessionDisplayCleared);
    CommandResult::Lines(vec!["Session display cleared.".into()])
}
```

**Edge cases**:
- Running during active agent streaming: flush the stream first, then clear.

---

## 3. `/compact`

**Aliases**: `/ctx`

**Description**: Manually trigger LLM context compaction for the current session
(normally triggered automatically). Summarises the conversation history, replaces
it with a compact summary, and reports token savings.

**Args**: None.

**Async**: Yes (LLM call required).

**Handler struct**: `CompactHandler { runtime: Arc<RuntimeHandle> }`

**Output**:

```
  ╌╌ /compact ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ⣷ Compacting context…                ← spinner while running
  Compacted 14 320 tokens → 1 842 tokens  (87% reduction)
  Cost: $0.0021
```

**Implementation**:

```rust
async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let session_id = match &ctx.session_id {
        Some(id) => id.clone(),
        None => return CommandResult::Error("No active session to compact.".into()),
    };
    match self.runtime.compact_context(&session_id).await {
        Ok(report) => CommandResult::Lines(vec![
            format!(
                "Compacted {} tokens → {} tokens  ({:.0}% reduction)",
                fmt_tokens(report.tokens_before),
                fmt_tokens(report.tokens_after),
                report.reduction_pct(),
            ),
            format!("Cost: ${:.4}", report.cost_usd),
        ]),
        Err(e) => CommandResult::Error(format!("Compaction failed: {e}")),
    }
}
```

**Edge cases**:
- No active session: `Error("No active session to compact.")`.
- Agent is running: return `Error("Cannot compact while an agent is active. Press Ctrl+C first.")`.
- Already minimal context: `"Context is already compact (N tokens). Nothing to do."`.

---

## 4. `/commit`

**Description**: Commit all session working-tree changes to the session branch
(`xaft/session-<uuid>`), bypassing the default "commit on task completion"
policy.  Intended for checkpointing mid-session.

**Args**: Optional commit message (default: auto-generated from session task).

**Async**: Yes (git I/O).

**Handler struct**: `CommitHandler`

**Output**:

```
  ╌╌ /commit ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Committed 3 files to xaft/session-f7fba85e
  abc1234  chore: session checkpoint
```

**Implementation**:

```rust
async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let msg = if ctx.args.is_empty() {
        format!("chore: session checkpoint [{}]",
            ctx.session_id.as_deref().unwrap_or("unknown"))
    } else {
        ctx.args.clone()
    };

    match xaft_tools::git::commit_session(&ctx.working_dir, &msg).await {
        Ok(info) => CommandResult::Lines(vec![
            format!("Committed {} file(s) to {}", info.files_changed, info.branch),
            format!("{}  {}", &info.sha[..7], info.message),
        ]),
        Err(e) => CommandResult::Error(format!("Commit failed: {e}")),
    }
}
```

**Edge cases**:
- No changes to commit: `"Nothing to commit (working tree clean)."`.
- Not in a git repo: `Error("Not a git repository.")`.
- Merge conflicts: `Error("Cannot commit: unresolved merge conflicts.")`.

---

## 5. `/config`

**Description**: Display the current resolved configuration (all layers merged).
Opens a read-only view; does not launch an editor (editing is deferred to a
future modal PRD).

**Args**: Optional key path to filter, e.g. `/config provider.anthropic`.

**Handler struct**: `ConfigHandler { config: Arc<XaftConfig> }`

**Output** (no args):

```
  ╌╌ /config ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Resolved config (6-layer merge):
  [provider.anthropic]
    type        = "anthropic"
    api_key_env = "ANTHROPIC_API_KEY"
  [agent.default]
    provider  = "anthropic"
    model     = "claude-3-5-sonnet-20241022"
    max_turns = 25
  …
```

**Output** (with key path):

```
  [provider.anthropic]
    type        = "anthropic"
    api_key_env = "ANTHROPIC_API_KEY"
```

**Implementation**:

Serialise `ctx.config` to TOML via `toml::to_string_pretty`, then optionally
filter lines by key path.  Each line is emitted as `LineKind::System`.

**Edge cases**:
- Unknown key path: `"No config key matches 'provider.xyz'."`.
- Secrets (`api_key`, `api_key_env` values containing actual keys): redacted
  as `[redacted]` if the value does not look like an env-var name (all caps +
  underscores).

---

## 6. `/cost`

**Aliases**: `/tokens`

**Description**: Display a per-agent breakdown of token usage and estimated cost
for the current session.

**Args**: None.

**Handler struct**: `CostHandler`

**Output**:

```
  ╌╌ /cost ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  AGENT     CALLS  IN       OUT    CACHE_R  COST
  ─────────────────────────────────────────────────
  planner   3      12 450   3 210  8 000    $0.0123
  coder     1      48 102   9 004  0        $0.0812
  qa        1      7 340    892    0        $0.0061
  ─────────────────────────────────────────────────
  TOTAL     5      67 892   13 106 8 000    $0.0996
```

**Implementation**:

Read `ctx.llm_stats` (an `Arc<RwLock<AgentStatsMap>>`), format into a
`CliTable`.

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    let stats = ctx.llm_stats.read().unwrap();
    if stats.is_empty() {
        return CommandResult::Lines(vec!["No LLM calls recorded yet.".into()]);
    }

    let mut table = CliTable::new(["AGENT", "CALLS", "IN", "OUT", "CACHE_R", "COST"]);
    let (mut ti, mut to, mut tc, mut cost, mut calls) = (0u64, 0u64, 0u64, 0f64, 0u32);

    for (agent, s) in stats.iter() {
        table.push_row([
            agent,
            &s.calls.to_string(),
            &fmt_tokens(s.input_tokens),
            &fmt_tokens(s.output_tokens),
            &fmt_tokens(s.cache_read),
            &format!("${:.4}", s.cost_usd),
        ]);
        ti += s.input_tokens; to += s.output_tokens;
        tc += s.cache_read; cost += s.cost_usd; calls += s.calls;
    }
    table.push_separator();
    table.push_row(["TOTAL", &calls.to_string(),
        &fmt_tokens(ti), &fmt_tokens(to), &fmt_tokens(tc),
        &format!("${:.4}", cost)]);

    CommandResult::StyledLines(table.render(ctx.terminal_cols))
}
```

---

## 7. `/diff`

**Description**: Show the working-tree diff between the current state and the
session base commit (`xaft/session-<uuid>` branch point).

**Args**: Optional file path to restrict the diff.

**Async**: Yes (git I/O).

**Output**:

```
  ╌╌ /diff ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  --- a/password_generator.py
  +++ b/password_generator.py
  @@ -1,3 +1,5 @@
  +import math
  +import secrets
  …
  3 files changed, +142 / -12
```

**Implementation**:

Call `xaft_tools::git::diff(&ctx.working_dir, path_filter).await`.  Lines
starting with `+` are rendered `LineKind::AgentText` (green), lines starting
with `-` are `LineKind::Error` (red), `@@` lines are `LineKind::System` (dim).

**Edge cases**:
- No changes: `"No changes in working tree."`.
- Diff too large (> 500 lines): show the first 500 lines and `… (N more lines, use /commit to commit)`.

---

## 8. `/doctor`

**Description**: Run a suite of diagnostic checks covering configuration,
connectivity, git state, and dependencies.  Prints a pass/fail report.

**Args**: None.

**Async**: Yes (network + process I/O).

**Output**:

```
  ╌╌ /doctor ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ✓ Config file found at ~/.config/xaft/xaft.toml
  ✓ ANTHROPIC_API_KEY is set
  ✓ API reachable (claude-3-5-sonnet-20241022, 142 ms)
  ✓ git found at /usr/bin/git (version 2.43.0)
  ✓ Working directory is a git repository
  ✗ MCP server "filesystem" not responding (connection refused)
  ✓ SQLite session store at ~/.xaft/sessions.db (12 sessions)
  6 checks, 1 failure.
```

**Checks** (run concurrently with `tokio::join!`):

| Check | Pass condition |
|-------|---------------|
| Config file | `XaftConfig::locate_file()` returns `Some` |
| API key | env var is set and non-empty |
| API connectivity | One model-list or ping call succeeds within 5 s |
| git binary | `which git` succeeds |
| Git repo | `.git` directory found in `working_dir` |
| MCP servers | Each configured MCP server responds to ping |
| SQLite store | DB file exists and is readable |

**Edge cases**:
- Network timeout: mark as `✗ API unreachable (timeout after 5 s)`.
- Partial MCP failure: list each server separately.

---

## 9. `/emacs`

**Description**: Switch the input bar keymap to Emacs bindings.

**Args**: None.

**Handler struct**: `EmacsHandler`

**Output**:

```
  Keymap set to Emacs.
```

**Emacs bindings added**:

| Key | Action |
|-----|--------|
| `Ctrl+A` | Move to start of line |
| `Ctrl+E` | Move to end of line |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+Y` | Yank (paste kill-ring) |
| `Alt+F` | Forward one word |
| `Alt+B` | Backward one word |
| `Ctrl+W` | Kill previous word |
| `Alt+D` | Kill next word |

**Implementation**: Sets `AppState.keymap = Keymap::Emacs`.

---

## 10. `/help`

**Description**: List all available slash commands with descriptions.

**Args**: Optional command name for detailed help on a single command.

**Handler struct**: `HelpHandler { registry: Arc<SlashCommandRegistry> }`

**Output** (no args):

```
  ╌╌ /help ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Slash commands:

  /agents      List loaded agents and their tool counts
  /clear       Reset the current session transcript
  /compact     Manually trigger context compaction
  /commit      Commit session changes to git
  /config      Show the resolved configuration
  /cost        Show per-agent token and cost table
  /diff        Show working-tree diff against session base
  /doctor      Run configuration and connectivity diagnostics
  /emacs       Switch input bar to Emacs keybindings
  /help        Show this help (or /help <command> for details)
  /init        Generate AGENTS.md project memory file
  /login       Authenticate (OAuth or API key)
  /logout      Remove stored credentials
  /mcp         Show MCP server status
  /memory      Browse the memory store
  /model       Switch the LLM model mid-session
  /permissions Open the per-tool permission editor
  /pr          Create a GitHub pull request
  /resume      Resume a prior session
  /rewind      Roll back transcript to message N
  /theme       Cycle or set the TUI colour theme
  /vim         Switch input bar to Vim keybindings

  Type /help <command> for detailed usage.
```

**Output** (`/help compact`):

```
  /compact — Manually trigger context compaction
  Aliases: /ctx
  Args:     (none)
  Async:    yes

  Summarises the active conversation history via an LLM call and replaces
  it with a compact representation, reducing future token costs.

  Example:
    /compact
```

**Implementation**:

Sort all handlers alphabetically by trigger.  For single-command help, print
description, aliases, args hint, async flag, and a usage example (if the
handler implements `fn example() -> Option<&'static str>`).

---

## 11. `/init`

**Description**: Scan the working directory and generate an `AGENTS.md` file
(Claude-Code-style project memory) using an LLM call.

**Args**: None.

**Async**: Yes (LLM call + filesystem writes).

**Output**:

```
  ╌╌ /init ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Scanning repository…
  Generated AGENTS.md (1 284 bytes)
```

**Implementation**:

1. Call `ListFilesTool` with depth 2 to get a repository overview.
2. Build a prompt: `"You are a code summariser. Given this file tree, write a
   concise AGENTS.md that describes the project structure, key files, and any
   conventions an AI agent should know before making changes."`.
3. Call the LLM provider directly (not through the agent loop).
4. Write the response to `working_dir/AGENTS.md`.

**Edge cases**:
- `AGENTS.md` already exists: prompt the user with
  `"AGENTS.md already exists. Overwrite? (y/N): "` using a one-line inline
  confirmation (not a modal).  Default is No.
- LLM error: return `CommandResult::Error`.

---

## 12. `/login`

**Description**: Interactively authenticate — either paste an API key or initiate
an OAuth flow for supported providers.

**Args**: Optional provider name (default: the configured default provider).

**Async**: Yes.

**Output**:

```
  ╌╌ /login ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Provider: anthropic
  Enter API key (input hidden): ****
  ✓ Saved to ~/.config/xaft/xaft.toml
  ✓ API key verified (claude-3-5-sonnet-20241022 reachable)
```

**Implementation**:

1. Determine provider from args or config.
2. For `anthropic` / `openai` / `google`: prompt for API key.
   Use `crossterm::terminal::disable_raw_mode` + `rpassword` for hidden input.
3. Write the key to the user config file under `[provider.<name>] api_key`.
4. Verify by making a lightweight API call.

**Edge cases**:
- Provider does not support API key auth (custom providers): `Error("Provider X uses env-var auth. Set XAFT_<NAME>_KEY in your shell.")`.
- Verification fails: warn but save anyway (`"Key saved but verification failed: {e}"`).

---

## 13. `/logout`

**Description**: Remove stored API credentials for a provider.

**Args**: Optional provider name (default: all providers).

**Output**:

```
  Removed API key for provider: anthropic
```

**Implementation**:

Remove the `api_key` field from the user config TOML under the relevant provider
section.

---

## 14. `/mcp`

**Description**: Show the status of all configured MCP (Model Context Protocol)
servers — name, transport, tool count, and connectivity status.

**Args**: None.

**Handler struct**: `McpHandler { registry: Arc<McpRegistry> }`

**Output**:

```
  ╌╌ /mcp ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  SERVER        TRANSPORT  TOOLS  STATUS
  ────────────────────────────────────────────
  filesystem    stdio      12     ✓ connected
  memory        http       5      ✗ not responding
  2 servers, 1 issue.
```

**Implementation**:

`McpRegistry::status_snapshot()` returns a `Vec<McpServerStatus>` (already
maintained by the runtime).  Format into a `CliTable`.

**Edge cases**:
- No MCP servers configured: `"No MCP servers configured."`.

---

## 15. `/memory`

**Description**: Browse the memory store — list stored facts, their topics,
and confidence scores.

**Args**: Optional search query to filter (`/memory authentication`).

**Handler struct**: `MemoryHandler { manager: Arc<MemoryManager> }`

**Output** (no args):

```
  ╌╌ /memory ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  ID      TOPICS           CONF  FACT
  ──────────────────────────────────────────────────────────────────
  m-001   auth, security   0.92  User prefers OAuth over API keys
  m-002   style            0.85  Use snake_case for config keys
  2 entries.
```

**Output** (`/memory auth`):

```
  m-001   auth, security   0.92  User prefers OAuth over API keys
  1 match.
```

**Implementation**:

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    let query = ctx.args.trim();
    let entries = if query.is_empty() {
        self.manager.list_all()
    } else {
        self.manager.search(query)
    };
    // format into CliTable
}
```

**Edge cases**:
- Empty memory store: `"Memory store is empty."`.
- Search returns no results: `"No memory entries match '{query}'."`.

---

## 16. `/model`

**Description**: Switch the LLM model used for the current session.  Takes
effect on the next agent call.

**Args**: Required model name (e.g. `claude-opus-4-8`, `gpt-4o`).

**Handler struct**: `ModelHandler { runtime: Arc<RuntimeHandle> }`

**Output**:

```
  Model switched to: claude-opus-4-8
  Note: the change takes effect on the next agent turn.
```

**Implementation**:

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    let name = ctx.args.trim().to_string();
    if name.is_empty() {
        return CommandResult::Error(
            "/model requires a model name.\n  Usage: /model <name>".into()
        );
    }
    self.runtime.set_model_override(name.clone());
    CommandResult::Lines(vec![
        format!("Model switched to: {name}"),
        "Note: the change takes effect on the next agent turn.".into(),
    ])
}
```

**Edge cases**:
- Unknown model name: warn but proceed (`"Model '{name}' not in known list — proceeding anyway. The API will reject it if unsupported."`).
- Agent is mid-turn: the override is queued; a note is printed.

---

## 17. `/permissions`

**Description**: Display the current per-tool permission rules (allow/deny/confirm).

**Args**: None (editing is deferred to a future PRD with a modal editor).

**Output**:

```
  ╌╌ /permissions ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  TOOL           RULE     SOURCE
  ────────────────────────────────────────────────────────────────
  bash_exec      confirm  built-in default
  write_file     allow    project .xaft.toml
  read_file      allow    built-in default
  git_commit     confirm  user ~/.config/xaft/xaft.toml
  delete_file    deny     project .xaft.toml
  5 rules.
```

**Implementation**:

Read `ctx.config.permissions` (a `Vec<PermissionRule>`), render as table.
Each rule has a `source` field tracking which config layer set it.

---

## 18. `/pr`

**Description**: Create a GitHub pull request for the current session branch.

**Args**: Optional PR title (default: auto-generated from the session task).

**Async**: Yes (git + GitHub API).

**Output**:

```
  ╌╌ /pr ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Pushing branch xaft/session-f7fba85e…
  ✓ Created PR #42: "Add password history and entropy improvements"
  https://github.com/user/repo/pull/42
```

**Implementation**:

```rust
async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let title = if ctx.args.is_empty() {
        auto_pr_title(&ctx).await
    } else {
        ctx.args.clone()
    };
    // 1. git push --set-upstream origin <session_branch>
    // 2. gh pr create --title <title> --body <auto_body>
    // Or use the GitHub API directly via reqwest + GITHUB_TOKEN
}
```

**Edge cases**:
- Not a git repo: `Error("Not a git repository.")`.
- No `GITHUB_TOKEN` set: `Error("GITHUB_TOKEN not set. Run /login first.")`.
- Branch already has a PR: `"PR already exists: <url>"`.
- No commits: `Error("Nothing to push — no commits on this branch.")`.

---

## 19. `/resume`

**Description**: Resume a previous session by ID, or interactively pick from
a list of recent sessions.

**Args**: Optional session ID prefix (e.g. `/resume f7fba85e`).

**Async**: Yes (DB I/O).

**Output** (no args — show list):

```
  ╌╌ /resume ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Recent sessions (most recent first):

  f7fba85e  2026-06-08 13:34  /root/rust_projects/python-password-generator
            "can you suggest 5 improvements?"
  3a1bc209  2026-06-07 22:10  /root/rust_projects/xaft
            "implement mentions"

  Use: /resume <id>
```

**Output** (with ID):

```
  Resuming session f7fba85e…
  ✓ Conversation history loaded (20 messages).
```

**Implementation**:

Without args: list up to 10 recent sessions via `ctx.session_store?.list(None)`.
With ID: call `XaftRuntime::resume_session(id)` which rewires the conversation
store and replays history (same path as `--continue` CLI flag).

**Edge cases**:
- No sessions found: `"No prior sessions found in this workspace."`.
- ID not found: `Error("Session '{id}' not found.")`.
- Active agent running: `Error("Cannot resume: an agent is active. Press Ctrl+C first.")`.

---

## 20. `/rewind`

**Description**: Roll back the conversation history to message N (0-indexed),
effectively forking the session from that point.  The rolled-back messages are
preserved in a `_rewound` shadow key but removed from the active conversation.

**Args**: Optional message index.  Without an index, shows the message list.

**Async**: Yes (DB write).

**Output** (no args — show list):

```
  ╌╌ /rewind ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Current conversation:

  [0]  user      hi how are you?
  [1]  assistant Hello! I'm doing well…
  [2]  user      can you suggest 5 improvements?
  [3]  assistant Here are 5 suggestions…
  [4]  user      Implement 5

  Use: /rewind <N>  to roll back to just before message N.
```

**Output** (`/rewind 4`):

```
  Rewound to message 3 (removed 2 messages).
  Fork point: "Implement 5" — type a new instruction to continue.
```

**Implementation**:

1. Load conversation from store.
2. Validate `msg_index < messages.len()`.
3. Save messages `[0..msg_index]` back to the conversation key.
4. Save messages `[msg_index..]` to `{session_id}::rewound::{timestamp}` for
   recovery.
5. Clear the TUI transcript and replay from the truncated history.
6. Emit `SessionRewound { session_id, from_index: msg_index }` signal.

**Edge cases**:
- `msg_index == 0`: `Error("Cannot rewind to before the first message.")`.
- `msg_index >= len`: `Error("Message index {n} out of range (max {len-1}).")`.

---

## 21. `/theme`

**Description**: Cycle through available TUI colour themes or switch to a
named theme.

**Args**: Optional theme name.  Without args, cycles to the next theme.

**Available themes**: `default`, `nord`, `solarized-dark`, `solarized-light`,
`gruvbox`, `dracula`, `monokai`, `catppuccin`.

**Output**:

```
  Theme: gruvbox
```

**Implementation**:

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    let next = if ctx.args.is_empty() {
        cycle_next_theme(&self.config.read().tui.theme)
    } else {
        ctx.args.trim().to_string()
    };

    if !KNOWN_THEMES.contains(&next.as_str()) {
        return CommandResult::Error(format!(
            "Unknown theme '{next}'. Available: {}",
            KNOWN_THEMES.join(", ")
        ));
    }

    self.config.write().tui.theme = next.clone();
    // Emit ThemeChanged signal so renderer picks up new colours immediately.
    let _ = ctx.signals.emit_sync(ThemeChanged { theme: next.clone() });
    CommandResult::Lines(vec![format!("Theme: {next}")])
}
```

---

## 22. `/vim`

**Aliases**: `/vi`

**Description**: Switch the input bar keymap to Vim modal bindings.

**Args**: None.

**Output**:

```
  Keymap set to Vim.  Press Esc for NORMAL mode.
```

**Vim bindings**:

| Mode | Key | Action |
|------|-----|--------|
| INSERT | `Esc` | Enter NORMAL mode |
| NORMAL | `i` | Enter INSERT mode at cursor |
| NORMAL | `a` | Enter INSERT mode after cursor |
| NORMAL | `I` | Enter INSERT at line start |
| NORMAL | `A` | Enter INSERT at line end |
| NORMAL | `h` / `l` | Move left / right |
| NORMAL | `j` / `k` | Move down / up (multi-line) |
| NORMAL | `w` | Forward word |
| NORMAL | `b` | Back word |
| NORMAL | `0` / `$` | Start / end of line |
| NORMAL | `gg` / `G` | First / last line |
| NORMAL | `x` | Delete char under cursor |
| NORMAL | `dd` | Delete current line |
| NORMAL | `cc` | Change entire line |
| NORMAL | `u` | Undo (deferred) |
| NORMAL | `/` | (intentionally not slash-command — launches vim search mode) |

The TUI status bar shows `[NORMAL]` / `[INSERT]` when Vim mode is active.

**Implementation**: Sets `AppState.keymap = Keymap::Vim(VimMode::Insert)`.
The keymap dispatcher in `state.rs` routes each key through the Vim state
machine before falling through to the default handler.
