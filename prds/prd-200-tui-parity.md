# PRD-200 — TUI Feature Parity with agenthicc

**Status**: draft · **Effort**: multi-crate (xaft-tui + xaft-skills + xaft-session) · **Depends on**: knowledge/tui-parity-gap-analysis.md

## Problem

xaft's TUI (`crates/xaft-tui`) is a capable streaming transcript renderer but
lacks several interaction affordances that agenthicc's Rich-Live TUI ships:
a `$` skill picker, collapsed tool-group summaries flushed to scrollback,
a Safe→Plan→Yolo mode cycle, paste-placeholder editing, bounded resumed
transcript replay with a loading label, total wall-clock telemetry, stable
waiting labels, a Windows `ReadConsoleInputW` backend, and `@`-picker path
fragments. Users switching from agenthicc expect the same ergonomics.

## Goals

- G1 `$` skill-only trigger with a dropdown backed by `xaft-skills`.
- G2 Tool-group collapse: contiguous tool completions grouped; `...and N more tool calls` flushed to scrollback at the next boundary or on interrupt.
- G3 Mode cycle Safe→Plan→Yolo→Safe with aliases (Auto≡Yolo, Ask/Guard≡Safe, Review≡Plan); reject `debug` via `/mode`.
- G4 Bracketed-paste placeholder projection (Home/End on visible row, Backspace-after-`]` whole-delete, Ctrl+V reveal, Esc semantics).
- G5 Bounded resume-tail replay (configurable `resume_transcript_turns`, default 20) with `Loading transcript…` status; chunked appending.
- G6 Total wall-clock-since-last-IDLE line after `Worked for`; stable waiting labels for approval/question/plan-review (no 50 ms timer redraws).
- G7 Windows `ReadConsoleInputW` input backend preserving Shift+Tab; POSIX non-TTY no-op guard.
- G8 `@` picker path fragments: `./`, `../`, absolute, `~`, `\\`; `@@` delimiter.
- G9 `/usage` local snapshot; `/config` opens during response; document `/bg`/`/cancel` ownership.
- G10 Tool-result blocks: ≤6-row diff truncation with `...`; bounded numbered previews.
- G11 Blank line after completed responses; welcome panel with `No list` fallback.
- G12 Overlay discipline documented + tested (no direct terminal writes outside the workspace).

## Non-goals

- Porting agenthicc's Python implementation to Rust.
- Changing the runtime/provider/tool crates' semantics (only TUI presentation).
- Removing xaft's six-mode registry (aliases map onto it; direct `/mode <name>` still selects any built-in).

## Design

- `$` trigger: extend `trigger/mod.rs` registry with `'$'` → `SkillTriggerHandler`; reuse `MenuDriver`/`DropdownWidget`; filter registry by `is_skill`.
- Tool groups: extend `transcript.rs` + the appender path (`renderer.rs`/`app.rs` flush boundaries) with `_group_count`-style state; flush on boundary/interrupt.
- Modes: adjust `mode/manager.rs::cycle()` to the three-mode cycle with alias lookup; reject `debug` in `mode_cmd.rs`.
- Paste: `input_bar.rs` gains a paste-placeholder projection (visible one-line view over hidden payload).
- Resume: `app.rs` reads the session-log tail (last N turns) and replays via the transcript renderer; status label `Loading transcript…`.
- Telemetry: `state.rs` tracks outer activity start; `surface.rs` prints `✾ Total wall clock time since last IDLE: …`.
- Backends: new `input/windows.rs` behind `#[cfg(windows)]` using `ReadConsoleInputW`; POSIX `surface.rs` guards non-TTY.
- Mentions: extend `MentionResolver` with `~`/absolute/`\\` and `@@` delimiter.

## Acceptance

- `cargo build --workspace` + `cargo test -p xaft-tui` green.
- Each G has a unit/integration test (paste projection, group flush, cycle order, resume tail, wall-clock, `$` picker).
- `cargo clippy -D warnings` and `cargo fmt --check` clean on touched crates.
- Docs updated: `docs/tui/` + new `docs/guides/tui.md` mirror describing the aligned feature set.

## Out of scope (CI/docs mirroring)

Separate goals cover `mkdocs.yml` + `docs/` restructure, `llms.txt`, CI
workflows, CHANGELOG/CONTRIBUTING/LICENSE, `prds/` + `knowledge/` skeletons,
and a `scripts/ci.sh` runner — see the goal list for this effort.
