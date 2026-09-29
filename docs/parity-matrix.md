# Parity Matrix — agenthicc → xaft

Tracks **user-visible surface parity** between the Python reference implementation
`agenthicc` (`~/python_projects/agenthicc`) and the Rust port `xaft`.

Scope is deliberately the *surface a user can touch*: CLI commands, slash commands,
TUI triggers and keybindings, built-in tools, configuration keys, and modes. Internal
architecture is **not** required to match — xaft is free to be idiomatic Rust.

Legend: ✅ present · 🟡 partial / divergent semantics · ❌ absent

---

## How to read this document

Each row cites the **agenthicc** source location that defines the feature and the
**xaft** crate/module that implements (or should implement) it. Status is updated as
gaps close. See `knowledge/tui-parity-gap-analysis.md` for the detailed TUI gap
narrative (gaps 1–11) that this matrix indexes.

**Source of truth.** agenthicc entries were extracted programmatically from its own
registries — `@command`/`@group` decorators in `src/agenthicc/cli/commands/*`, the
`BUILTIN_COMMANDS` list in `src/agenthicc/commands/builtins.py`, `__all__` /
`*_AGENT_TOOLS` lists in `src/agenthicc/tools/**/agent_tools.py`, the dataclasses in
`src/agenthicc/config.py`, and `src/agenthicc/modes/builtin.py`. xaft entries were
extracted from the clap enums in `crates/xaft-cli/src/args.rs`, the `SlashCommand`
enum in `crates/xaft-tui/src/slash/mod.rs`, `TOOL_NAME` consts in `crates/xaft-tools`,
the config structs in `crates/xaft-config`, and `crates/xaft-tui/src/mode/builtins.rs`.

---

## 1. CLI commands

agenthicc registers **57** CLI entries via `@command`/`@group`. xaft exposes
`run`, `config {show,init,validate}`, `session {list,show,resume,cancel}`, `completions`,
`version`.

