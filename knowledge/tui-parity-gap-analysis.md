# TUI Parity Gap Analysis: xaft ↔ agenthicc

Status: gap report (goal #1 of the "make xaft look like agenthicc" effort)
Date: 2026-08-14
Scope: `crates/xaft-tui` (Rust, ratatui/crossterm) vs agenthicc `tui/` (Python, Rich Live)

## Method

The agenthicc TUI surface is documented in `docs/guides/tui.md` and implemented
in `src/agenthicc/tui/` (workspace/appender, input/unified_session, trigger
system, workspace/overlays, workspace/components, tui/triggers, terminal
backends). Each gap below cites the agenthicc feature + source, then the xaft
counterpart or lacuna in `crates/xaft-tui/src`.

Legend: ✅ present · 🟡 partial · ❌ absent

---

## Gap 1 — `$` skill-only trigger (❌ xaft)

- **agenthicc**: `$` opens a skill-only dropdown. Source:
  `src/agenthicc/tui/triggers/slash_command.py` — `class SkillTrigger(SlashCommandTrigger): char = "$"; skill_only = True; include_aliases = True`. Registered alongside `/` and `@`; documented in `docs/guides/tui.md` ("`$` — skill-only picker backed by discovered skill records").
- **xaft**: trigger registry registers only `'@'` (MentionTriggerHandler) and `'/'` (SlashCommandTriggerHandler). See `crates/xaft-tui/src/trigger/mod.rs` (module doc: "`'@'` → MentionTriggerHandler … `'/'` → SlashCommandTriggerHandler") and `trigger/mention.rs`/`trigger/slash_command.rs`. There is **no `$` skill trigger**; xaft has a `crates/xaft-skills` crate but no TUI picker for it.
- **Action**: add a `$`-trigger skill picker in `crates/xaft-tui/src/trigger/` (register `'$'`), backed by the skill loader, reusing the dropdown/menu machinery.

## Gap 2 — Safe → Plan → Yolo cycle vs xaft's Auto/Ask/Review/Safe/Debug (🟡)

- **agenthicc**: three selectable modes cycle **Safe → Plan → Yolo → Safe**; `Auto` (Yolo), `Guard`/`Ask` (Safe), `Review` (Plan) remain accepted aliases; `Debug` rejected. Source: `docs/guides/tui.md` ("The selectable mode cycle is **Safe → Plan → Yolo → Safe** … `Debug` is not an alias and is rejected"), `src/agenthicc/tui/runtime/mode_manager.py`.
- **xaft**: six built-in modes `auto, plan, ask, review, safe, debug` in `crates/xaft-tui/src/mode/builtins.rs`. Shift+Tab cycling exists (`state.rs:2070` "Shift+Tab — cycle mode (PRD 66)"), and `/mode [name]` exists (`slash/commands/mode_cmd.rs`). But the **cycle order and semantics differ**: xaft cycles through all six; agenthicc cycles three with aliases. xaft has no Yolo alias for Auto and **accepts** Debug rather than rejecting it.
- **Action**: align the cycle to Safe→Plan→Yolo(→Safe) with alias mapping (Auto≡Yolo, Ask/Guard≡Safe, Review≡Plan) and reject `debug` in `/mode`; keep full mode registry for direct selection.

## Gap 3 — collapsed tool-group "...and N more tool calls" flush (❌ xaft)

- **agenthicc**: `ScrollBufferAppender` batches contiguous tool completions into a group; while a group is open the overflow count lives in the live footer, and at the next conversation boundary (or interrupt) the appender **flushes** `...and N more tool calls` to the scroll buffer — never left only in the footer. Source: `src/agenthicc/tui/workspace/appender.py` (`_group_count`, `flush()`, `_flush_exploration_group()`, `_flush_group_summary()`; docstring "so the collapsed tool count is printed in the scroll buffer instead of remaining in the live footer"), `docs/guides/tui.md` ("When a tool group is collapsed, its `...and N more tool calls` line is flushed … at the next conversation boundary and immediately when the agent is interrupted").
- **xaft**: `crates/xaft-tui/src/transcript.rs` renders tool blocks and `agent_tracker.rs` tracks tool counts, but there is **no collapsed-group summary flushed to scrollback** on boundary/interrupt (grep for `more tool calls`/`N more` in `crates/xaft-tui/src` → no matches).
- **Action**: implement tool-group batching in the transcript/appender path with a flushed overflow summary line.

## Gap 4 — "Loading transcript…" status + resumed-transcript tail (🟡 xaft)

- **agenthicc**: on `--continue`/`--resume`, the newest N (default 20) turns are replayed into the appender (presentation-only, not re-persisted) with a `Loading transcript…` status label; replay is chunked. Source: `docs/guides/tui.md` ("the newest 20 complete turns are loaded from the tail … `Loading transcript…` label … chunked"), `src/agenthicc/tui/workspace/appender.py::replay()`.
- **xaft**: resume exists — `app.rs` (`--resume` → `replay_history_lines_multi` / `replay_history_lines`; "resumed: {short_id}" separators at `app.rs:470/627/667`). But xaft replays from **memory keys**, not a session-log tail projection, and there is **no `Loading transcript…` status** or bounded-turn limit (20) / chunking.
- **Action**: add a bounded tail-replay (configurable `resume_transcript_turns`, default 20) with a `Loading transcript…` status label and chunked appending.

## Gap 5 — wall-clock telemetry: `✾ Total wall clock time since last IDLE` (🟡 xaft)

- **agenthicc**: after a turn returns to IDLE the scroll buffer prints `✾ Worked for …` **and** `✾ Total wall clock time since last IDLE: …` (the outer activity span across internal LLM turns/phases; waiting-modal ticks retain the activity start point). Source: `docs/guides/tui.md`, `src/agenthicc/tui/workspace/` (activity clock) + status/components.
- **xaft**: prints `✻ Worked for {elapsed}` (`crates/xaft-tui/src/surface.rs:113`) and per-turn telemetry, but there is **no total-wall-clock-since-last-IDLE line**.
- **Action**: track an outer activity start timestamp and print the total wall-clock summary after `Worked for`.

## Gap 6 — stable waiting labels for approval/question/plan-review (🟡 xaft)

- **agenthicc**: when a tool approval, plan review, or `ask_user()` question is pending, the status bar switches from the animated Thinking state to a **stable waiting label** (no 50 ms timer redraws). Source: `docs/guides/tui.md` ("the status bar changes from the animated Thinking state to a stable waiting label … without publishing a timer redraw every 50 ms").
- **xaft**: `AgentStatus::AwaitingApproval` exists (`agent_tracker.rs`) and shows "Approval"/"⚠", and the ephemeral status bar renders a mode prefix, but the **stable-label suppression of timer redraws** during approval/question/plan-review is not explicitly enforced.
- **Action**: suppress tick-driven status redraws while an approval/question/plan-review overlay owns the prompt; show a stable label.

## Gap 7 — Windows `ReadConsoleInputW` + POSIX non-TTY no-op backend (❌ xaft)

- **agenthicc**: `tui/terminal/windows_backend.py` uses `ReadConsoleInputW` so Shift+Tab preserves its modifier; `posix_backend.py` is a no-op for non-TTY fds and restores terminal state on exit. Source: `docs/guides/tui.md`, `src/agenthicc/tui/terminal/`.
- **xaft**: `crates/xaft-tui/src/surface.rs` uses `crossterm` `enable_raw_mode/disable_raw_mode` (POSIX), and there is **no Windows `ReadConsoleInputW` backend** and no explicit non-TTY no-op guard (grep `cfg(windows)`/`ReadConsoleInput` in `crates/xaft-tui/src` → 0 matches).
- **Action**: add a `cfg(windows)` input backend using `ReadConsoleInputW` (preserving Shift+Tab as BackTab) and a non-TTY guard in the POSIX backend.

## Gap 8 — bracketed-paste placeholder editing (🟡 xaft)

- **agenthicc**: large bracketed pastes stay behind a single `[Pasted text #N ...]` composer placeholder; Home/End operate on the visible projection; Backspace after the closing `]` deletes the whole paste, elsewhere one hidden char; `Ctrl+V` reveals; Esc-after-`]` deletes whole. Source: `docs/guides/tui.md` ("Large bracketed pastes stay behind a single `[Pasted text #N ...]` composer placeholder …"), `src/agenthicc/tui/input/`.
- **xaft**: paste exists — `input_bar.rs` (`pasted_text_preserves_newlines`, `paste_in_middle_inserts_lines`, `crlf_normalized` tests) and `bridge.rs:135` (bracketed-paste payload). But xaft **expands** the paste into the buffer rather than a **collapsed placeholder** with projection editing.
- **Action**: add a paste-placeholder projection (Home/End on visible row, Backspace-after-`]` whole-delete, Ctrl+V reveal, Esc semantics).

## Gap 9 — `@` picker path fragments (./, ../, absolute, ~, \\) (🟡 xaft)

- **agenthicc**: the `@` picker accepts `./`, `../`, absolute, `~`-prefixed, and platform-native `/` or `\\` separators; a second `@` is a delimiter, not a path char; boundary rule applied to submitted messages. Source: `docs/guides/tui.md`, `src/agenthicc/tui/triggers/` + mention parser.
- **xaft**: `@` mention resolver exists (`trigger/mention.rs`, `mention.rs`) and handles workspace-relative paths, but **`~`-expansion and Windows `\\` separators** are not evident (grep shows `workspace_root.join(ctx.dir_prefix...)` only).
- **Action**: extend `MentionResolver` path handling: `~`, absolute, `\\` separators, `@@` delimiter.

## Gap 10 — `/usage` local snapshot + `/config` overlay during response (🟡 xaft)

- **agenthicc**: `/usage` shows local token/cost without sending to the agent; `/config` opens the config overlay immediately (even mid-response); `/cancel`/`/interrupt` share the Ctrl+C cancellation owner; `/bg` uses the background-session control plane. Source: `docs/guides/tui.md`, `src/agenthicc/commands/builtins.py`.
- **xaft**: `/cost` exists (`slash/commands/cost.rs` — token/cost table) and `/config` exists (`slash/commands/config.rs`, config menu overlay), and the input pipeline supports local execution while streaming (`user_message.rs`). Gaps: no explicit **`/usage`** alias, no documented **mid-response `/config`** overlay guarantee, and background sessions are not wired through `/bg` in the TUI (xaft-session/background lives in the runtime crate).
- **Action**: add `/usage` alias, guarantee `/config` opens during streaming, document `/bg`/`/cancel` owner mapping.

## Gap 11 — tool-result blocks: numbered preview + 6-row diff truncation (🟡 xaft)

- **agenthicc**: `● Read(...)`, `● Run(...)`, `● Search(...)` blocks with bounded numbered output preview; `● Update(...)` diff shows ≤6 changed rows (first+last 3 with `...`). Source: `docs/guides/tui.md` ("Each contiguous change block shows at most six changed rows …"), `src/agenthicc/tui/workspace/appender.py`.
- **xaft**: `transcript.rs` has `build_file_diff_lines` and the renderer draws tool blocks with bounded output (`read_before_edit` style previews in `xaft-tools`), but the **6-row diff truncation contract** is not documented/verified.
- **Action**: enforce ≤6-row diff preview in `build_file_diff_lines` with `...` omission row; verify bounded numbered preview for read/run/search.

## Gap 12 — blank-line separator + `What's new` welcome fallback (🟡 xaft)

- **agenthicc**: completed LLM responses end with a blank line in the scroll buffer; startup welcome keeps `What's new` with `No list` fallback when the remote changelog is unreachable. Source: `docs/guides/tui.md`, `src/agenthicc/tui/welcome.py`.
- **xaft**: `app.rs`/`surface.rs` render a startup hint; response spacing is not explicitly guaranteed (blank line after every completed response). Welcome changelog fetch + `No list` fallback ❌.
- **Action**: add a trailing blank line after completed responses; add a startup welcome with remote-changelog fallback (`No list`).

## Gap 13 — overlay discipline: no direct terminal writes outside workspace (🟡 xaft)

- **agenthicc**: overlays must not write to the terminal directly; they update state/callback and let the workspace redraw; new approval kinds need overlay class + registry entry + tests. Source: `docs/guides/tui.md` ("An overlay must not write directly to the terminal outside the workspace …").
- **xaft**: overlay system exists (`menu/`, `workspace` equivalents in `app.rs`/`state.rs`/`prompt.rs`), and the single render loop implies central redraw; but there is no explicit **documented discipline/test** that overlays never write outside the workspace.
- **Action**: add a doc note + test asserting overlays only mutate state (no direct terminal writes).

## Non-gaps (already at parity)

- Shift+Tab mode cycling: `state.rs:2070`, prompt hint `prompt.rs:123/131`, `/mode` `mode_cmd.rs`.
- Bracketed paste newline preservation: `input_bar.rs` tests G5/G5b.
- Resume/replay: `app.rs` `--resume` → `replay_history_lines*` (though bounded-tail/chunking/label is a gap).
- `Worked for` telemetry: `surface.rs:113`.
- Approval queue + auto-approve gates + risk levels: `approval.rs`, `approval_gate.rs`.
- Token/cost display: `ephemeral.rs`, `surface.rs:116`, `/cost`.
- POSIX raw mode via crossterm + restore-on-exit guard: `surface.rs:49-85`, `renderer.rs:390-397`.

## Suggested implementation order

1. `$` skill trigger (Gap 1) — self-contained, high value.
2. Tool-group collapse + flush (Gap 3) — transcript/appender core.
3. Mode cycle alignment + aliases (Gap 2).
4. Paste placeholder projection (Gap 8) — input core.
5. Bounded resume tail + `Loading transcript…` (Gap 4).
6. Wall-clock total + stable waiting labels (Gaps 5–6).
7. Windows backend + non-TTY guard (Gap 7) — platform.
8. `@` path fragments, `/usage`, 6-row diff, blank-line/welcome (Gaps 9–13).
