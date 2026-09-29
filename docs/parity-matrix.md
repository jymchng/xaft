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
| 18 | `jobs list` | `cli/commands/background.py` | — | ❌ |
| 19 | `jobs status` | `cli/commands/background.py` | — | ❌ |
| 20 | `jobs cancel` | `cli/commands/background.py` | crates/xaft-cli `SessionSubcommand::Cancel` (session-scoped, not job-scoped) | 🟡 |
| 21 | `jobs approve` | `cli/commands/background.py` | — | ❌ |
| 22 | `jobs reject` | `cli/commands/background.py` | — | ❌ |
| 23 | `jobs input` | `cli/commands/background.py` | — | ❌ |
| 24 | `jobs labels` | `cli/commands/background.py` | — | ❌ |
| 25 | `jobs rename` | `cli/commands/background.py` | — | ❌ |
| 26 | `jobs retry` | `cli/commands/background.py` | — | ❌ |
| 27 | `jobs resume` | `cli/commands/background.py` | — | ❌ |
| 28 | `jobs archive` | `cli/commands/background.py` | — | ❌ |
| 29 | `jobs restore` | `cli/commands/background.py` | — | ❌ |
| 30 | `jobs delete` | `cli/commands/background.py` | — | ❌ |
| 31 | `jobs purge` | `cli/commands/background.py` | — | ❌ |
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

**Subtotal: 60 rows.** ❌ 41 · 🟡 10 · ✅ 9

---

## 2. Slash commands

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

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 1 | `append_file` | `append_to_file` (renamed) | ✅ |
| 2 | `apply_diff` | `patch_file` (renamed) | ✅ |
| 3 | `batch_copy` | — | ❌ |
| 4 | `batch_delete` | — | ❌ |
| 5 | `batch_move` | — | ❌ |
| 6 | `batch_read` | — | ❌ |
| 7 | `batch_write` | — | ❌ |
| 8 | `checksum_file` | — | ❌ |
| 9 | `copy_file` | `copy_file` | ✅ |
| 10 | `delete_file` | `delete_file` | ✅ |
| 11 | `file_exists` | `file_stat` (partial) | 🟡 |
| 12 | `get_file_info` | `file_stat` (renamed) | ✅ |
| 13 | `grep_file` | `grep` (renamed) | ✅ |
| 14 | `grep_files` | `search_files` (renamed) | ✅ |
| 15 | `list_directory` | `list_files` (renamed) | ✅ |
| 16 | `make_directory` | `create_directory` (renamed) | ✅ |
| 17 | `move_file` | `move_file` | ✅ |
| 18 | `patch_file` | `patch_file` | ✅ |
| 19 | `read_file` | `read_file` | ✅ |
| 20 | `read_lines` | `read_many`, `read_file` (partial) | 🟡 |
| 21 | `search_files` | `search_files` | ✅ |
| 22 | `touch_file` | — | ❌ |
| 23 | `truncate_file` | — | ❌ |
| 24 | `write_file` | `write_file` | ✅ |

### 6.7 `tools/git/agent_tools.py` (11 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 25 | `git_add` | `git_add` | ✅ |
| 26 | `git_blame` | `git_blame` | ✅ |
| 27 | `git_branch` | `git_branch` | ✅ |
| 28 | `git_checkout` | `git_checkout_files` (renamed) | ✅ |
| 29 | `git_commit` | `git_commit_staged` (renamed) | ✅ |
| 30 | `git_diff` | `git_diff` | ✅ |
| 31 | `git_grep` | `git_grep` | ✅ |
| 32 | `git_log` | `git_log` | ✅ |
| 33 | `git_show` | `git_show` | ✅ |
| 34 | `git_stash` | `git_stash` | ✅ |
| 35 | `git_status` | `git_status` | ✅ |

