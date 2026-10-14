# Claude-Code Parity Features PRD — Round 2

## Background

PRD 27 introduced 20 features covering the high-visibility gaps (slash commands, plan
mode, @-mentions, MCP, hooks, skills, subagents, compaction, forking, etc.). This PRD
addresses the **second wave**: deeper input UX, transcript rendering polish, project
browsing, OS-level safety, project-tooling integrations, and workflow reliability.

These features target the codebase as of PRDs 22–27. None re-implement work already
done; they extend the new modules from PRD 27 (slash command system, tool
registry, signal bus, TUI state) and add smaller new modules to existing crates.

The features are organised into six groups: **Input & Editing UX** (F21–F25),
**Transcript Rendering** (F26–F28), **Project Browsing** (F29), **Safety & Policy**
(F30–F33), **Tooling & Workflow** (F34–F37), and **Multi-Worktree + Compliance**
(F38–F40). Each feature is independent and shippable; together they round out the
Claude Code affordance surface.

---

## Goal

Add the 20 features described below. Each feature:

- Has a small, focused surface (one PRD section).
- Integrates with existing systems: `SignalBus`, `WorkflowConfig`, `ToolRegistry`,
  `MemoryManager`, `ConversationStore`, `XaftConfig`.
- Emits a signal for every user-visible transition (per `AGENTS.md`).
- Comes with unit + integration tests in the same style as `tests/explore_pool.rs`.
- Can be enabled / disabled independently via `XaftConfig`.

---

## High-Level Architecture

```text
┌──────────────────────────────────────────────────────────────────────────┐
│                          Input & Editing UX                              │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌──────────┐ │
│ │ Multi-line │ │    Tab     │ │   Hist     │ │   $EDITOR  │ │ DnD File │ │  F21-F25
│ └────────────┘ └────────────┘ └────────────┘ └────────────┘ └──────────┘ │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                        Transcript Rendering                              │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐                            │
│ │  Markdown  │ │ Syntax Hi  │ │  Find in   │                            │  F26-F28
│ └────────────┘ └────────────┘ └────────────┘                            │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                         Project Browsing                                 │
│ ┌────────────┐                                                       │
│ │ File Tree  │                                                       │  F29
│ └────────────┘                                                       │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                          Safety & Policy                                 │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐            │
│ │ OS Sandbox │ │ Network    │ │ FS Policy  │ │ KillSwitch │            │  F30-F33
│ └────────────┘ └────────────┘ └────────────┘ └────────────┘            │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                       Tooling & Workflow                                 │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐            │
│ │  Watch     │ │    Test    │ │   Lint     │ │ TypeCheck  │            │  F34-F37
│ └────────────┘ └────────────┘ └────────────┘ └────────────┘            │
└──────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────┐
│                   Multi-Worktree + Compliance                            │
│ ┌────────────┐ ┌────────────┐ ┌────────────┐                            │
│ │ Multi-WT   │ │ Audit+Repl │ │ Checkpoint │                            │  F38-F40
│ └────────────┘ └────────────┘ └────────────┘                            │
└──────────────────────────────────────────────────────────────────────────┘
```

---

# Part 1 — Input & Editing UX

## [Done] F21. Multi-line Input with Shift+Enter

### Goal
Allow newlines in the input bar so users can compose multi-line prompts, paste code
snippets, and write structured questions — all without leaving the input bar.

### Behaviour