| # | agenthicc command | agenthicc source | xaft location | Status |
|---|---|---|---|---|
| 1 | `config show` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Show` | ✅ |
| 2 | `config init` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Init` | ✅ |
| 3 | `config validate` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Validate` | ✅ |
| 4 | `config profiles` | `cli/commands/config.py` | — | ❌ |
| 5 | `session list` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::List` | ✅ |
| 6 | `session show` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Show` | ✅ |
| 7 | `session resume` | — (xaft-only; agenthicc resumes via `jobs resume`) | crates/xaft-cli `SessionSubcommand::Resume` | ✅ |
| 8 | `session send` | `cli/commands/session_service.py` | — | ❌ |
| 9 | `session control` | `cli/commands/session_service.py` | — | ❌ |
| 10 | `session create` | `cli/commands/session_service.py` | — | ❌ |
| 11 | `session events` | `cli/commands/session_service.py` | — | ❌ |
| 12 | `session export` | `cli/commands/session_service.py` | — | ❌ |
| 13 | `session serve` | `cli/commands/session_service.py` | — | ❌ |
| 14 | `sessions list` | `cli/commands/sessions.py` | `xaft session list` | ✅ |
| 15 | `sessions show` | `cli/commands/sessions.py` | `xaft session show` | ✅ |
| 16 | `sessions export` | `cli/commands/sessions.py` | — | ❌ |
| 17 | `sessions inspect` | `cli/commands/sessions.py` | — | ❌ |
| 18 | `jobs list` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 19 | `jobs status` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 20 | `jobs cancel` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 21 | `jobs approve` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 22 | `jobs reject` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 23 | `jobs input` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 24 | `jobs labels` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 25 | `jobs rename` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 26 | `jobs retry` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 27 | `jobs resume` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 28 | `jobs archive` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 29 | `jobs restore` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 30 | `jobs delete` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 31 | `jobs purge` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 32 | `mcp list` | `cli/commands/mcp.py` | crates/xaft-tui slash `/mcp` | 🟡 |
| 33 | `mcp add` | `cli/commands/mcp.py` | crates/xaft-tui slash `/mcp` | 🟡 |
| 34 | `mcp get` | `cli/commands/mcp.py` | crates/xaft-tui slash `/mcp` | 🟡 |
| 35 | `mcp remove` | `cli/commands/mcp.py` | crates/xaft-tui slash `/mcp` | 🟡 |
| 36 | `mcp connect` | `cli/commands/mcp.py` | — | ❌ |
| 37 | `mcp disconnect` | `cli/commands/mcp.py` | — | ❌ |
| 38 | `mcp auth` | `cli/commands/mcp.py` | — | ❌ |
| 39 | `mcp logout` | `cli/commands/mcp.py` | — | ❌ |
| 40 | `mcp refresh` | `cli/commands/mcp.py` | — | ❌ |
| 41 | `mcp doctor` | `cli/commands/mcp.py` | crates/xaft-tui slash `/doctor` | 🟡 |
| 42 | `runs list` | `cli/commands/runs.py` | — | ❌ |
| 43 | `runs show` | `cli/commands/runs.py` | — | ❌ |
| 44 | `runs cancel` | `cli/commands/runs.py` | — | ❌ |
| 45 | `runs resume` | `cli/commands/runs.py` | — | ❌ |
| 46 | `skills add` | `cli/commands/skills.py` | — | ❌ |
| 47 | `trust cli` | `cli/commands/trust.py` | — | ❌ |
| 48 | `workflows list` | `cli/commands/workflows.py` | — | ❌ |
| 49 | `workflows run` | `cli/commands/workflows.py` | — | ❌ |
| 50 | `worktrees list` | `cli/commands/worktrees.py` | — | ❌ |
| 51 | `worktrees show` | `cli/commands/worktrees.py` | — | ❌ |
| 52 | `worktrees integrate` | `cli/commands/worktrees.py` | — | ❌ |
| 53 | `worktrees recover` | `cli/commands/worktrees.py` | — | ❌ |
| 54 | `login` | `cli/commands/auth.py` | crates/xaft-tui slash `/login` (no CLI subcommand) | 🟡 |
| 55 | `logout` | `cli/commands/auth.py` | crates/xaft-tui slash `/logout` (no CLI subcommand) | 🟡 |
| 56 | `whoami` | `cli/commands/auth.py` | — | ❌ |
| 57 | `init` | `cli/commands/init.py` | crates/xaft-tui slash `/init` (no CLI subcommand) | 🟡 |
| 58 | `run` | `cli/parser.py` | crates/xaft-cli `Commands::Run` | ✅ |
| 59 | `agents` | `cli/commands/` (agent registry) | — (no CLI subcommand; `/agents` slash only) | 🟡 |
| 60 | `attach` | `cli/commands/` (attach to session) | — | ❌ |

**Subtotal: 60 rows.** ❌ 28 · 🟡 9 · ✅ 23

---

## 2. Slash commands

> **agenthicc source:** `src/agenthicc/commands/builtins.py` (`BUILTIN_COMMANDS`).

agenthicc registers **26** built-in slash commands (`BUILTIN_COMMANDS`) plus
**3** aliases. xaft's `SlashCommand` enum has **24** variants, several of which have no
agenthicc counterpart (they are Claude-Code-style extras: `/diff`, `/commit`, `/pr`,
`/rewind`, `/permissions`, `/theme`, `/vim`, `/emacs`, `/doctor`, `/memory`, `/agents`).

| # | agenthicc command | aliases | xaft location | Status |
|---|---|---|---|---|
| 1 | `/replay` | — | — | ❌ |
| 2 | `/cancel` | `/interrupt` | — | ❌ |
| 3 | `/loop` | — | — | ❌ |
| 4 | `/loops` | — | — | ❌ |
| 5 | `/clear` | — | crates/xaft-tui `SlashCommand::Clear` | ✅ |
| 6 | `/commands` | — | — | ❌ |
| 7 | `/tools` | — | — | ❌ |
| 8 | `/workflows` | — | — | ❌ |
| 9 | `/config` | — | crates/xaft-tui `SlashCommand::Config` | ✅ |
| 10 | `/expand` | — | — | ❌ |
| 11 | `/help` | — | crates/xaft-tui `SlashCommand::Help` | ✅ |
| 12 | `/history` | — | — | ❌ |
| 13 | `/init` | — | crates/xaft-tui `SlashCommand::Init` | ✅ |
| 14 | `/mcp` | — | crates/xaft-tui `SlashCommand::Mcp` | ✅ |
| 15 | `/model` | — | crates/xaft-tui `SlashCommand::Model` | ✅ |
| 16 | `/models` | — | crates/xaft-tui `SlashCommand::Model` (single command, no list mode) | 🟡 |
| 17 | `/skills` | — | — | ❌ |
| 18 | `/status` | — | — | ❌ |
| 19 | `/startup` | — | — | ❌ |
| 20 | `/ps` | `/processes` | — | ❌ |
| 21 | `/workers` | — | crates/xaft-tui `SlashCommand::Agents` | 🟡 |
| 22 | `/stop` | `/stop-terminal` | — | ❌ |
| 23 | `/usage` | — | crates/xaft-tui `SlashCommand::Cost` | ✅ |
| 24 | `/mode` | — | crates/xaft-tui `SlashCommand::Mode` | ✅ |
| 25 | `/workflow` | — | — | ❌ |
| 26 | `/compact` | — | crates/xaft-tui `SlashCommand::Compact` | ✅ |

**Subtotal: 26 rows.** ❌ 15 · 🟡 2 · ✅ 9

### 2a. xaft-only slash commands (no agenthicc counterpart)

These are supersets, not gaps — they do not block parity but are recorded for completeness.

- `/diff`
- `/commit`
- `/pr`
- `/rewind`
- `/permissions`
- `/theme`
- `/vim`
- `/emacs`
- `/doctor`
- `/memory`
- `/agents`
- `/login`
- `/logout`
- `/quit`

---

## 3. Modes

> **agenthicc source:** `src/agenthicc/modes/builtin.py`, `modes/registry.py`,
> `docs/guides/modes.md`.

agenthicc cycles **Safe → Plan → Yolo → Safe** with alias mapping. xaft defines **6** modes.
See `knowledge/tui-parity-gap-analysis.md` **Gap 2**.

| # | agenthicc mode | alias | agenthicc semantics | xaft location | Status |
|---|---|---|---|---|---|
| 1 | `Safe` | — | Side-effecting actions require explicit approval. | crates/xaft-tui mode `safe` | ✅ |
| 2 | `Plan` | — | Read-only planning; side effects hard-blocked. | crates/xaft-tui mode `plan` | ✅ |
| 3 | `Yolo` | — | Full automatic — all tools allowed, no prompt. | crates/xaft-tui mode `auto` | 🟡 |
| 4 | `Yolo` | alias `auto` | Auto is the accepted alias for Yolo. | crates/xaft-tui mode `auto` | ✅ |
| 5 | `Safe` | alias `ask` / `guard` | Ask and Guard are accepted aliases for Safe. | crates/xaft-tui modes `ask`; `guard` **not accepted** | 🟡 |
| 6 | `Plan` | alias `review` | Review is the accepted alias for Plan. | crates/xaft-tui mode `review` | ✅ |
| 7 | `—` | `debug` rejected | agenthicc rejects `debug` — it is not an alias. | crates/xaft-tui mode `debug` **accepted** | ❌ |

**Subtotal: 7 rows.** ❌ 1 · 🟡 2 · ✅ 4

---

## 4. TUI triggers

> **agenthicc source:** `src/agenthicc/tui/triggers/slash_command.py`,
> `src/agenthicc/tui/triggers/at_mention.py`.

| # | trigger | agenthicc class | xaft handler | Status |
|---|---|---|---|---|
| 1 | `/` | `SlashCommandTrigger` — Slash palette | crates/xaft-tui `SlashCommandTriggerHandler` | ✅ |
| 2 | `@` | `AtMentionTrigger` — File/dir mention picker | crates/xaft-tui `MentionTriggerHandler` | ✅ |
| 3 | `$` | `SkillTrigger` — Skill-only picker | crates/xaft-tui `SkillTriggerHandler` | ✅ |

**Subtotal: 3 rows.** ✅ 3

> Note: xaft's `xaft-tui` module doc also advertises a `#` history trigger, but no
> `#` handler is registered in `trigger/mod.rs`. agenthicc has no `#` trigger either, so
> this is a **documentation defect in xaft**, recorded as a follow-up rather than a
> parity gap.