### 6.8 `tools/exec/agent_tools.py` (10 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 36 | `inspect_terminal` | — | ❌ |
| 37 | `run_bash` | `bash_exec` (renamed) | ✅ |
| 38 | `run_command` | — | ❌ |
| 39 | `run_python` | — | ❌ |
| 40 | `run_python_expr` | — | ❌ |
| 41 | `run_tests` | — | ❌ |
| 42 | `shell` | `bash_exec` (renamed) | ✅ |
| 43 | `stop_terminal` | — | ❌ |
| 44 | `wait_terminal` | — | ❌ |
| 45 | `wait_terminal_ready` | — | ❌ |

### 6.9 `tools/introspect/agent_tools.py` (5 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 46 | `inspect_agenthicc_source` | — | ❌ |
| 47 | `list_agenthicc_docs` | `list_files` (partial) | 🟡 |
| 48 | `read_agenthicc_doc` | `read_many`, `read_file` (partial) | 🟡 |
| 49 | `search_agenthicc_docs` | `search_files` (partial) | 🟡 |
| 50 | `search_agenthicc_source` | `search_files` (partial) | 🟡 |

### 6.10 `tools/outlook/agent_tools.py` (9 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 51 | `calendar_events` | — | ❌ |
| 52 | `create_event` | `create_directory` (partial) | 🟡 |
| 53 | `list_emails` | `list_files` (partial) | 🟡 |
| 54 | `list_folders` | `list_files` (partial) | 🟡 |
| 55 | `move_email` | `move_file` (partial) | 🟡 |
| 56 | `read_email` | `read_many`, `read_file` (partial) | 🟡 |
| 57 | `reply_email` | — | ❌ |
| 58 | `search_emails` | `search_files` (partial) | 🟡 |
| 59 | `send_email` | — | ❌ |

### 6.11 `tools/playwright/agent_tools.py` (9 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 60 | `playwright_status` | — | ❌ |
| 61 | `playwright_open` | — | ❌ |
| 62 | `playwright_snapshot` | — | ❌ |
| 63 | `playwright_click` | — | ❌ |
| 64 | `playwright_fill` | — | ❌ |
| 65 | `playwright_press` | — | ❌ |
| 66 | `playwright_wait_for` | — | ❌ |
| 67 | `playwright_screenshot` | — | ❌ |
| 68 | `playwright_close` | — | ❌ |

### 6.12 `tools/cloakbrowser/agent_tools.py` (9 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 69 | `cloakbrowser_status` | — | ❌ |
| 70 | `cloakbrowser_open` | — | ❌ |
| 71 | `cloakbrowser_snapshot` | — | ❌ |
| 72 | `cloakbrowser_click` | — | ❌ |
| 73 | `cloakbrowser_fill` | — | ❌ |
| 74 | `cloakbrowser_press` | — | ❌ |
| 75 | `cloakbrowser_wait_for` | — | ❌ |
| 76 | `cloakbrowser_screenshot` | — | ❌ |
| 77 | `cloakbrowser_close` | — | ❌ |

### 6.13 `agent_tools.py` (1 tools)

| # | agenthicc tool | xaft tool | Status |
|---|---|---|---|
| 78 | `mcp_connection_guide` | — | ❌ |

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

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 1 | `env` | `core` | 🟡 |

### `RequestOptionSettings` (7 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 2 | `extra_headers` | `provider` | 🟡 |
| 3 | `extra_query` | `provider` | 🟡 |
| 4 | `extra_body` | `provider` | 🟡 |
| 5 | `provider` | `provider` | 🟡 |
| 6 | `timeout_s` | `provider` | 🟡 |
| 7 | `max_retries` | `provider` | 🟡 |
| 8 | `include_raw_response` | `provider` | 🟡 |

### `ProviderProfile` (18 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 9 | `name` | `provider` | 🟡 |
| 10 | `provider` | `provider` | 🟡 |
| 11 | `model` | `provider` | 🟡 |
| 12 | `base_url` | `provider` | 🟡 |
| 13 | `api_key` | `provider` | 🟡 |
| 14 | `api_key_env` | `provider` | 🟡 |
| 15 | `default_headers` | `provider` | 🟡 |
| 16 | `default_query` | `provider` | 🟡 |
| 17 | `client_options` | `provider` | 🟡 |
| 18 | `request_options` | `provider` | 🟡 |
| 19 | `timeout_s` | `provider` | 🟡 |
| 20 | `max_retries` | `provider` | 🟡 |
| 21 | `temperature` | `provider` | 🟡 |
| 22 | `top_p` | `provider` | 🟡 |
| 23 | `max_completion_tokens` | `provider` | 🟡 |
| 24 | `protocol` | `provider` | 🟡 |
| 25 | `capabilities` | `provider` | 🟡 |
| 26 | `session_header` | `provider` | 🟡 |