- `Enter` sends the current buffer (existing behaviour).
- `Shift+Enter` (or `Alt+Enter` on terminals that don't distinguish) inserts a
  newline.
- `Ctrl+J` also inserts a newline (universal fallback).
- Pasted newlines preserved as-is.
- Visual cursor wraps with the line; secondary lines indented with a thin `│ `
  guide to indicate continuation.
- Soft-wrap at terminal width; no horizontal scrolling.
- The input bar grows up to 8 visual lines; past that, older lines scroll.

### Architecture

```text
InputBar state
  │
  ├── line_count: usize
  ├── cursor: (row, col)
  ├── soft_wrap: bool
  │
  ▼
handle_key(KeyEvent)
  ├── Enter            → submit
  ├── Shift+Enter      → insert '\n' at cursor
  ├── Alt+Enter        → insert '\n' at cursor (fallback)
  ├── Ctrl+J           → insert '\n' at cursor
  ├── Up/Down          → move within multi-line buffer
  ├── Left/Right       → move within current line
  └── Ctrl+U           → clear to line start (multi-line aware)
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/input_bar.rs` | **New** — multi-line input widget |
| `xaft-tui/src/state.rs` | Replace single-line buffer with `InputBar` |
| `xaft-config/src/keybinding.rs` | Bind `shift+enter`, `ctrl+j` |

### Tests

- `shift_enter_inserts_newline`
- `alt_enter_inserts_newline_fallback`
- `ctrl_j_inserts_newline_universal`
- `enter_submits_multi_line_buffer`
- `cursor_moves_across_lines`
- `buffer_soft_wraps_at_terminal_width`
- `pasted_text_preserves_newlines`

### Acceptance criteria

1. Typing `Hello<Shift+Enter>World<Enter>` sends a 2-line prompt to the agent.
2. The visual cursor moves between lines with `Up`/`Down` arrows.
3. The input bar collapses back to a single line after submit.

---

## F22. Tab Completion

### Goal
Shell-style tab completion for files, paths, slash commands, and `@`-mentions.

### Behaviour

- First `Tab` inserts the longest common prefix of completions.
- Second `Tab` shows a popup of all completions.
- Contexts:
  - `/` at start → complete slash commands (F1).
  - `@` anywhere → complete file paths under `working_dir`.
  - `!` at start → complete from shell history (bash `~/.bash_history`).
  - bare word → fuzzy-complete from prior inputs (LRU 100).
- `Esc` dismisses the popup; `↑`/`↓` navigates.
- Completion respects the existing `list_files` exclusions (`.git`, `node_modules`,
  `target`).

### Architecture

```text
input buffer
  │
  ▼
CompleterRegistry::complete(&buffer, cursor, working_dir)
  │
  ├── SlashCompleter        → Vec<Completion>
  ├── AtMentionCompleter    → Vec<Completion>     # uses list_files
  ├── ShellHistoryCompleter → Vec<Completion>
  ├── LruInputCompleter     → Vec<Completion>
  │
  ▼
CompletionPopup::render(completions, selected)
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/completion/mod.rs` | **New** — `CompleterRegistry` |
| `xaft-tui/src/completion/{slash,mention,history,lru}.rs` | One file per completer |
| `xaft-tui/src/completion/popup.rs` | `CompletionPopup` widget |
| `xaft-tui/src/state.rs` | Wire completer into input handler |

### Tests

- `slash_completer_returns_all_commands`
- `at_mention_completer_lists_files`
- `at_mention_completer_respects_gitignore`
- `lru_completer_suggests_recent_inputs`
- `tab_inserts_longest_common_prefix`
- `tab_shows_popup_on_second_press`

### Acceptance criteria

1. `/co<tab>` completes to `/compact` (only match).
2. `/c<tab><tab>` shows popup of `/clear`, `/compact`, `/config`, `/cost`,
   `/commit`.
3. `@src/<tab>` lists files under `src/`.

---

## F23. Input History with Ctrl+R Search

### Goal
A reverse-incremental-search history (à la bash / fish) for prior prompts.

### Behaviour

- Each non-empty input is recorded in `~/.xaft/history` (one per line, max 10 000).
- `Ctrl+R` opens a search prompt at the bottom: `🔍 <query>`.
- Typing filters history lines that contain the query (substring, case-insensitive).
- `Enter` re-inserts the highlighted match into the input bar.
- `Esc` cancels the search.
- `↑`/`↓` in normal mode (empty buffer) cycles through history newest → oldest.
- History is per-project: scoped to `working_dir` (sha256 of canonical path).

### Architecture

```text
InputHistory
  │
  ├── file: PathBuf                 # ~/.xaft/history/<hash>
  ├── entries: VecDeque<String>     # capped at 10_000
  ├── cursor: Option<usize>
  │
  ▼
HistoryStore::push(&msg) -> ()
HistoryStore::search(&query) -> Vec<&str>
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/history.rs` | **New** — `InputHistory`, `HistoryStore` |
| `xaft-tui/src/state.rs` | Hook `Ctrl+R` into keymap; record on submit |
| `xaft-tui/src/app.rs` | Init `HistoryStore` at startup |

### Tests

- `history_records_submitted_inputs`
- `history_search_filters_by_substring`
- `history_persists_across_sessions`
- `history_caps_at_max_entries`
- `history_isolated_per_working_dir`
- `ctrl_r_opens_search_prompt`

### Acceptance criteria

1. `Ctrl+R` then typing `pag` shows prior inputs containing "pag".
2. `Enter` restores the highlighted input; submitting records it again (deduped).
3. Closing the TUI and reopening preserves history.

---

## F24. External Editor Integration (`$EDITOR`)

### Goal
For complex prompts (long questions, multi-file summaries, code snippets), open the
input in the user's preferred external editor.

### Behaviour

- `Ctrl+G` (or `Ctrl+X Ctrl+E`) opens `$VISUAL` or `$EDITOR` with the current buffer
  in a temp file (`/tmp/xaft-edit-<pid>-<uuid>.md`).
- The editor launches attached to the TUI (suspend-resume or new pseudo-terminal
  depending on `isatty`).
- On editor exit, the file content replaces the input buffer.
- Syntax: markdown highlighting in the temp file.
- `Ctrl+C` in the editor aborts the edit (buffer restored).
- Empty files abort the edit.

### Architecture

```text
Ctrl+G
  │
  ▼
ExternalEditor::edit(&buffer, editor_cmd) -> Result<String>
  │
  ├── write buffer to temp file
  ├── spawn editor subprocess (suspended TUI)
  ├── wait for exit
  ├── read temp file → String
  └── cleanup
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/external_editor.rs` | **New** — `ExternalEditor` |
| `xaft-tui/src/state.rs` | Bind `Ctrl+G` |

### Tests

- `external_editor_uses_visual_or_editor_env`
- `external_editor_writes_buffer_to_temp_file`
- `external_editor_reads_modified_content`
- `external_editor_aborts_on_empty_file`
- `external_editor_aborts_on_ctrl_c`

### Acceptance criteria

1. `Ctrl+G` opens `vim` (or user's `$EDITOR`) with the current input.
2. Saving and quitting vim returns the edited content to the input bar.
3. `:q!` (vim) leaves the input unchanged.

---

## F25. Drag-and-Drop File Upload

### Goal
Beyond paste (F5), accept files dropped from the OS file manager (e.g. dragging a
file from Finder onto the terminal) and attach them to the next message.

### Behaviour

- TUI listens for bracketed paste events containing file URIs (`file://`).
- macOS: Finder drag-drop emits the file path on stdout; we capture it via
  `crossterm::Event::Paste` and convert to file URI.
- Linux: similar via the same paste path.
- Multiple files: each becomes a separate `ContentBlock::Image` or `FileRef`.
- Visible feedback: `[📎 3 files attached]` in the input bar.

### Architecture

```text
crossterm::Event::Paste(text)
  │
  ▼
DropHandler::handle(&text) -> Vec<Attachment>
  │
  ├── parse URIs
  ├── validate (size, mime, exists)
  ├── emit ImageAttached for images
  └── return attachments to inject into next message
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/drag_drop.rs` | **New** — `DropHandler` |
| `xaft-runtime/src/dispatch.rs` | Inject attachments (reuses F5) |
| `xaft-tui/src/state.rs` | Show attached-files hint |

### Tests

- `drop_handler_parses_file_uri`
- `drop_handler_rejects_missing_files`
- `drop_handler_rejects_oversized_files`
- `drop_handler_returns_multiple_attachments`

### Acceptance criteria

1. Dragging a PNG from Finder onto the terminal attaches it to the next message.
2. Multi-file drops are accepted up to a configurable cap (default 10).
3. Non-file URIs (`https://...`) are ignored.

---

# Part 2 — Transcript Rendering

## F26. Markdown Rendering

### Goal
Render common Markdown in agent output as styled terminal text, not raw markdown
syntax.

### Behaviour

- Headers (`#`, `##`, `###`) → bold + larger glyphs.
- Bold (`**…**`), italic (`*…*`), strikethrough (`~~…~~`).
- Lists (`-`, `1.`) → bullets/numbers with hanging indent.
- Tables → aligned with `│` borders.
- Links → underlined + URL after (e.g. `[text](url)` → `text (url)`).
- Block quotes (`> `) → dimmed with `│` prefix.
- Horizontal rules (`---`) → 60-cell `─` separator (already used by TUI).
- Code spans and blocks rendered separately (F27).

### Architecture

```text
agent message (text)
  │
  ▼
MarkdownRenderer::render(&text, max_width) -> Vec<StyledLine>
  │
  ├── pulldown_cmark for parsing
  ├── termimad for ANSI styling
  └── adapt output to xaft-tui StyledLine
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/render/markdown.rs` | **New** — `MarkdownRenderer` |
| `xaft-tui/src/transcript.rs` | Route `LineKind::AgentText` through renderer |
| `xaft-tui/Cargo.toml` | Add `pulldown-cmark`, `termimad` |

### Tests

- `markdown_renders_headers_with_bold`
- `markdown_renders_lists_with_indent`
- `markdown_renders_tables_with_borders`
- `markdown_renders_links_with_url_suffix`
- `markdown_preserves_code_blocks_for_f27`
- `markdown_handles_malformed_input`

### Acceptance criteria

1. Agent output with `## Summary` is rendered with a bold header.
2. Tables are aligned within the terminal width.
3. Code blocks (` ``` `) are passed through to F27 untouched.

---

## F27. Syntax Highlighting in Code Blocks

### Goal
Colorize code blocks in agent output (and the input bar preview, see F24) by
detected language.

### Behaviour

- Detect language from ` ```lang ` fence.
- Fallback: heuristics (extension if path hint, or `text`).
- Supported: rust, python, javascript, typescript, go, java, c, cpp, json, yaml,
  toml, bash, sql, html, css, markdown.
- Powered by `syntect` (sublime-syntax based; ~200 languages supported, but we
  configure only the 15 above to keep startup fast).
- Each rendered line has 1-3 ANSI colour codes; styled via `termimad`.

### Architecture

```text
code block (text, lang)
  │
  ▼
SyntaxHighlighter::highlight(&text, &lang, theme) -> Vec<StyledLine>
  │
  ├── SyntaxSet::load_defaults_newlines()  # lazy-load once
  ├── detect_lang(lang_str) -> Option<SyntaxReference>
  ├── highlight line by line
  └── adapt to xaft-tui StyledLine
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/render/highlight.rs` | **New** — `SyntaxHighlighter` |
| `xaft-tui/src/render/markdown.rs` | Delegate fenced code blocks to highlighter |
| `xaft-tui/Cargo.toml` | Add `syntect` |

### Tests

- `highlight_detects_language_from_fence`
- `highlight_falls_back_to_plain_text`
- `highlight_renders_rust_keywords`
- `highlight_renders_python_strings`
- `highlight_handles_unknown_language_gracefully`

### Acceptance criteria

1. ` ```rust ` code blocks show keywords (`fn`, `let`, `pub`) in color.
2. Unknown languages render as plain text without error.
3. Highlighting adds < 5 ms latency per code block (syntect is fast enough).

---

## F28. Inline Transcript Search (Ctrl+F)

### Goal
`Ctrl+F` opens a search bar that finds the next match in the visible transcript;
`Enter` (or `Ctrl+F` again) jumps to the next occurrence.

### Behaviour

- `Ctrl+F` opens `[search: <query>]` line at the bottom.
- Live filter: as user types, all transcript lines containing the query are
  highlighted (inverted background); non-matching lines dimmed.
- `Enter` jumps the cursor to the next match.
- `Shift+Enter` jumps to the previous match.
- `Esc` exits search mode, restoring normal rendering.
- Match count shown: `[3/12]`.
- `n` / `N` (vim-style) jump next/previous after exiting search bar.

### Architecture

```text
Ctrl+F
  │
  ▼
SearchBar::open(&transcript)
  │
  ▼
query updated → SearchEngine::find_all(&transcript, &query) -> Vec<Match>
  │
  ▼
TranscriptFilter::apply(matches) → highlights + dims
  │
  ▼
Enter / Shift+Enter → scroll to next/prev match
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/search.rs` | **New** — `TranscriptSearch` |
| `xaft-tui/src/transcript.rs` | Add `apply_search_filter` |
| `xaft-tui/src/state.rs` | Bind `Ctrl+F` |

### Tests

- `search_finds_all_occurrences_case_insensitive`
- `search_highlights_matching_lines`
- `search_dims_non_matching_lines`
- `search_jumps_to_next_match_on_enter`
- `search_jumps_to_previous_on_shift_enter`
- `search_displays_match_count`

### Acceptance criteria

1. `Ctrl+F` then `auth` finds every transcript line mentioning `auth`.
2. `Enter` moves through matches; the match count is `[3/12]`.
3. `Esc` restores the unfiltered transcript.

---

# Part 3 — Project Browsing

## F29. File Tree Side Panel

### Goal
A toggleable side panel showing the project structure, with click-to-open and live
file-watching.

### Behaviour

- `Ctrl+B` opens/closes the panel (or just `B` if no dashboard is open; F8).
- Shows tree of files under `working_dir`, respecting `.gitignore`.
- `↑/↓` navigates, `Enter` opens the file in the editor (or shows content in a
  preview pane).
- `r` refreshes (also auto-refreshes on `FileEditsCommitted` signal).
- `m` shows modified files (M), staged (S), untracked (?).
- Filter via `/` (fuzzy file search).
- File watcher via `notify` crate picks up external changes.

### Layout

```text
┌─ Files ─────────────────┐ ┌─ Transcript ─────────────────────────────┐
│ ▾ src/                  │ │  ◈ Planner                                 │
│   ▾ runtime/            │ │  ...                                       │
│     • orchestrator.rs M │ │  ◉ Coder                                   │
│     • explorer.rs       │ │  ...                                       │
│   • agents/             │ │                                            │
│   • tools/              │ │                                            │
│ ▸ tests/                │ │                                            │
│ • Cargo.toml S          │ │                                            │
│ • README.md             │ │                                            │
└─────────────────────────┘ └────────────────────────────────────────────┘
```

### Architecture

```text
FileTree
  │
  ├── tree: TreeNode { path, kind, git_status, is_expanded }
  ├── selected: Option<PathBuf>
  ├── filter: Option<String>
  │
  ▼
FileTreeBuilder::build(working_dir, git_status) -> Tree
  │
  ▼
notify::Watcher → on change → rebuild affected subtree
```

### Files

| File | Change |
|---|---|
| `xaft-tui/src/file_tree/mod.rs` | **New** — `FileTree` widget |
| `xaft-tui/src/file_tree/watcher.rs` | `notify` integration |
| `xaft-tui/src/state.rs` | Render side-by-side with transcript |

### Tests

- `file_tree_lists_working_dir`
- `file_tree_respects_gitignore`
- `file_tree_marks_modified_files`
- `file_tree_filters_by_query`
- `file_tree_watcher_picks_up_external_change`
- `file_tree_opens_file_in_preview`

### Acceptance criteria

1. `Ctrl+B` toggles the panel; it updates live on file edits.
2. Files modified by the agent are marked `M`; pre-existing modifications marked
   differently.
3. Fuzzy filter `/auth` narrows to `auth.rs`, `auth/`, etc.

---

# Part 4 — Safety & Policy

## F30. OS-Level Sandboxing

### Goal
Wrap command execution in a real OS-level sandbox (macOS Seatbelt, Linux
bubblewrap, no-op on Windows) so even a buggy policy cannot leak outside the
working directory.

### Behaviour

- `xaft` auto-detects platform and applies the right sandbox profile.
- **macOS**: SBPL profile denying `file-write*` outside `working_dir` and
  `file-read*` of `~/.ssh`, `~/.aws`, `~/.gnupg`. Allow network for `bash_exec`
  only when network policy (F31) permits.
- **Linux**: `bwrap --bind working_dir / --tmpfs /tmp --ro-bind /usr /usr ...`.
- **Windows**: best-effort via `job object` + ACL on working_dir (informational
  warning if not fully isolated).
- Sandbox profile is per-session; regenerated on `working_dir` change.
- A new `SandboxViolation` signal emitted if the sandbox blocks a syscall (rare,
  surfaces to the user).

### Architecture

```text
CommandExecutor
  │
  ▼
PlatformSandbox::wrap(command, working_dir, profile) -> WrappedCommand
  │
  ├── macos: pre-pend `sandbox-exec -f <profile> <command>`
  ├── linux: pre-pend `bwrap --args <command>`
  └── windows: SetInformationJobObject + ACL (best effort)
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/sandbox/mod.rs` | **New** — `PlatformSandbox` |
| `xaft-runtime/src/sandbox/{macos,linux,windows}.rs` | Per-platform impl |
| `xaft-config/src/types.rs` | `SandboxConfig { enabled, profile_path }` |
| `xaft-cli/src/main.rs` | Print warning if sandbox unavailable |

### Tests

- `sandbox_profile_blocks_writes_outside_working_dir`
- `sandbox_profile_blocks_reads_of_ssh_dir`
- `sandbox_falls_back_gracefully_on_unsupported_platform`
- `sandbox_emits_violation_signal`
- `sandbox_disabled_flag_uses_unsandboxed_exec`

### Acceptance criteria

1. On macOS, `edit_file("/etc/passwd")` is blocked at the OS level.
2. On Linux, `bwrap` invocation is visible in the agent's tool trace.
3. A warning is printed once at startup if the platform isn't fully sandboxed.

---

## F31. Network Policy DSL

### Goal
Restrict which tools may make network requests and to which hosts.

### Behaviour

- New `[tool].network` config section.
- Default-deny for write-tools (edit, write, bash). Read tools are allow-listed.
- DSL:
  ```toml
  [tool.network]
  default = "deny"               # deny | allow

  [[tool.network.rules]]
  tool = "web_fetch"
  action = "allow"
  hosts = ["docs.rs", "*.rust-lang.org", "github.com"]

  [[tool.network.rules]]
  tool = "bash_exec"
  action = "prompt"              # ask user each invocation
  patterns = ["curl *", "wget *", "npm install *"]

  [[tool.network.rules]]
  tool = "*"
  action = "deny"
  hosts = ["169.254.0.0/16", "metadata.google.internal"]
  ```
- CIDR matching for IP ranges.
- `prompt` mode requires approval gate; TUI shows `⚠ Allow curl example.com? [y/n]`.
- Emit `NetworkAccessBlocked { tool, target, reason }` for deny events.

### Architecture

```text
Tool::call
  │
  ▼
NetworkPolicy::check(tool_name, target) -> PolicyDecision
  │
  ├── Allow      → proceed
  ├── Deny       → return error, emit signal
  ├── Prompt     → emit ToolPendingApproval
  └── transform  → rewrite target
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/policy/network.rs` | **New** — `NetworkPolicy` |
| `xaft-config/src/types.rs` | `NetworkConfig` (rules + default) |
| `xaft-tools/src/web/{fetch,search}.rs` | Check policy before request |
| `xaft-tools/src/bash_exec.rs` | Pre-exec policy hook |

### Tests

- `network_policy_default_deny_blocks_unlisted`
- `network_policy_allows_listed_hosts`
- `network_policy_cidr_blocks_metadata_endpoint`
- `network_policy_prompt_triggers_approval`
- `network_policy_deny_emits_signal`

### Acceptance criteria

1. Without explicit config, `bash_exec("curl evil.com")` is denied.
2. `web_fetch("https://docs.rs/foo")` succeeds when `docs.rs` is allowlisted.
3. The TUI shows clear reasons for blocks.

---

## F32. Filesystem Write Policy DSL

### Goal
Constrain the set of paths each tool may write, beyond the working directory.

### Behaviour

- DSL mirrors the network policy:
  ```toml
  [fs.policy]
  default = "deny"

  [[fs.policy.rules]]
  tool = "write_file"
  action = "allow"
  paths = ["src/**", "tests/**", "docs/**"]

  [[fs.policy.rules]]
  tool = "edit_file"
  action = "allow"
  paths = ["**/*.rs", "**/*.toml", "**/*.md"]

  [[fs.policy.rules]]
  tool = "bash_exec"
  action = "prompt"
  paths = ["**"]

  [[fs.policy.rules]]
  tool = "*"
  action = "deny"
  paths = [".env*", "**/secrets/**", ".git/**", "**/*.key", "**/*.pem"]
  ```
- `globset` for pattern matching.
- `prompt` mode requires approval.
- Emit `FilesystemWriteBlocked { tool, path, reason }`.

### Architecture

Same shape as F31 but scoped to write operations.

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/policy/fs.rs` | **New** — `FilesystemPolicy` |
| `xaft-config/src/types.rs` | `FilesystemPolicyConfig` |
| `xaft-tools/src/fs/{write,edit}.rs` | Check policy before write |
| `xaft-tools/src/bash_exec.rs` | Pre-exec policy hook (resolve paths) |

### Tests

- `fs_policy_blocks_writes_to_secrets`
- `fs_policy_allows_writes_to_listed_paths`
- `fs_policy_glob_matches_nested_paths`
- `fs_policy_prompt_triggers_approval`
- `fs_policy_deny_emits_signal`

### Acceptance criteria

1. `write_file(".env.production", "...")` is blocked at the policy layer.
2. Path matches are evaluated before any disk touch.
3. The TUI shows `[fs-blocked] .env.production by policy`.

---

## F33. Cost / Token Kill Switch

### Goal
Auto-abort the run when a configured cost or token limit is reached.

### Behaviour

- New `[limits]` config section:
  ```toml
  [limits]
  max_cost_usd = 5.00
  max_input_tokens = 1_000_000
  max_output_tokens = 250_000
  max_duration_secs = 3600
  on_limit = "abort"             # abort | warn | prompt
  ```
- After every `ModelCallComplete`, `LimitsEnforcer` checks all four limits.
- On breach:
  - `abort` → cancel run, save session with `Cancelled` status, print summary
    showing which limit was hit.
  - `warn` → emit `LimitWarning` signal, continue.
  - `prompt` → emit `LimitReached { limit, value, threshold }`; TUI shows
    `[continue? y/n]`.
- Per-agent limits supported:
  ```toml
  [limits.per_agent.planner]
  max_cost_usd = 0.50
  ```

### Architecture

```text
ModelCallComplete signal
  │
  ▼
LimitsEnforcer::check(&accumulator, &config)
  │
  ├── over_cost       → action(cost)
  ├── over_input_tok  → action(input)
  ├── over_output_tok → action(output)
  ├── over_duration   → action(duration)
  └── within          → no-op
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/limits.rs` | **New** — `LimitsEnforcer` |
| `xaft-config/src/types.rs` | `LimitsConfig` |
| `xaft-agent/src/signals.rs` | `LimitWarning`, `LimitReached` |
| `xaft-runtime/src/orchestrator.rs` | Hook into `ModelCallComplete` |

### Tests

- `limits_enforcer_aborts_on_cost_breach`
- `limits_enforcer_warns_on_input_token_breach`
- `limits_enforcer_prompts_on_duration_breach`
- `limits_enforcer_per_agent_isolated`
- `limits_enforcer_within_thresholds_passes`

### Acceptance criteria

1. Crossing `max_cost_usd` aborts the run with a clear message.
2. `warn` mode shows `[COST WARNING] $4.80 / $5.00 (96%)` in the status bar.
3. `prompt` mode requires explicit user action.

---

# Part 5 — Tooling & Workflow

## F34. Watch Mode (Auto-Run on File Change)

### Goal
A `xaft watch "run tests after every save"` mode that re-invokes a task whenever
watched files change.

### Behaviour

- `xaft watch --files "src/**/*.rs" --task "run cargo test"` starts a long-running
  session that re-runs the task on each matching file event.
- File events debounced 500ms (collapse bursts).
- A 3-second cool-down between runs prevents thrashing.
- The previous run's `RunResult` is shown while the new run is queued.
- `Ctrl+C` cancels the watcher.
- Optional `--once` flag runs the task once and exits (no watching).

### Architecture

```text
xaft watch --files "src/**/*.rs" --task "..."
  │
  ├── notify::Watcher::new(files_glob)
  ├── debouncer (500ms)
  │
  ▼
on file event (after debounce + cooldown)
  │
  ▼
spawn runtime.run(RunRequest{ task, working_dir, ... })
```

### Files

| File | Change |
|---|---|
| `xaft-cli/src/commands/watch.rs` | **New** — `watch` subcommand |
| `xaft-config/src/types.rs` | `WatchConfig { debounce_ms, cooldown_secs }` |

### Tests

- `watch_starts_watcher_on_matching_files`
- `watch_debounces_bursts`
- `watch_cooldown_throttles_runs`
- `watch_cancels_on_ctrl_c`
- `watch_once_runs_and_exits`

### Acceptance criteria

1. `xaft watch` re-invokes the task on each save, showing fresh results.
2. Two edits in 100ms trigger only one re-run.
3. `Ctrl+C` cleanly stops the watcher.

---

## F35. Test Runner Auto-Detection

### Goal
Detect the project's test framework and expose a `run_tests` tool that runs only
the relevant tests.

### Behaviour

- On session start, detect:
  - **Rust**: `cargo test`
  - **Python**: `pytest` (or `unittest`)
  - **JavaScript/TypeScript**: `npm test` / `yarn test` / `pnpm test`
  - **Go**: `go test ./...`
  - **Java (Maven)**: `mvn test`
  - **Java (Gradle)**: `gradle test`
- `run_tests` tool schema:
  ```rust
  pub struct RunTestsInput {
      pub filter: Option<String>,        // cargo test <filter>, pytest -k
      pub scope: Option<TestScope>,      // File | Module | Package | Workspace
      pub fail_fast: Option<bool>,
      pub timeout_secs: Option<u32>,
  }
  ```
- Output: structured JSON `{ passed, failed, skipped, duration_secs, failures: Vec<{name, message, file, line}> }`.
- Emits `TestsRun { passed, failed, duration_secs }` signal.

### Architecture

```text
ProjectDetector::detect(working_dir) -> ProjectKind
  │
  ├── Cargo.toml     → Rust
  ├── pyproject.toml → Python
  ├── package.json   → JS/TS
  ├── go.mod         → Go
  ├── pom.xml        → Java (Maven)
  └── build.gradle   → Java (Gradle)
  │
  ▼
TestRunner::new(kind) -> Box<dyn TestRunner>
  │
  ▼
run_tests(input) -> TestResult (structured)
```

### Files

| File | Change |
|---|---|
| `xaft-tools/src/test/mod.rs` | **New** — `TestRunner` trait |
| `xaft-tools/src/test/{rust,python,js,go,java}.rs` | Per-language impls |
| `xaft-tools/src/test/detect.rs` | `ProjectDetector` |
| `xaft-agent/src/signals.rs` | `TestsRun` |

### Tests

- `detector_recognises_cargo_project`
- `detector_recognises_python_project`
- `runner_runs_cargo_test_with_filter`
- `runner_parses_pytest_json_output`
- `runner_handles_missing_test_framework`
- `runner_respects_timeout`

### Acceptance criteria

1. `run_tests(filter="auth")` runs only auth-related tests.
2. Failures are reported with file:line and message.
3. The agent can iterate on fixes and re-run.

---

## F36. Linter / Formatter Auto-Run

### Goal
After every edit, run the project's linter / formatter and report diffs to the
agent.

### Behaviour

- Detect:
  - **Rust**: `cargo fmt` + `cargo clippy`
  - **Python**: `ruff check` + `ruff format` (or `black`)
  - **JS/TS**: `prettier --check` + `eslint`
  - **Go**: `gofmt -l` + `golangci-lint run`
- New `lint` and `format` tools.
- `format` tool: runs formatter in fix mode, then reports files changed.
- `lint` tool: returns structured lint output (file:line:col: message).
- Optional auto-run hook: after every `edit_file`/`write_file`, run `format` on
  the changed file (config-gated, default off in interactive mode).
- Emits `LintRun { tool, warnings, errors }` signal.

### Files

| File | Change |
|---|---|
| `xaft-tools/src/lint/mod.rs` | **New** — `Linter` trait + per-lang impls |
| `xaft-tools/src/format/mod.rs` | **New** — `Formatter` trait + per-lang impls |
| `xaft-config/src/types.rs` | `LintConfig { auto_format_on_edit, linter_overrides }` |

### Tests

- `linter_detects_cargo_clippy_warnings`
- `linter_detects_ruff_errors`
- `formatter_runs_ruff_format`
- `formatter_returns_changed_files`
- `auto_format_hook_runs_on_edit`

### Acceptance criteria

1. `format` tool runs the project formatter and reports files changed.
2. `lint` returns structured warnings the agent can act on.
3. Auto-run mode is opt-in via config.

---

## F37. Type Checker Integration

### Goal
After every edit, run the project's type checker and surface type errors.

### Behaviour

- Detect:
  - **Rust**: `cargo check`
  - **Python**: `mypy` (or `pyright`)
  - **TypeScript**: `tsc --noEmit`
  - **Go**: `go build ./...`
- `type_check` tool: returns structured errors `{ file, line, col, severity, message }`.
- Auto-run on edit (config-gated, default off).
- Emits `TypeCheckRun { errors, warnings }` signal.
- The tool can also run with a specific file scope: `type_check(paths=["src/foo.rs"])`.

### Files

| File | Change |
|---|---|
| `xaft-tools/src/typecheck/mod.rs` | **New** — `TypeChecker` trait + per-lang impls |
| `xaft-agent/src/signals.rs` | `TypeCheckRun` |

### Tests

- `typecheck_runs_cargo_check`
- `typecheck_runs_mypy_with_config`
- `typecheck_runs_tsc_with_tsconfig`
- `typecheck_returns_structured_errors`
- `typecheck_handles_missing_tool`

### Acceptance criteria

1. `type_check` returns errors with file:line:col.
2. The agent can fix type errors and re-run.
3. Auto-run mode is opt-in.

---

# Part 6 — Multi-Worktree + Compliance

## F38. Multi-Worktree Parallel Workflow

### Goal
Run multiple independent agent sessions in parallel, each in its own git worktree,
coordinated by a meta-agent.

### Behaviour

- `/parallel` (F1) starts a parallel workflow:
  ```
  /parallel --tasks "fix auth bug" "add pagination" "update docs"
  ```
- For each task, create a new worktree (branch `parallel/<n>/<short>`) and spawn
  a `XaftRuntime::run` task.
- A coordinator agent (built-in `parallel-coordinator`) monitors all sub-tasks
  and reports a unified summary at the end.
- Each sub-task's commits stay isolated; no merge happens automatically.
- The user reviews all branches and decides which to merge.
- Concurrency configurable (default 3).

### Architecture

```text
/parallel --tasks "A" "B" "C"
  │
  ▼
ParallelCoordinator
  │
  ├── Worktree A: branch parallel/0/abc
  │     └── spawn runtime.run(task=A)
  ├── Worktree B: branch parallel/1/def
  │     └── spawn runtime.run(task=B)
  └── Worktree C: branch parallel/2/ghi
        └── spawn runtime.run(task=C)
  │
  ▼
await all → emit ParallelTasksCompleted { results }
  │
  ▼
TUI: "✓ A: 2 commits; ✓ B: 1 commit; ✗ C: failed (auth error)"
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/parallel.rs` | **New** — `ParallelCoordinator` |
| `xaft-runtime/src/orchestrator.rs` | Hook into workflow |
| `xaft-tui/src/slash/parallel.rs` | **New** — `/parallel` command |
| `xaft-agent/src/signals.rs` | `ParallelTaskStarted`, `ParallelTaskCompleted`, `ParallelTasksCompleted` |

### Tests

- `parallel_coordinator_creates_n_worktrees`
- `parallel_coordinator_runs_n_runtimes_concurrently`
- `parallel_coordinator_aggregates_results`
- `parallel_coordinator_handles_partial_failure`
- `parallel_coordinator_respects_concurrency_cap`

### Acceptance criteria

1. `/parallel` with 3 tasks spawns 3 worktrees and runs them concurrently.
2. Each worktree is on a distinct branch with isolated commits.
3. The summary lists per-task outcomes.

---

## F39. Audit Log + Session Replay

### Goal
A complete, immutable log of every action in a session, plus a replay mechanism
that re-runs the session deterministically.

### Behaviour

- Audit log written to `~/.xaft/sessions/<id>/audit.jsonl` (one JSON object per
  line, append-only).
- Each entry: `{ timestamp, kind, payload }` where `kind` ∈ {`llm_call`,
  `tool_call`, `file_edit`, `commit`, `approval`, `signal`, `cost`}.
- `xaft replay <session-id>`:
  - Reads audit log.
  - Reconstructs the LLM call sequence (using `MockLlmProvider` with the recorded
    responses).
  - Re-runs the session, comparing actual state to recorded state.
  - Reports any divergence.
- Audit log survives session deletion (for compliance) — controlled by
  `[audit].retain_after_delete`.

### Architecture

```text
SignalBus::emit
  │
  ▼
AuditListener (subscribed at bootstrap)
  │
  ├── for each signal: serde_json::to_value(&signal)
  ├── append to audit.jsonl
  └── emit (don't block the bus)
```

### Files

| File | Change |
|---|---|
| `xaft-audit/src/lib.rs` | **New** crate |
| `xaft-audit/src/logger.rs` | `AuditLogger` (jsonl writer) |
| `xaft-audit/src/replay.rs` | `SessionReplayer` |
| `xaft-runtime/src/lib.rs` | Attach `AuditLogger` in `bootstrap` |
| `xaft-cli/src/commands/replay.rs` | **New** — `xaft replay` subcommand |
| `xaft-config/src/types.rs` | `AuditConfig { enabled, retain_after_delete, redact }` |

### Tests

- `audit_logger_writes_to_jsonl`
- `audit_logger_captures_all_signals`
- `audit_logger_redacts_secrets`
- `replayer_reconstructs_llm_sequence`
- `replayer_detects_state_divergence`
- `replayer_handles_missing_audit_file`

### Acceptance criteria

1. `audit.jsonl` contains one line per significant event.
2. Secrets (`api_key`, `password`) are redacted.
3. `xaft replay <id>` reproduces the session, with optional divergence report.

---

## F40. Named Checkpoints + Auto-Checkpoint

### Goal
Allow users to mark a session state as a named checkpoint and revert to it later.
Plus an auto-checkpoint policy that captures state every N turns.

### Behaviour

- `/checkpoint <name>` marks the current state (transcript + files + git ref) as
  a named snapshot.
- `/checkpoints` lists all named checkpoints for the current session.
- `/rewind-to <name>` (alias of `/rewind`) restores files to that git ref and
  truncates the transcript.
- Auto-checkpoint: every N turns (config, default 10), capture a checkpoint named
  `auto-<turn-N>` automatically.
- Checkpoints stored in `~/.xaft/sessions/<id>/checkpoints/<name>.json` with
  `{ name, turn, message_index, git_sha, transcript_snapshot_path }`.
- Emits `CheckpointCreated`, `CheckpointRestored` signals.

### Architecture

```text
/checkpoint my-fix
  │
  ▼
CheckpointStore::create(name, session) -> Checkpoint
  │
  ├── git rev-parse HEAD → git_sha
  ├── snapshot transcript (copy messages.json)
  ├── write checkpoint manifest
  └── emit CheckpointCreated signal
  │
  ▼
/rewind-to my-fix
  │
  ▼
CheckpointStore::restore(name) -> ()
  │
  ├── git checkout <git_sha> -- .
  ├── replace transcript in memory
  ├── replace conversation store
  └── emit CheckpointRestored signal
```

### Files

| File | Change |
|---|---|
| `xaft-runtime/src/checkpoint.rs` | **New** — `CheckpointStore` |
| `xaft-tui/src/slash/{checkpoint,checkpoints,rewind_to}.rs` | **New** slash commands |
| `xaft-config/src/types.rs` | `CheckpointConfig { auto_interval_turns }` |
| `xaft-agent/src/signals.rs` | `CheckpointCreated`, `CheckpointRestored` |

### Tests

- `checkpoint_creates_manifest_with_git_sha`
- `checkpoint_snapshots_transcript`
- `checkpoint_restore_resets_files`
- `checkpoint_restore_truncates_transcript`
- `auto_checkpoint_fires_every_n_turns`
- `checkpoint_listing_returns_all_named`

### Acceptance criteria

1. `/checkpoint before-refactor` creates a named snapshot.
2. `/checkpoints` shows it in a list.
3. `/rewind-to before-refactor` restores the git state and transcript.

---

# Cross-Cutting Concerns

## Config additions

```toml
[input]
multi_line = true
tab_completion = true
history_max = 10_000
external_editor = "${EDITOR}"

[dnd]
enabled = true
max_files = 10
max_size_bytes = 50_000_000

[render.markdown]
enabled = true
max_width = 100

[render.highlight]
enabled = true
theme = "base16-eighties.dark"
languages = ["rust", "python", "javascript", "typescript", "go", "json", "yaml", "toml", "bash", "sql", "html", "css", "markdown"]

[search]
case_sensitive = false
incremental = true

[file_tree]
enabled = true
respect_gitignore = true
watch = true

[sandbox]
enabled = true
profile = "default"            # default | strict | permissive

[tool.network]
default = "deny"               # deny | allow

[[tool.network.rules]]
tool = "web_fetch"
action = "allow"
hosts = ["docs.rs", "*.rust-lang.org", "github.com"]

[[tool.network.rules]]
tool = "bash_exec"
action = "prompt"
patterns = ["curl *", "wget *", "npm install *"]

[fs.policy]
default = "deny"

[[fs.policy.rules]]
tool = "write_file"
action = "allow"
paths = ["src/**", "tests/**", "docs/**"]

[[fs.policy.rules]]
tool = "edit_file"
action = "allow"
paths = ["**/*.rs", "**/*.toml", "**/*.md"]

[[fs.policy.rules]]
tool = "*"
action = "deny"
paths = [".env*", "**/secrets/**", ".git/**", "**/*.key", "**/*.pem"]

[limits]
max_cost_usd = 5.00
max_input_tokens = 1_000_000
max_output_tokens = 250_000
max_duration_secs = 3600
on_limit = "abort"             # abort | warn | prompt

[limits.per_agent.planner]
max_cost_usd = 0.50

[watch]
debounce_ms = 500
cooldown_secs = 3

[test]
auto_run_on_edit = false

[lint]
auto_format_on_edit = false
linter_overrides = { rust = "clippy" }

[typecheck]
auto_run_on_edit = false

[parallel]
default_concurrency = 3
auto_merge = false

[audit]
enabled = true
retain_after_delete = true
redact_patterns = ["api_key", "password", "secret", "token"]

[checkpoint]
auto_interval_turns = 10
max_named = 50
```

## Signal additions (single file: `xaft-agent/src/signals.rs`)

| Signal | Added by | Fields |
|---|---|---|
| `MentionExpanded` (reuse) | F21 | (already in PRD 27) |
| `TabCompletionTriggered` | F22 | `prefix, candidates` |
| `HistorySearched` | F23 | `query, matches` |
| `ExternalEditorInvoked` | F24 | `editor, path, duration` |
| `FileDropped` | F25 | `paths, total_bytes` |
| `MarkdownRendered` | F26 | `lines, blocks` |
| `CodeHighlighted` | F27 | `lang, lines` |
| `TranscriptSearchOpened` / `TranscriptSearchUpdated` | F28 | `query, current, total` |
| `FileTreeOpened` / `FileTreeRefreshed` | F29 | `path, total_files` |
| `SandboxViolation` | F30 | `platform, syscall, path` |
| `NetworkAccessBlocked` | F31 | `tool, target, reason` |
| `FilesystemWriteBlocked` | F32 | `tool, path, reason` |
| `LimitWarning` / `LimitReached` | F33 | `limit, value, threshold` |
| `FileChanged` | F34 | `path, kind` |
| `TestsRun` | F35 | `passed, failed, duration_secs, framework` |
| `LintRun` | F36 | `tool, warnings, errors` |
| `TypeCheckRun` | F37 | `errors, warnings, tool` |
| `ParallelTaskStarted` / `ParallelTaskCompleted` / `ParallelTasksCompleted` | F38 | per-task fields |
| `AuditEventRecorded` | F39 | `kind, payload_size` |
| `CheckpointCreated` / `CheckpointRestored` | F40 | `name, turn, git_sha` |

## Crate additions

| New crate | Owner | Depends on |
|---|---|---|
| `xaft-audit` | F39 | `serde_json`, `tokio` |
| `xaft-policy` | F31, F32 | `globset`, `ip_network` |

## Test infrastructure

- Each new tool gets 3+ unit tests (success, edge case, error).
- Each new slash command gets 1+ integration test using `MockLlmProvider` /
  `MockTransport`.
- Sandbox tests must be marked `#[ignore]` for non-Linux/macOS CI but run by
  default on the respective platforms.
- Audit log tests use `tempfile::TempDir` for the audit file.
- Replay tests use `MockTransport::queue_*` to verify the replayed session's LLM
  call sequence matches the recorded one.

---

# Rollout Plan

The features form 5 independent, shippable milestones.

| Milestone | Features | Estimated surface |
|---|---|---|
| **M1 — Input UX** | F21, F22, F23, F24, F25 | `xaft-tui` |
| **M2 — Transcript polish** | F26, F27, F28, F29 | `xaft-tui` |
| **M3 — Safety & policy** | F30, F31, F32, F33 | new `xaft-policy` crate + `xaft-runtime` |
| **M4 — Tooling & workflow** | F34, F35, F36, F37 | `xaft-tools` + `xaft-cli` |
| **M5 — Compliance & multi-worktree** | F38, F39, F40 | `xaft-audit` crate + `xaft-runtime` |

Recommended order: **M3 → M4 → M1 → M2 → M5**. M3 (safety) is foundational and
should land before tools that can execute network / FS operations. M4 adds tools
that depend on the policy layer. M1 and M2 polish the user experience. M5 is
backend infrastructure that can land last.

---

# Acceptance Criteria (Whole PRD)

1. All 20 features implemented with unit + integration tests passing.
2. `just test` — full workspace green.
3. `just lint` — no new clippy warnings in the 20 feature files (pre-existing
   warnings in `xaft-config` are out of scope).
4. AGENTS.md invariants preserved: every new mutation emits a signal; no
   structured-output-driven state changes; tools remain the only mutation boundary.
5. Every new tool is registered through `ToolRegistry` (no ad-hoc wiring).
6. Every new config field documented in `xaft-config` with a safe default.
7. Backward compatibility: `XaftConfig::default()` continues to work unchanged.
8. Zero new runtime dependencies beyond what's declared in each new crate's
   `Cargo.toml`.
9. Each new crate publishes a `lib.rs` quick-start example using `MockLlmProvider`.
10. The whole PRD can be incrementally enabled: every feature gated by its config
    flag, defaulting to a safe value.
11. Sandbox tests pass on macOS and Linux; Windows is best-effort with a printed
    warning.
12. Replay determinism: replaying a recorded session reproduces the LLM call
    sequence exactly (modulo tool side-effects on the file system).

---

# Files to create / modify (summary)

| File | Change |
|---|---|
| `prds/28_claude_code_parity_features_r2.md` | This file |
| `crates/xaft-tui/src/input_bar.rs` | F21 |
| `crates/xaft-tui/src/completion/{mod,slash,mention,history,lru,popup}.rs` | F22 |
| `crates/xaft-tui/src/history.rs` | F23 |
| `crates/xaft-tui/src/external_editor.rs` | F24 |
| `crates/xaft-tui/src/drag_drop.rs` | F25 |
| `crates/xaft-tui/src/render/{markdown,highlight}.rs` | F26, F27 |
| `crates/xaft-tui/src/search.rs` | F28 |
| `crates/xaft-tui/src/file_tree/{mod,watcher}.rs` | F29 |
| `crates/xaft-runtime/src/sandbox/{mod,macos,linux,windows}.rs` | F30 |
| `crates/xaft-policy/src/{mod,network,fs,cidr}.rs` | **New** crate, F31, F32 |
| `crates/xaft-runtime/src/limits.rs` | F33 |
| `crates/xaft-cli/src/commands/watch.rs` | F34 |
| `crates/xaft-tools/src/test/{mod,rust,python,js,go,java,detect}.rs` | F35 |
| `crates/xaft-tools/src/lint/mod.rs` | F36 |
| `crates/xaft-tools/src/format/mod.rs` | F36 |
| `crates/xaft-tools/src/typecheck/mod.rs` | F37 |
| `crates/xaft-runtime/src/parallel.rs` | F38 |
| `crates/xaft-audit/src/{lib,logger,replay}.rs` | **New** crate, F39 |
| `crates/xaft-cli/src/commands/replay.rs` | F39 |
| `crates/xaft-runtime/src/checkpoint.rs` | F40 |
| `crates/xaft-tui/src/slash/{checkpoint,checkpoints,rewind_to,parallel}.rs` | F38, F40 |
| `crates/xaft-agent/src/signals.rs` | 20+ new signal types |
| `crates/xaft-config/src/types.rs` | New config sections |
| `crates/xaft-runtime/src/orchestrator.rs` | Wire limits, parallel, audit, checkpoint |
| `crates/xaft-runtime/src/lib.rs` | Re-exports |
| `crates/xaft-cli/src/args.rs` | New flags: `--watch`, `--replay`, etc. |
| `Cargo.toml` (workspace) | Add `xaft-policy`, `xaft-audit` members |