---

## 5. Keybindings

> **agenthicc source:** `src/agenthicc/tui/input/capabilities.py`,
> `src/agenthicc/tui/workspace/overlays/*`, `docs/guides/tui.md`.

| # | key | agenthicc action | xaft location | Status |
|---|---|---|---|---|
| 1 | `Shift+Tab` | Cycle mode Safe → Plan → Yolo | crates/xaft-tui `state.rs` | 🟡 |
| 2 | `Ctrl+C` | Cancel current intent | crates/xaft-tui input | ✅ |
| 3 | `Esc` | Pause / back out of overlay | crates/xaft-tui input | ✅ |
| 4 | `Ctrl+V` | Reveal collapsed paste placeholder | crates/xaft-tui input bar | ✅ |
| 5 | `Enter` | Submit / select in overlay | crates/xaft-tui input | ✅ |
| 6 | `Up`/`Down` | Navigate picker list | crates/xaft-tui palette | ✅ |
| 7 | `Home`/`End` | Move to start/end of visible line | crates/xaft-tui input bar | ✅ |
| 8 | `PageUp`/`PageDown` | Scroll transcript | crates/xaft-tui transcript | ✅ |
| 9 | `k`/`j` | Vim-style up/down | crates/xaft-tui slash `/vim` | ✅ |
| 10 | `y`/`n` | Confirm / deny in overlays | crates/xaft-tui overlays | ✅ |
| 11 | `s` | Suspend in terminal overlay | — | ❌ |
| 12 | `?` | Toggle help overlay | crates/xaft-tui help overlay | ✅ |

