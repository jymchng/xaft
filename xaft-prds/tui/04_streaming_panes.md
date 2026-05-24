# Streaming Panes

## Token-Level Streaming Rendering

Each text delta from `StreamEvent::TextDelta` is appended to the active agent pane's line buffer. The render loop picks up accumulated deltas on the next 33ms tick.

```rust
fn handle_agent_text_delta(state: &mut AppState, agent_idx: usize, delta: String) {
    if let Some(pane) = state.agent_panes.get_mut(agent_idx) {
        // Append delta to current line
        if let Some(last) = pane.current_line.as_mut() {
            last.push_str(&delta);
            if delta.contains('\n') {
                // Commit line to buffer
                let line = pane.current_line.take().unwrap();
                if pane.lines.len() >= 2000 {
                    pane.lines.pop_front();
                }
                pane.lines.push_back(StyledLine::plain(line));
                pane.current_line = Some(String::new());
            }
        }
        if pane.auto_scroll {
            pane.scroll_offset = pane.lines.len().saturating_sub(1);
        }
    }
}
```

## Thinking Blocks

Extended thinking content (`StreamEvent::ThinkingDelta`) is rendered in a collapsed section, expandable with `t`:

```
[▶ Thinking...] (press 't' to expand)
```

When expanded:
```
[▼ Thinking]
I need to first understand the current auth structure.
Let me read auth.rs to see what's there before making changes...
```

## Tool Execution Feed

When a tool is executing, the agent pane shows a live status line:

```
▶ Calling: write_file("src/auth.rs")
  Bytes: 4,231 · Status: writing...
```

After completion:
```
✓ write_file("src/auth.rs") · 124ms · 4,231 bytes
```

## Shell Console Streaming

The Shell tab streams `cargo test` output in real time:

```rust
fn handle_shell_output(state: &mut AppState, command: &str, chunk: &str, is_stderr: bool) {
    let color = if is_stderr { Color::Yellow } else { Color::White };
    let styled = StyledLine { spans: vec![StyledSpan { text: chunk.to_string(), color }] };
    if state.shell_lines.len() >= 5000 {
        state.shell_lines.pop_front();
    }
    state.shell_lines.push_back(styled);
}
```

## References

- Next: [Approval Dialogs →](05_approval_dialogs.md)
