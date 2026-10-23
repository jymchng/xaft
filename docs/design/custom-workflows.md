# Enabling custom workflows in xaft — design proposal

**Status:** implemented (2026-10-23) — Option A as the base, with
declarative transition tools, guards, effects and tool groups, plus Option C's
escape hatch as `agent_type = "script"` phases. See
[Custom workflows](../workflows/04-custom-workflows.md) for the shipped design;
this page is kept as the decision record.
**Date:** 2026-08-15
**Scope:** `crates/xaft-runtime` (WorkflowConfig / RunRequest) + TUI `/workflow(s)`
**Author question:** "How should xaft enable custom workflows, allowing users to create theirs?"

---

## Current state (verified against source)

xaft's workflow model is a compile-time enum with three variants
(`crates/xaft-agents/src/registry.rs:98`, re-exported from `xaft-runtime`
at `crates/xaft-runtime/src/lib.rs:72`):

```rust
pub enum WorkflowConfig {
    Standard,                       // plan → coder → QA ↔ fixer (default)
    Dynamic { initial_agent, max_handoffs, agent_subset },
    Meta { meta_prompt, max_spawned_agents, max_parallel, allow_nesting },
}
```

- `RunRequest.workflow: WorkflowConfig` (`crates/xaft-runtime/src/dispatch.rs:46`).
- The TUI exposes `/workflow` and `/workflows` commands that select among the
  built-in enum variants (see `docs/guides/workflows.md`).
- **There is no way for a user to define a workflow without recompiling xaft.**

Reference: agenthicc (the companion project) already solves this with
**Python workflow plugins**: project `.agenthicc/workflows/*.py` or global
`~/.agenthicc/workflows/` containing `WorkflowPlugin`/`PhaseSpec` classes, plus
`[workflows.<name>]` TOML for per-project values, discovered by
`workflows/loader.py` (`docs/guides/custom-workflows-and-config.md`).

---

## Goal

Let users author custom workflows **without recompiling xaft**, with enough
flexibility to express arbitrary orchestration logic, while staying safe and
discoverable.

## Design considerations

- **No recompilation** — workflows are data or scripts loaded at runtime.
- **Discoverability** — `/workflows` lists custom workflows alongside built-ins.
- **Validation** — graphs are checked (unreachable nodes, missing terminal
  nodes, cycles without iteration guards) at load time with clear errors.
- **Safety** — arbitrary code (if any) runs under explicit opt-in and
  documented limits.
- **Versionability** — workflows live in the repo (`.xaft/workflows/`) so they
  are shared via git.

---

## Option A — TOML-declarative phase graph

Users define a **directed graph of phases** in TOML:

```toml
# .xaft/workflows/release-review.toml
name = "release-review"
description = "Plan → review → fix → approve"

[phase.plan]
agent = "planner"
tools = ["read_file", "search_code", "git_*"]

[phase.review]
agent = "qa"
tools = ["read_file", "git_diff"]
next = "fix"

[phase.fix]
agent = "fixer"
tools = ["*"]
next = "review"
max_iterations = 3        # loop guard

[phase.approve]
agent = "reviewer"
terminal = true
```

**Runtime integration:** a new `WorkflowRegistry` in `xaft-runtime` loads
`.xaft/workflows/*.toml` (project) and `~/.config/xaft/workflows/*.toml`
(global), validates the graph, and adds a `WorkflowConfig::Custom { graph }`
variant. `RunRequest.workflow` accepts it; the TUI `/workflow <name>` selects
it.

| Aspect | Assessment |
|---|---|
| Authoring | Trivial — TOML, no code |
| Flexibility | Fixed phase-graph model only |
| Safety | No arbitrary code; fully validate-able |
| Effort | Low–medium (loader + validator + registry + one enum variant) |
| Pros | Declarative, versionable, safe, easy to document |
| Cons | Cannot express arbitrary runner logic |

---

## Option B — Script/plugin workflows with arbitrary logic *(preferred direction)*

The most flexible: users provide **arbitrary glue logic**, matching agenthicc's
`WorkflowPlugin` philosophy. Three sub-variants:

### B1. Embedded script (rhai recommended)

Embed a lightweight Rust scripting engine (e.g. `rhai`) so users write
`.xaft/workflows/<name>.rhai` that returns the same graph structure and can
hook `on_phase_enter` / `on_phase_exit`:

```rhai
// .xaft/workflows/release-review.rhai
let graph = #{
  phases: [
    #{ name: "plan", agent: "planner", tools: ["read_file", "search_code"] },
    #{ name: "review", agent: "qa", tools: ["read_file", "git_diff"], next: "fix" },
    #{ name: "fix", agent: "fixer", tools: ["*"], next: "review", max_iterations: 3 },
    #{ name: "approve", agent: "reviewer", terminal: true },
  ]
};
graph
```

- **Pros:** arbitrary logic without a Rust toolchain; single small dep; sandbox
  can be restricted (no fs/network unless granted).
- **Cons:** new scripting surface to learn, document, and secure.

### B2. Rust dylib plugin

Users compile `.xaft/workflows/<name>.so` exposing
`fn build_workflow() -> WorkflowConfig`; loaded via `libloading`.

- **Pros:** full Rust power; type-safe.
- **Cons:** requires a Rust toolchain and compilation — high friction; ABI
  stability concerns; needs security review (arbitrary native code).

### B3. Agenthicc-style external process plugin

A workflow is an executable that speaks a small JSON protocol on stdin/stdout
(like MCP): xaft sends `{ "request": "build", "params": {...} }`, the plugin
returns `{ "graph": [...] }` and later receives phase events.

- **Pros:** language-agnostic (Python/Node/shell); no embedding dep; natural
  for teams that already script.
- **Cons:** process spawn overhead; needs a protocol spec + timeout/error
  handling.

---

## Option C — Hybrid: TOML graph + scripted escape hatch *(recommended)*

Ship **Option A as the default** (covers ~90% of needs) and add a per-phase
`runner = "script:<name>"` option that delegates that phase's logic to a
scripted function (B1-style rhai or B3-style subprocess):

```toml
[phase.fix]
agent = "fixer"
tools = ["*"]
next = "review"
runner = "script:apply_fix_and_retest"   # escape hatch
```

| Aspect | Assessment |
|---|---|
| Authoring | TOML for common cases; script only when needed |
| Flexibility | Graph + arbitrary logic escape hatch |
| Safety | Scripts are opt-in and can be restricted |
| Effort | Medium (A + one runner hook) |
| Pros | Declarative ergonomics + full power; matches agenthicc's "TOML + plugin" philosophy |
| Cons | Two authoring models to document |

---

## Option D — Full trait-based plugin system

Extend xaft's existing trait-based design with a `WorkflowPlugin` trait
(`Send + Sync`, `fn build(&self, params) -> WorkflowConfig`), discovered from
`.xaft/plugins/` through the existing plugin loader:

```rust
pub trait WorkflowPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn build(&self, params: &WorkflowParams) -> WorkflowConfig;
}
```

| Aspect | Assessment |
|---|---|
| Authoring | Rust trait impls (or FFI-backed) |
| Flexibility | Highest |
| Safety | Depends on how plugins are loaded (dylib = native code risk) |
| Effort | High (plugin loader + trait + registry + docs) |
| Pros | Matches xaft's agent/tool trait design; cleanest long-term extension point |
| Cons | Heaviest; overkill if users mainly need phase graphs |

---

## Recommendation

Implement the **hybrid of A + B (Option C)**, with this build order:

1. **Option A first** — TOML phase-graph `WorkflowRegistry` + `Custom` variant
   + `/workflows` listing + graph validation. Safe, fast, covers most needs.
2. **Then add the script escape hatch** — per-phase `runner` hook using an
   embedded script (rhai) or a subprocess JSON protocol (B3), matching your
   preference for arbitrary logic in the agenthicc spirit.

This gives declarative ergonomics for common workflows and full power when
needed, without forcing users to learn Rust.

---

## Appendix — `tui_screenshot` availability

- agenthicc's source (`src/agenthicc/tools/tui_screenshot.py`) defines a tool
  literally named `"tui_screenshot"` (stateful TUI session manager + styled
  screenshot renderer, `name = "tui_screenshot"`), but it is **not registered**
  in the tool exports and is **not exposed as a callable tool to this agent**.
- The analogous capability available in this session is `terminal_screenshot`
  (launches a tmux session, optionally sends keys, and captures deterministic
  PNG / SVG / ANSI / plain-text screenshots).

---

*This document is a proposal — no source code was changed.*