**Subtotal: 12 rows.** ❌ 1 · 🟡 1 · ✅ 10

---

## 6. Built-in tools

agenthicc exposes **78** built-in tools across 8
packages. xaft implements **38** (`TOOL_NAME` consts in `crates/xaft-tools`).

### 6.6 `tools/fs/agent_tools.py` (24 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 1 | `append_file` | `tools/fs/agent_tools.py` | `append_to_file` (renamed) | ✅ |
| 2 | `apply_diff` | `tools/fs/agent_tools.py` | `patch_file` (renamed) | ✅ |
| 3 | `batch_copy` | `tools/fs/agent_tools.py` | — | ❌ |
| 4 | `batch_delete` | `tools/fs/agent_tools.py` | — | ❌ |
| 5 | `batch_move` | `tools/fs/agent_tools.py` | — | ❌ |
| 6 | `batch_read` | `tools/fs/agent_tools.py` | — | ❌ |
| 7 | `batch_write` | `tools/fs/agent_tools.py` | — | ❌ |
| 8 | `checksum_file` | `tools/fs/agent_tools.py` | — | ❌ |
| 9 | `copy_file` | `tools/fs/agent_tools.py` | `copy_file` | ✅ |
| 10 | `delete_file` | `tools/fs/agent_tools.py` | `delete_file` | ✅ |
| 11 | `file_exists` | `tools/fs/agent_tools.py` | `file_stat` (partial) | 🟡 |
| 12 | `get_file_info` | `tools/fs/agent_tools.py` | `file_stat` (renamed) | ✅ |
| 13 | `grep_file` | `tools/fs/agent_tools.py` | `grep` (renamed) | ✅ |
| 14 | `grep_files` | `tools/fs/agent_tools.py` | `search_files` (renamed) | ✅ |
| 15 | `list_directory` | `tools/fs/agent_tools.py` | `list_files` (renamed) | ✅ |
| 16 | `make_directory` | `tools/fs/agent_tools.py` | `create_directory` (renamed) | ✅ |
| 17 | `move_file` | `tools/fs/agent_tools.py` | `move_file` | ✅ |
| 18 | `patch_file` | `tools/fs/agent_tools.py` | `patch_file` | ✅ |
| 19 | `read_file` | `tools/fs/agent_tools.py` | `read_file` | ✅ |
| 20 | `read_lines` | `tools/fs/agent_tools.py` | `read_many`, `read_file` (partial) | 🟡 |
| 21 | `search_files` | `tools/fs/agent_tools.py` | `search_files` | ✅ |
| 22 | `touch_file` | `tools/fs/agent_tools.py` | — | ❌ |
| 23 | `truncate_file` | `tools/fs/agent_tools.py` | — | ❌ |
| 24 | `write_file` | `tools/fs/agent_tools.py` | `write_file` | ✅ |

### 6.7 `tools/git/agent_tools.py` (11 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 25 | `git_add` | `tools/git/agent_tools.py` | `git_add` | ✅ |
| 26 | `git_blame` | `tools/git/agent_tools.py` | `git_blame` | ✅ |
| 27 | `git_branch` | `tools/git/agent_tools.py` | `git_branch` | ✅ |
| 28 | `git_checkout` | `tools/git/agent_tools.py` | `git_checkout_files` (renamed) | ✅ |
| 29 | `git_commit` | `tools/git/agent_tools.py` | `git_commit_staged` (renamed) | ✅ |
| 30 | `git_diff` | `tools/git/agent_tools.py` | `git_diff` | ✅ |
| 31 | `git_grep` | `tools/git/agent_tools.py` | `git_grep` | ✅ |
| 32 | `git_log` | `tools/git/agent_tools.py` | `git_log` | ✅ |
| 33 | `git_show` | `tools/git/agent_tools.py` | `git_show` | ✅ |
| 34 | `git_stash` | `tools/git/agent_tools.py` | `git_stash` | ✅ |
| 35 | `git_status` | `tools/git/agent_tools.py` | `git_status` | ✅ |

