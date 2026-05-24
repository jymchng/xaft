# Layout Engine

## Responsive Layout System

`xaft` TUI adapts to terminal dimensions. Minimum: 80×24. Recommended: 160×48.

```
Terminal width < 100:  Single-pane mode (no left/right split)
Terminal width ≥ 100:  Dual-pane mode (30% left, 70% right)
Terminal width ≥ 160:  Three-pane mode (20% left, 50% center, 30% right)
```

## Pane Registry

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pane {
    PlanTree,
    AgentOutput(usize),   // agent index
    DiffViewer,
    ShellConsole,
    CostDashboard,
    LogConsole,
}

pub struct PaneLayout {
    pub visible_panes: Vec<Pane>,
    pub focused_pane: Pane,
    pub sizes: HashMap<Pane, Constraint>,
}
```

## Tab System

Five tabs accessible via number keys (1–5) or Tab/Shift-Tab:

```
[1: Output] [2: Plan] [3: Diff] [4: Shell] [5: Logs]
```

- **Output**: Agent streaming text. Multiple agents → sub-tabs per agent.
- **Plan**: Plan step tree with status, timing, cost per step.
- **Diff**: Staged changes diff viewer. Sortable by file.
- **Shell**: Live shell command output. Scrollable history.
- **Logs**: Timestamped event log. Filterable by level/type.

## Adaptive Agent Panes

When multiple agents are active (parallel execution), the Output tab shows a split view:

```
[Agent: code_agent ⟳]  [Agent: review_agent ✓]
──────────────────────  ──────────────────────
< streaming output >    Last response:
                        No issues found.
```

## Status Bar Fields

```
[session_state] · [task_id-short] · [step N/M] · [tool_name] · [turns] · [$cost] · [tokens] · [elapsed]

Example:
Executing · task:ab12 · Step 2/5 · write_file · Turn 7/20 · $0.042 · 3,420 tk · 2m34s
```

## References

- Next: [Diff Viewer →](03_diff_viewer.md)
