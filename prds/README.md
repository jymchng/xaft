# Product requirements index

This directory contains the project's product requirements and implementation
notes. Many documents are historical design records; verify their status
against the current source before implementing them.

## How to use

- PRDs are numbered in rough chronological order. Lower numbers are older
  design records; the numbers reflect the order they were written, not
  priority.
- Implementation notes are suffixed (`_implementation`) or numbered in the
  same sequence as their design counterparts.
- Before implementing anything, read the relevant PRD **and** check the
  current source — the code is the source of truth and may have moved past
  the document.

## Current roadmap

- [PRD-200 — TUI Feature Parity with agenthicc](prd-200-tui-parity.md) —
  Safe→Plan→Yolo cycle, `$` skill trigger, tool-group collapse, resume-tail
  replay, paste placeholder, wall-clock telemetry, Windows backend.
- [PRD-52 — Skills System](52_skills_system.md) — skill loading, frontmatter,
  prompt injection.
- [PRD-64 — Mode System Architecture](64_mode_system_architecture.md) —
  built-in modes, tool filters, hooks.
- [PRD-65 — Builtin Modes](65_builtin_modes.md) — the six built-in modes.
- [PRD-59 — Input Trigger System](59_input_trigger_system.md) — `/`, `@`,
  `$`, `#` triggers.
- [PRD-61 — Menu Widget System](61_menu_widget_system.md) — dropdowns and
  the command menu.
- [PRD-56 — Parallel Tool Calls](56_parallel_tool_calls.md) — concurrent
  tool execution.
- [PRD-58 — Meta Agent](58_meta_agent.md) — the meta-agent orchestration.

## Historical design records (selected)

The full set (09–66) covers the original TUI, agent handoff, provider
abstraction, session persistence, mention system, slash commands, config
display, and the explore-agent pool. These are design records; verify against
`crates/*` before relying on them.

## Adding a PRD

1. Number it after the highest existing number (currently 200+).
2. Mirror the structure: **Problem / Goals / Non-goals / Design / Acceptance**.
3. Reference the relevant crates and link related PRDs.
4. Add it to this index.