### 6.8 `tools/exec/agent_tools.py` (10 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 36 | `inspect_terminal` | `tools/exec/agent_tools.py` | — | ❌ |
| 37 | `run_bash` | `tools/exec/agent_tools.py` | `bash_exec` (renamed) | ✅ |
| 38 | `run_command` | `tools/exec/agent_tools.py` | — | ❌ |
| 39 | `run_python` | `tools/exec/agent_tools.py` | — | ❌ |
| 40 | `run_python_expr` | `tools/exec/agent_tools.py` | — | ❌ |
| 41 | `run_tests` | `tools/exec/agent_tools.py` | — | ❌ |
| 42 | `shell` | `tools/exec/agent_tools.py` | `bash_exec` (renamed) | ✅ |
| 43 | `stop_terminal` | `tools/exec/agent_tools.py` | — | ❌ |
| 44 | `wait_terminal` | `tools/exec/agent_tools.py` | — | ❌ |
| 45 | `wait_terminal_ready` | `tools/exec/agent_tools.py` | — | ❌ |

### 6.9 `tools/introspect/agent_tools.py` (5 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 46 | `inspect_agenthicc_source` | `tools/introspect/agent_tools.py` | — | ❌ |
| 47 | `list_agenthicc_docs` | `tools/introspect/agent_tools.py` | `list_files` (partial) | 🟡 |
| 48 | `read_agenthicc_doc` | `tools/introspect/agent_tools.py` | `read_many`, `read_file` (partial) | 🟡 |
| 49 | `search_agenthicc_docs` | `tools/introspect/agent_tools.py` | `search_files` (partial) | 🟡 |
| 50 | `search_agenthicc_source` | `tools/introspect/agent_tools.py` | `search_files` (partial) | 🟡 |

### 6.10 `tools/outlook/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 51 | `calendar_events` | `tools/outlook/agent_tools.py` | — | ❌ |
| 52 | `create_event` | `tools/outlook/agent_tools.py` | `create_directory` (partial) | 🟡 |
| 53 | `list_emails` | `tools/outlook/agent_tools.py` | `list_files` (partial) | 🟡 |
| 54 | `list_folders` | `tools/outlook/agent_tools.py` | `list_files` (partial) | 🟡 |
| 55 | `move_email` | `tools/outlook/agent_tools.py` | `move_file` (partial) | 🟡 |
| 56 | `read_email` | `tools/outlook/agent_tools.py` | `read_many`, `read_file` (partial) | 🟡 |
| 57 | `reply_email` | `tools/outlook/agent_tools.py` | — | ❌ |
| 58 | `search_emails` | `tools/outlook/agent_tools.py` | `search_files` (partial) | 🟡 |
| 59 | `send_email` | `tools/outlook/agent_tools.py` | — | ❌ |

### 6.11 `tools/playwright/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 60 | `playwright_status` | `tools/playwright/agent_tools.py` | — | ❌ |
| 61 | `playwright_open` | `tools/playwright/agent_tools.py` | — | ❌ |
| 62 | `playwright_snapshot` | `tools/playwright/agent_tools.py` | — | ❌ |
| 63 | `playwright_click` | `tools/playwright/agent_tools.py` | — | ❌ |
| 64 | `playwright_fill` | `tools/playwright/agent_tools.py` | — | ❌ |
| 65 | `playwright_press` | `tools/playwright/agent_tools.py` | — | ❌ |
| 66 | `playwright_wait_for` | `tools/playwright/agent_tools.py` | — | ❌ |
| 67 | `playwright_screenshot` | `tools/playwright/agent_tools.py` | — | ❌ |
| 68 | `playwright_close` | `tools/playwright/agent_tools.py` | — | ❌ |

### 6.12 `tools/cloakbrowser/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 69 | `cloakbrowser_status` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 70 | `cloakbrowser_open` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 71 | `cloakbrowser_snapshot` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 72 | `cloakbrowser_click` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 73 | `cloakbrowser_fill` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 74 | `cloakbrowser_press` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 75 | `cloakbrowser_wait_for` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 76 | `cloakbrowser_screenshot` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 77 | `cloakbrowser_close` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |

### 6.13 `agent_tools.py` (1 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 78 | `mcp_connection_guide` | `agent_tools.py` | — | ❌ |

**Subtotal: 78 rows.** ❌ 39 · 🟡 12 · ✅ 27

> **Largest tool gaps:** the whole `outlook` package (9 mail/calendar tools), the whole
> `introspect` package (5 self-documentation tools), the whole `playwright` and
> `cloakbrowser` browser-automation packages (18 tools), and the batching helpers
> (`batch_read`/`write`/`copy`/`move`/`delete`). xaft has no browser automation and no
> email tool surface.

---

## 7. Configuration keys

agenthicc's `config.py` defines **185** fields across **20** classes, rendered
into `xaft.toml` by `config_template.py`. xaft defines **13** top-level sections.

### `SecretReference` (1 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 1 | `env` | `config.py::SecretReference` | `core` | 🟡 |