### `ResolvedProviderProfile` (17 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 27 | `name` | — | ❌ |
| 28 | `provider` | — | ❌ |
| 29 | `model` | — | ❌ |
| 30 | `base_url` | — | ❌ |
| 31 | `api_key` | — | ❌ |
| 32 | `default_headers` | — | ❌ |
| 33 | `default_query` | — | ❌ |
| 34 | `client_options` | — | ❌ |
| 35 | `request_options` | — | ❌ |
| 36 | `timeout_s` | — | ❌ |
| 37 | `max_retries` | — | ❌ |
| 38 | `temperature` | — | ❌ |
| 39 | `top_p` | — | ❌ |
| 40 | `max_completion_tokens` | — | ❌ |
| 41 | `protocol` | — | ❌ |
| 42 | `capabilities` | — | ❌ |
| 43 | `session_header` | — | ❌ |

### `ExecutionSettings` (34 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 44 | `max_concurrent_intents` | `provider` | 🟡 |
| 45 | `max_parallel_tasks` | `provider` | 🟡 |
| 46 | `agent_pool_size` | `provider` | 🟡 |
| 47 | `max_agent_turns` | `provider` | 🟡 |
| 48 | `authoring_max_generation_attempts` | `provider` | 🟡 |
| 49 | `authoring_max_phase_turns` | `provider` | 🟡 |
| 50 | `max_output_tokens` | `provider` | 🟡 |
| 51 | `turn_timeout_s` | `provider` | 🟡 |
| 52 | `auto_compact` | `provider` | 🟡 |
| 53 | `context_windows` | `provider` | 🟡 |
| 54 | `prompt_cache` | `provider` | 🟡 |
| 55 | `file_cache` | `provider` | 🟡 |
| 56 | `transport_max_retries` | `provider` | 🟡 |
| 57 | `transport_retry_base_delay_s` | `provider` | 🟡 |
| 58 | `transport_retry_max_total_s` | `provider` | 🟡 |
| 59 | `llm_sdk_max_retries` | `provider` | 🟡 |
| 60 | `profile` | `provider` | 🟡 |
| 61 | `provider` | `provider` | 🟡 |
| 62 | `model` | `provider` | 🟡 |
| 63 | `api_key` | `provider` | 🟡 |
| 64 | `api_key_env` | `provider` | 🟡 |
| 65 | `base_url` | `provider` | 🟡 |
| 66 | `default_headers` | `provider` | 🟡 |
| 67 | `default_query` | `provider` | 🟡 |
| 68 | `client_options` | `provider` | 🟡 |
| 69 | `request_options` | `provider` | 🟡 |
| 70 | `timeout_s` | `provider` | 🟡 |
| 71 | `temperature` | `provider` | 🟡 |
| 72 | `top_p` | `provider` | 🟡 |
| 73 | `max_completion_tokens` | `provider` | 🟡 |
| 74 | `provider_capabilities` | `provider` | 🟡 |
| 75 | `_resolved_profile` | `provider` | 🟡 |
| 76 | `session_header` | `provider` | 🟡 |
| 77 | `irrecoverable_error_max_retries` | `provider` | 🟡 |

