# Diff Viewer

## Overview

The diff viewer pane renders unified diffs of agent-produced changes with syntax highlighting, navigation, and inline approval controls.

## Diff Acquisition

After each `write_file` or `apply_patch` tool call:
1. `WorkspaceEditor::diff(path, new_content)` produces a `UnifiedDiff`
2. A `PatchApplied` signal is emitted
3. TUI subscribes and updates `DiffViewerState`

## Rendering

```rust
pub struct DiffViewerState {
    pub files: Vec<FileDiff>,
    pub selected_file: usize,
    pub scroll_offset: usize,
    pub show_context_lines: usize,  // default 3
}

pub struct FileDiff {
    pub path: PathBuf,
    pub hunks: Vec<DiffHunk>,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub language: Option<String>,  // for syntax highlighting
}

pub fn render_diff(frame: &mut Frame, state: &DiffViewerState, area: Rect) {
    // File list on left, hunks on right
    let layout = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(75),
    ]).split(area);

    render_file_list(frame, &state.files, state.selected_file, layout[0]);
    render_hunks(frame, &state.files[state.selected_file].hunks, state.scroll_offset, layout[1]);
}

fn render_hunks(frame: &mut Frame, hunks: &[DiffHunk], scroll: usize, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for hunk in hunks {
        // Hunk header
        lines.push(Line::from(Span::styled(
            format!("@@ -{},{} +{},{} @@", hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count),
            Style::default().fg(Color::Cyan),
        )));

        for diff_line in &hunk.lines {
            let (prefix, color) = match diff_line.kind {
                DiffLineKind::Added   => ("+", Color::Green),
                DiffLineKind::Removed => ("-", Color::Red),
                DiffLineKind::Context => (" ", Color::DarkGray),
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(color)),
                Span::styled(&diff_line.content, Style::default().fg(color)),
            ]));
        }
    }

    let visible = &lines[scroll.min(lines.len().saturating_sub(1))..];
    frame.render_widget(
        Paragraph::new(visible.to_vec())
            .block(Block::bordered().title(" Changes ")),
        area,
    );
}
```

## Diff Navigation

| Key | Action |
|---|---|
| `j/k` or `↑/↓` | Scroll by line |
| `J/K` | Next/prev hunk |
| `n/p` | Next/prev file |
| `Enter` | Open file in `$EDITOR` |
| `y` | Accept all changes in file |
| `N` | Discard all changes in file |

## References

- agtrs: `agtrs-workspace/src/diff.rs`
- Next: [Streaming Panes →](04_streaming_panes.md)