### `RequestOptionSettings` (7 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 2 | `extra_headers` | `config.py::RequestOptionSettings` | `provider` | 🟡 |
| 3 | `extra_query` | `config.py::RequestOptionSettings` | `provider` | 🟡 |
| 4 | `extra_body` | `config.py::RequestOptionSettings` | `provider` | 🟡 |
| 5 | `provider` | `config.py::RequestOptionSettings` | `provider` | 🟡 |
| 6 | `timeout_s` | `config.py::RequestOptionSettings` | `provider` | 🟡 |
| 7 | `max_retries` | `config.py::RequestOptionSettings` | `provider` | 🟡 |
| 8 | `include_raw_response` | `config.py::RequestOptionSettings` | `provider` | 🟡 |

### `ProviderProfile` (18 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 9 | `name` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 10 | `provider` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 11 | `model` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 12 | `base_url` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 13 | `api_key` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 14 | `api_key_env` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 15 | `default_headers` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 16 | `default_query` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 17 | `client_options` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 18 | `request_options` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 19 | `timeout_s` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 20 | `max_retries` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 21 | `temperature` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 22 | `top_p` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 23 | `max_completion_tokens` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 24 | `protocol` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 25 | `capabilities` | `config.py::ProviderProfile` | `provider` | 🟡 |
| 26 | `session_header` | `config.py::ProviderProfile` | `provider` | 🟡 |

### `ResolvedProviderProfile` (17 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 27 | `name` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 28 | `provider` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 29 | `model` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 30 | `base_url` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 31 | `api_key` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 32 | `default_headers` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 33 | `default_query` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 34 | `client_options` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 35 | `request_options` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 36 | `timeout_s` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 37 | `max_retries` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 38 | `temperature` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 39 | `top_p` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 40 | `max_completion_tokens` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 41 | `protocol` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 42 | `capabilities` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |
| 43 | `session_header` | `config.py::ResolvedProviderProfile` | `provider (resolved; internal)` | 🟡 |

### `ExecutionSettings` (34 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 44 | `max_concurrent_intents` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 45 | `max_parallel_tasks` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 46 | `agent_pool_size` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 47 | `max_agent_turns` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 48 | `authoring_max_generation_attempts` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 49 | `authoring_max_phase_turns` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 50 | `max_output_tokens` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 51 | `turn_timeout_s` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 52 | `auto_compact` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 53 | `context_windows` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 54 | `prompt_cache` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 55 | `file_cache` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 56 | `transport_max_retries` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 57 | `transport_retry_base_delay_s` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 58 | `transport_retry_max_total_s` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 59 | `llm_sdk_max_retries` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 60 | `profile` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 61 | `provider` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 62 | `model` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 63 | `api_key` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 64 | `api_key_env` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 65 | `base_url` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 66 | `default_headers` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 67 | `default_query` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 68 | `client_options` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 69 | `request_options` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 70 | `timeout_s` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 71 | `temperature` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 72 | `top_p` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 73 | `max_completion_tokens` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 74 | `provider_capabilities` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 75 | `_resolved_profile` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 76 | `session_header` | `config.py::ExecutionSettings` | `provider` | 🟡 |
| 77 | `irrecoverable_error_max_retries` | `config.py::ExecutionSettings` | `provider` | 🟡 |

### `CloakBrowserSettings` (15 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 78 | `enabled` | `config.py::CloakBrowserSettings` | — | ❌ |
| 79 | `transport` | `config.py::CloakBrowserSettings` | — | ❌ |
| 80 | `cdp_endpoint` | `config.py::CloakBrowserSettings` | — | ❌ |
| 81 | `allowed_domains` | `config.py::CloakBrowserSettings` | — | ❌ |
| 82 | `headless` | `config.py::CloakBrowserSettings` | — | ❌ |
| 83 | `navigation_timeout_s` | `config.py::CloakBrowserSettings` | — | ❌ |
| 84 | `action_timeout_s` | `config.py::CloakBrowserSettings` | — | ❌ |
| 85 | `max_pages` | `config.py::CloakBrowserSettings` | — | ❌ |
| 86 | `max_actions_per_turn` | `config.py::CloakBrowserSettings` | — | ❌ |
| 87 | `max_snapshot_chars` | `config.py::CloakBrowserSettings` | — | ❌ |
| 88 | `max_screenshot_bytes` | `config.py::CloakBrowserSettings` | — | ❌ |
| 89 | `allow_persistent_profiles` | `config.py::CloakBrowserSettings` | — | ❌ |
| 90 | `profile_root` | `config.py::CloakBrowserSettings` | — | ❌ |
| 91 | `license_key_env` | `config.py::CloakBrowserSettings` | — | ❌ |
| 92 | `allow_all_domains` | `config.py::CloakBrowserSettings` | — | ❌ |