### `CloakBrowserSettings` (15 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 78 | `enabled` | — | ❌ |
| 79 | `transport` | — | ❌ |
| 80 | `cdp_endpoint` | — | ❌ |
| 81 | `allowed_domains` | — | ❌ |
| 82 | `headless` | — | ❌ |
| 83 | `navigation_timeout_s` | — | ❌ |
| 84 | `action_timeout_s` | — | ❌ |
| 85 | `max_pages` | — | ❌ |
| 86 | `max_actions_per_turn` | — | ❌ |
| 87 | `max_snapshot_chars` | — | ❌ |
| 88 | `max_screenshot_bytes` | — | ❌ |
| 89 | `allow_persistent_profiles` | — | ❌ |
| 90 | `profile_root` | — | ❌ |
| 91 | `license_key_env` | — | ❌ |
| 92 | `allow_all_domains` | — | ❌ |

### `PlaywrightSettings` (16 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 93 | `enabled` | — | ❌ |
| 94 | `transport` | — | ❌ |
| 95 | `browser_type` | — | ❌ |
| 96 | `browser_channel` | — | ❌ |
| 97 | `executable_path` | — | ❌ |
| 98 | `allowed_domains` | — | ❌ |
| 99 | `headless` | — | ❌ |
| 100 | `navigation_timeout_s` | — | ❌ |
| 101 | `action_timeout_s` | — | ❌ |
| 102 | `max_pages` | — | ❌ |
| 103 | `max_actions_per_turn` | — | ❌ |
| 104 | `max_snapshot_chars` | — | ❌ |
| 105 | `max_screenshot_bytes` | — | ❌ |
| 106 | `allow_persistent_profiles` | — | ❌ |
| 107 | `profile_root` | — | ❌ |
| 108 | `allow_all_domains` | — | ❌ |

### `ToolSettings` (11 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 109 | `mcp_servers` | `tool` | 🟡 |
| 110 | `plugins` | `tool` | 🟡 |
| 111 | `allowed` | `tool` | 🟡 |
| 112 | `denied` | `tool` | 🟡 |
| 113 | `max_live_tool_calls` | `tool` | 🟡 |
| 114 | `group_exploratory_calls` | `tool` | 🟡 |
| 115 | `http_timeout_s` | `tool` | 🟡 |
| 116 | `question_timeout_s` | `tool` | 🟡 |
| 117 | `cloakbrowser` | `tool` | 🟡 |
| 118 | `playwright` | `tool` | 🟡 |
| 119 | `browser_backend` | `tool` | 🟡 |

### `MemorySettings` (3 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 120 | `project_memory_path` | `memory` | 🟡 |
| 121 | `vector_db` | `memory` | 🟡 |
| 122 | `session_ttl_seconds` | `memory` | 🟡 |

### `SecuritySettings` (5 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 123 | `sandbox_mode` | `guardrail` | 🟡 |
| 124 | `allowed_paths` | `guardrail` | 🟡 |
| 125 | `network_allow_list` | `guardrail` | 🟡 |
| 126 | `max_tool_cpu_seconds` | `guardrail` | 🟡 |
| 127 | `max_tool_memory_mb` | `guardrail` | 🟡 |

### `ApiSettings` (3 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 128 | `host` | — | ❌ |
| 129 | `port` | — | ❌ |
| 130 | `api_key_env` | — | ❌ |

### `PluginSettings` (9 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 131 | `auto_trust` | `plugins` | 🟡 |
| 132 | `auto_install` | `plugins` | 🟡 |
| 133 | `install_target` | `plugins` | 🟡 |
| 134 | `allowed_modules` | `plugins` | 🟡 |
| 135 | `timeout_seconds` | `plugins` | 🟡 |
| 136 | `disabled` | `plugins` | 🟡 |
| 137 | `trust_file` | `plugins` | 🟡 |
| 138 | `audit_file` | `plugins` | 🟡 |
| 139 | `strict_cli_shadow` | `plugins` | 🟡 |

### `BehaviourSettings` (3 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 140 | `verbose` | `core` | 🟡 |
| 141 | `confirm_exits` | `core` | 🟡 |
| 142 | `resume_transcript_turns` | `core` | 🟡 |

### `LoopSettings` (10 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 143 | `enabled` | — | ❌ |
| 144 | `default_interval_s` | — | ❌ |
| 145 | `min_interval_s` | — | ❌ |
| 146 | `max_interval_s` | — | ❌ |
| 147 | `max_age_s` | — | ❌ |
| 148 | `max_prompt_bytes` | — | ❌ |
| 149 | `max_consecutive_failures` | — | ❌ |
| 150 | `busy_poll_s` | — | ❌ |
| 151 | `persist` | — | ❌ |
| 152 | `allow_slash_commands` | — | ❌ |

### `AgentSettings` (5 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 153 | `description` | `agent` | 🟡 |
| 154 | `model` | `agent` | 🟡 |
| 155 | `max_turns` | `agent` | 🟡 |
| 156 | `allowed_skills` | `agent` | 🟡 |
| 157 | `denied_skills` | `agent` | 🟡 |

### `AgentsSettings` (1 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 158 | `agents` | `agent` | 🟡 |

### `SkillsSettings` (2 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 159 | `install_default_skills` | — | ❌ |
| 160 | `default_skill_directory` | — | ❌ |

### `StorageS3Settings` (9 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 161 | `bucket` | — | ❌ |
| 162 | `region` | — | ❌ |
| 163 | `prefix` | — | ❌ |
| 164 | `access_key_id` | — | ❌ |
| 165 | `secret_access_key` | — | ❌ |
| 166 | `endpoint_url` | — | ❌ |
| 167 | `profile` | — | ❌ |
| 168 | `path_style` | — | ❌ |
| 169 | `mounts` | — | ❌ |

### `StorageSettings` (2 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 170 | `s3` | — | ❌ |
| 171 | `default_backend` | — | ❌ |

### `AgenthiccConfig` (14 keys)

| # | agenthicc key | xaft section | Status |
|---|---|---|---|
| 172 | `execution` | `(root)` | 🟡 |
| 173 | `providers` | `(root)` | 🟡 |
| 174 | `behaviour` | `(root)` | 🟡 |
| 175 | `hooks` | `(root)` | 🟡 |
| 176 | `tools` | `(root)` | 🟡 |
| 177 | `memory` | `(root)` | 🟡 |
| 178 | `security` | `(root)` | 🟡 |
| 179 | `api` | `(root)` | 🟡 |
| 180 | `plugins` | `(root)` | 🟡 |
| 181 | `skills` | `(root)` | 🟡 |
| 182 | `agents` | `(root)` | 🟡 |
| 183 | `storage` | `(root)` | 🟡 |
| 184 | `workflows` | `(root)` | 🟡 |
| 185 | `loops` | `(root)` | 🟡 |

**Subtotal: 185 rows.** ❌ 74 · 🟡 111 · ✅ 0

> Config rows are 🟡 even where an xaft section exists, because per-key names and
> defaults are xaft-specific and have not yet been diffed field-by-field. Section-level
> presence is proven; per-key semantic equivalence is the work of **G4**, which must
> downgrade any genuinely-missing key to ❌.

---

## Summary

| Section | agenthicc entries | xaft entries | ✅ | 🟡 | ❌ |
|---|---|---|---|---|---|
| 1. CLI commands | 59 | 12 | 9 | 10 | 41 |
| 2. Slash commands | 26 | 24 | 9 | 2 | 15 |
| 3. Modes | 3 | 6 | 4 | 2 | 1 |
| 4. TUI triggers | 3 | 3 | 3 | 0 | 0 |
| 5. Keybindings | 12 | 12 | 10 | 1 | 1 |
| 6. Built-in tools | 77 | 38 | 27 | 12 | 39 |
| 7. Configuration keys | 185 | 13 | 0 | 111 | 74 |
| **TOTAL** | **365** | **108** | **62** | **138** | **171** |

Current parity: **62** of 371 rows ✅ (16%). The remaining
**309** rows are the backlog for goals G4–G10.

---

## Related documents

- `knowledge/tui-parity-gap-analysis.md` — narrative TUI gap analysis (gaps 1–11)
- `docs/guides/modes.md`, `docs/guides/tui.md`, `docs/guides/configuration.md` — xaft guides
- agenthicc `docs/guides/*.md` and `docs/usage/` — reference documentation