### `PlaywrightSettings` (16 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 93 | `enabled` | `config.py::PlaywrightSettings` | — | ❌ |
| 94 | `transport` | `config.py::PlaywrightSettings` | — | ❌ |
| 95 | `browser_type` | `config.py::PlaywrightSettings` | — | ❌ |
| 96 | `browser_channel` | `config.py::PlaywrightSettings` | — | ❌ |
| 97 | `executable_path` | `config.py::PlaywrightSettings` | — | ❌ |
| 98 | `allowed_domains` | `config.py::PlaywrightSettings` | — | ❌ |
| 99 | `headless` | `config.py::PlaywrightSettings` | — | ❌ |
| 100 | `navigation_timeout_s` | `config.py::PlaywrightSettings` | — | ❌ |
| 101 | `action_timeout_s` | `config.py::PlaywrightSettings` | — | ❌ |
| 102 | `max_pages` | `config.py::PlaywrightSettings` | — | ❌ |
| 103 | `max_actions_per_turn` | `config.py::PlaywrightSettings` | — | ❌ |
| 104 | `max_snapshot_chars` | `config.py::PlaywrightSettings` | — | ❌ |
| 105 | `max_screenshot_bytes` | `config.py::PlaywrightSettings` | — | ❌ |
| 106 | `allow_persistent_profiles` | `config.py::PlaywrightSettings` | — | ❌ |
| 107 | `profile_root` | `config.py::PlaywrightSettings` | — | ❌ |
| 108 | `allow_all_domains` | `config.py::PlaywrightSettings` | — | ❌ |

### `ToolSettings` (11 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 109 | `mcp_servers` | `config.py::ToolSettings` | `tool` | 🟡 |
| 110 | `plugins` | `config.py::ToolSettings` | `tool` | 🟡 |
| 111 | `allowed` | `config.py::ToolSettings` | `tool` | 🟡 |
| 112 | `denied` | `config.py::ToolSettings` | `tool` | 🟡 |
| 113 | `max_live_tool_calls` | `config.py::ToolSettings` | `tool` | 🟡 |
| 114 | `group_exploratory_calls` | `config.py::ToolSettings` | `tool` | 🟡 |
| 115 | `http_timeout_s` | `config.py::ToolSettings` | `tool` | 🟡 |
| 116 | `question_timeout_s` | `config.py::ToolSettings` | `tool` | 🟡 |
| 117 | `cloakbrowser` | `config.py::ToolSettings` | `tool` | 🟡 |
| 118 | `playwright` | `config.py::ToolSettings` | `tool` | 🟡 |
| 119 | `browser_backend` | `config.py::ToolSettings` | `tool` | 🟡 |

### `MemorySettings` (3 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 120 | `project_memory_path` | `config.py::MemorySettings` | `memory` | 🟡 |
| 121 | `vector_db` | `config.py::MemorySettings` | `memory` | 🟡 |
| 122 | `session_ttl_seconds` | `config.py::MemorySettings` | `memory` | 🟡 |

### `SecuritySettings` (5 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 123 | `sandbox_mode` | `config.py::SecuritySettings` | `guardrail` | 🟡 |
| 124 | `allowed_paths` | `config.py::SecuritySettings` | `guardrail` | 🟡 |
| 125 | `network_allow_list` | `config.py::SecuritySettings` | `guardrail` | 🟡 |
| 126 | `max_tool_cpu_seconds` | `config.py::SecuritySettings` | `guardrail` | 🟡 |
| 127 | `max_tool_memory_mb` | `config.py::SecuritySettings` | `guardrail` | 🟡 |

### `ApiSettings` (3 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 128 | `host` | `config.py::ApiSettings` | — | ❌ |
| 129 | `port` | `config.py::ApiSettings` | — | ❌ |
| 130 | `api_key_env` | `config.py::ApiSettings` | — | ❌ |

### `PluginSettings` (9 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 131 | `auto_trust` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 132 | `auto_install` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 133 | `install_target` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 134 | `allowed_modules` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 135 | `timeout_seconds` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 136 | `disabled` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 137 | `trust_file` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 138 | `audit_file` | `config.py::PluginSettings` | `plugins` | 🟡 |
| 139 | `strict_cli_shadow` | `config.py::PluginSettings` | `plugins` | 🟡 |

### `BehaviourSettings` (3 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 140 | `verbose` | `config.py::BehaviourSettings` | `core` | 🟡 |
| 141 | `confirm_exits` | `config.py::BehaviourSettings` | `core` | 🟡 |
| 142 | `resume_transcript_turns` | `config.py::BehaviourSettings` | `core` | 🟡 |

### `LoopSettings` (10 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 143 | `enabled` | `config.py::LoopSettings` | — | ❌ |
| 144 | `default_interval_s` | `config.py::LoopSettings` | — | ❌ |
| 145 | `min_interval_s` | `config.py::LoopSettings` | — | ❌ |
| 146 | `max_interval_s` | `config.py::LoopSettings` | — | ❌ |
| 147 | `max_age_s` | `config.py::LoopSettings` | — | ❌ |
| 148 | `max_prompt_bytes` | `config.py::LoopSettings` | — | ❌ |
| 149 | `max_consecutive_failures` | `config.py::LoopSettings` | — | ❌ |
| 150 | `busy_poll_s` | `config.py::LoopSettings` | — | ❌ |
| 151 | `persist` | `config.py::LoopSettings` | — | ❌ |
| 152 | `allow_slash_commands` | `config.py::LoopSettings` | — | ❌ |

### `AgentSettings` (5 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 153 | `description` | `config.py::AgentSettings` | `agent` | 🟡 |
| 154 | `model` | `config.py::AgentSettings` | `agent` | 🟡 |
| 155 | `max_turns` | `config.py::AgentSettings` | `agent` | 🟡 |
| 156 | `allowed_skills` | `config.py::AgentSettings` | `agent` | 🟡 |
| 157 | `denied_skills` | `config.py::AgentSettings` | `agent` | 🟡 |

### `AgentsSettings` (1 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 158 | `agents` | `config.py::AgentsSettings` | `agent` | 🟡 |

### `SkillsSettings` (2 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 159 | `install_default_skills` | `config.py::SkillsSettings` | — | ❌ |
| 160 | `default_skill_directory` | `config.py::SkillsSettings` | — | ❌ |

### `StorageS3Settings` (9 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 161 | `bucket` | `config.py::StorageS3Settings` | — | ❌ |
| 162 | `region` | `config.py::StorageS3Settings` | — | ❌ |
| 163 | `prefix` | `config.py::StorageS3Settings` | — | ❌ |
| 164 | `access_key_id` | `config.py::StorageS3Settings` | — | ❌ |
| 165 | `secret_access_key` | `config.py::StorageS3Settings` | — | ❌ |
| 166 | `endpoint_url` | `config.py::StorageS3Settings` | — | ❌ |
| 167 | `profile` | `config.py::StorageS3Settings` | — | ❌ |
| 168 | `path_style` | `config.py::StorageS3Settings` | — | ❌ |
| 169 | `mounts` | `config.py::StorageS3Settings` | — | ❌ |

### `StorageSettings` (2 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 170 | `s3` | `config.py::StorageSettings` | — | ❌ |
| 171 | `default_backend` | `config.py::StorageSettings` | — | ❌ |

### `AgenthiccConfig` (14 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 172 | `execution` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 173 | `providers` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 174 | `behaviour` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 175 | `hooks` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 176 | `tools` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 177 | `memory` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 178 | `security` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 179 | `api` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 180 | `plugins` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 181 | `skills` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 182 | `agents` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 183 | `storage` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 184 | `workflows` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |
| 185 | `loops` | `config.py::AgenthiccConfig` | `(root)` | 🟡 |

**Subtotal: 185 rows.** ❌ 57 · 🟡 128 · ✅ 0

> Config rows are 🟡 even where an xaft section exists, because per-key names and
> defaults are xaft-specific and have not yet been diffed field-by-field. Section-level
> presence is proven; per-key semantic equivalence is the work of **G4**, which must
> downgrade any genuinely-missing key to ❌.

---

## Summary

| Section | agenthicc entries | xaft entries | ✅ | 🟡 | ❌ |
|---|---|---|---|---|---|
| 1. CLI commands | 59 | 12 | 23 | 9 | 28 |
| 2. Slash commands | 26 | 24 | 9 | 2 | 15 |
| 3. Modes | 3 | 6 | 4 | 2 | 1 |
| 4. TUI triggers | 3 | 3 | 3 | 0 | 0 |
| 5. Keybindings | 12 | 12 | 10 | 1 | 1 |
| 6. Built-in tools | 77 | 38 | 27 | 12 | 39 |
| 7. Configuration keys | 185 | 13 | 0 | 128 | 57 |
| **TOTAL** | **365** | **108** | **76** | **154** | **141** |

Current parity: **76** of 371 rows ✅ (20%). The remaining
**295** rows are the backlog for goals G4–G10.

---

## Related documents

- `knowledge/tui-parity-gap-analysis.md` — narrative TUI gap analysis (gaps 1–11)
- `docs/guides/modes.md`, `docs/guides/tui.md`, `docs/guides/configuration.md` — xaft guides
- agenthicc `docs/guides/*.md` and `docs/usage/` — reference documentation
