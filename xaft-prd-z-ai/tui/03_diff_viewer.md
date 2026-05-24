# XAFT Diff Viewer

## Inline Diff Viewer: Rendering EditReceipt Diffs in the Terminal

The diff viewer is xaft's most visually complex widget. It receives `EditReceipt` signals
from the agtrs `FileEditor` tool and renders inline diffs showing exactly what the agent
changed, in real-time, as edits stream in.

### EditReceipt Structure (from agtrs)

```rust
/// The signal payload from FileEditor tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditReceipt {
    /// The file that was edited
    pub file_path: PathBuf,

    /// The edit operation performed
    pub operation: EditOperation,

    /// Lines that were replaced/inserted/deleted
    pub hunks: Vec<DiffHunk>,

    /// Whether this edit has been approved
    pub approved: Option<bool>,

    /// Timestamp of the edit
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Agent that performed the edit
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    /// Old file range (line_start, line_count)
    pub old_range: (usize, usize),
    /// New file range (line_start, line_count)
    pub new_range: (usize, usize),
    /// Line-by-line diff
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiffLine {
    Context { content: String, line_number: (usize, usize) },
    Removed { content: String, line_number: usize },
    Added   { content: String, line_number: usize },
}
```

### Visual Representation

#### Unified Mode (default)

```
 src/auth/token.rs
═══════════════════════════════════════════════════════
 12 │ fn validate_token(token: &str) -> Result<bool> {
 13 │     let secret = get_secret();
 14 │     let now = SystemTime::now()
    │  ─────────────────────────────────────────────────
 15 │-        .duration_since(UNIX_EPOCH)?.as_secs();
    │+        .duration_since(UNIX_EPOCH)?
    │+        .as_secs();
 16 │
    │  ─────────────────────────────────────────────────
 17 │-    if token.expiry < now {
 17 │+    if token.expiry <= now {
 18 │         return Ok(false);
 19 │     }
═══════════════════════════════════════════════════════
 Hunk 2/4 │ n/N next/prev │ Tab side-by-side │ a approve
```

#### Side-by-Side Mode

```
 src/auth/token.rs                                         │ src/auth/token.rs (edited)
───────────────────────────────────────────────────────────┼───────────────────────────────────────────────────
 14 │     let now = SystemTime::now()                     │ 14 │     let now = SystemTime::now()
 15 │-        .duration_since(UNIX_EPOCH)?.as_secs();     │ 15 │+        .duration_since(UNIX_EPOCH)?
    │                                                      │ 16 │+        .as_secs();
 16 │                                                      │ 17 │
 17 │-    if token.expiry < now {                          │ 18 │+    if token.expiry <= now {
 18 │         return Ok(false);                            │ 19 │         return Ok(false);
───────────────────────────────────────────────────────────┼───────────────────────────────────────────────────
 Hunk 2/4 │ n/N next/prev │ Tab unified │ a approve       │
```

## Diff Widget State

```rust
/// State for the DiffViewer pane
pub struct DiffState {
    /// All diffs received in this session, ordered by receipt time
    diffs: Vec<FileDiff>,

    /// Currently displayed file diff index
    current_diff_index: usize,

    /// Currently highlighted hunk index (within current file diff)
    current_hunk_index: usize,

    /// Display mode
    mode: DiffMode,

    /// Scroll offset (vertical)
    scroll_offset: u16,

    /// Syntax highlight cache
    syntax_cache: HashMap<PathBuf, SyntaxHighlights>,

    /// Number of context lines to show around hunks
    context_lines: usize,

    /// Whether the diff viewer is in approval mode
    approval_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiffMode {
    Unified,
    SideBySide,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub file_path: PathBuf,
    pub hunks: Vec<DiffHunk>,
    pub receipt: EditReceipt,
    pub old_content: String,
    pub new_content: String,
}
```

## Syntax Highlighting with tree-sitter

### Approach

xaft uses tree-sitter for syntax highlighting in the diff viewer. The approach:

1. **Parse the new file content** with the appropriate tree-sitter grammar
2. **Map highlight query matches** to ratatui `Style` objects
3. **Cache results** per file — only re-parse when the file changes
4. **Apply highlights to diff lines** — context and added lines get highlights,
   removed lines are rendered in red (no syntax highlighting for deletions)

```rust
/// Syntax highlight engine
pub struct SyntaxEngine {
    /// Loaded grammars, keyed by language name
    grammars: HashMap<String, tree_sitter::Language>,

    /// Highlight query sets, keyed by language name
    highlight_queries: HashMap<String, Query>,

    /// Theme mapping: tree-sitter highlight capture → ratatui Style
    theme: HashMap<String, Style>,

    /// Parse cache: (file_path, content_hash) → highlighted lines
    cache: HashMap<(PathBuf, u64), Vec<HighlightedLine>>,
}

impl SyntaxEngine {
    /// Highlight a file's content, returning per-line style spans
    pub fn highlight(&mut self, path: &Path, content: &str) -> Vec<HighlightedLine> {
        let lang_name = self.detect_language(path);
        let content_hash = Self::hash_content(content);

        // Check cache
        let cache_key = (path.to_path_buf(), content_hash);
        if let Some(cached) = self.cache.get(&cache_key) {
            return cached.clone();
        }

        let grammar = self.grammars.get(&lang_name)
            .unwrap_or_else(|| self.grammars.get("plain").unwrap());
        let query = self.highlight_queries.get(&lang_name)
            .unwrap_or_else(|| self.highlight_queries.get("plain").unwrap());

        // Parse
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(grammar).expect("Invalid grammar");
        let tree = parser.parse(content, None).expect("Parse failed");

        // Apply highlight queries
        let mut cursor = QueryCursor::new();
        let matches = cursor.matches(query, tree.root_node(), content.as_bytes());

        let mut lines = vec![HighlightedLine::default(); content.lines().count()];
        for m in matches {
            for capture in m.captures {
                let capture_name = &query.capture_names()[capture.index as usize];
                let style = self.theme.get(capture_name)
                    .copied()
                    .unwrap_or(Style::default());

                let start_line = capture.node.start_position().row;
                let end_line = capture.node.end_position().row;
                let start_col = capture.node.start_position().column;
                let end_col = capture.node.end_position().column;

                for line_idx in start_line..=end_line {
                    let line = &mut lines[line_idx];
                    let span_start = if line_idx == start_line { start_col } else { 0 };
                    let span_end = if line_idx == end_line { end_col } else { usize::MAX };
                    line.spans.push(HighlightSpan {
                        start: span_start,
                        end: span_end,
                        style,
                    });
                }
            }
        }

        // Cache result
        self.cache.insert(cache_key, lines.clone());
        lines
    }

    /// Detect language from file extension
    fn detect_language(&self, path: &Path) -> String {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust".into(),
            Some("py") => "python".into(),
            Some("js") => "javascript".into(),
            Some("ts") => "typescript".into(),
            Some("go") => "go".into(),
            Some("java") => "java".into(),
            Some("c" | "h") => "c".into(),
            Some("cpp" | "hpp" | "cc") => "cpp".into(),
            Some("rb") => "ruby".into(),
            Some("md") => "markdown".into(),
            Some("toml") => "toml".into(),
            Some("json") => "json".into(),
            Some("yaml" | "yml") => "yaml".into(),
            _ => "plain".into(),
        }
    }
}
```

### Theme: tree-sitter Capture → ratatui Style

| tree-sitter Capture | ratatui Style | Example |
|---|---|---|
| `keyword` | `fg(Yellow) bold` | `fn`, `let`, `if` |
| `function` | `fg(Blue)` | `validate_token` |
| `function.call` | `fg(Cyan)` | `get_secret()` |
| `string` | `fg(Green)` | `"hello"` |
| `comment` | `fg(DarkGray) italic` | `// note` |
| `type` | `fg(Magenta)` | `Result` |
| `number` | `fg(Red)` | `42` |
| `operator` | `fg(White)` | `->`, `<=` |
| `property` | `fg(Cyan)` | `token.expiry` |
| `punctuation` | `fg(Gray)` | `(`, `{` |
| `variable` | `fg(White)` | `now` |
| `constant` | `fg(Red) bold` | `UNIX_EPOCH` |

### HighlightedLine Structure

```rust
#[derive(Debug, Clone, Default)]
pub struct HighlightedLine {
    pub spans: Vec<HighlightSpan>,
}

#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

impl HighlightedLine {
    /// Render this line into ratatui Spans, applying syntax highlighting
    /// and diff coloring
    pub fn to_spans(&self, content: &str, diff_type: DiffLineType) -> Vec<Span> {
        let bg = match diff_type {
            DiffLineType::Added => Color::Rgb(40, 60, 40),    // Dark green bg
            DiffLineType::Removed => Color::Rgb(60, 30, 30),  // Dark red bg
            DiffLineType::Context => Color::Reset,
        };

        let mut spans = Vec::new();
        let mut pos = 0;

        for span in &self.spans {
            // Gap before this span (unhighlighted text)
            if pos < span.start {
                let text = &content[pos..span.start.min(content.len())];
                spans.push(Span::styled(
                    text.to_string(),
                    Style::default().bg(bg),
                ));
            }

            // The highlighted span
            let end = span.end.min(content.len());
            let text = &content[span.start.min(content.len())..end];
            spans.push(Span::styled(
                text.to_string(),
                span.style.add_modifier(Modifier::empty()).bg(bg),
            ));

            pos = end;
        }

        // Remaining text after last span
        if pos < content.len() {
            spans.push(Span::styled(
                content[pos..].to_string(),
                Style::default().bg(bg),
            ));
        }

        spans
    }
}
```

## Line Number Display

### Dual Line Numbers (Side-by-Side and Unified)

```
Unified mode:
    14 │     let now = SystemTime::now()
    15 │-        .duration_since(UNIX_EPOCH)?.as_secs();
    15 │+        .duration_since(UNIX_EPOCH)?
    16 │+        .as_secs();
    17 │
    18 │-    if token.expiry < now {
    18 │+    if token.expiry <= now {

Side-by-side mode:
  OLD                                  │  NEW
  14 │     let now = SystemTime::now() │  14 │     let now = SystemTime::now()
  15 │-       .duration_since(...)     │  15 │+       .duration_since(...)
     │                                  │  16 │+       .as_secs();
  17 │                                  │  17 │
  18 │-    if token.expiry < now {     │  18 │+    if token.expiry <= now {
```

```rust
/// Render line number column
fn render_line_number(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    line_num: Option<usize>,
    diff_type: DiffLineType,
) {
    let (text, style) = match (line_num, diff_type) {
        (Some(n), DiffLineType::Context) => (
            format!("{:>4}", n),
            Style::default().fg(Color::DarkGray),
        ),
        (Some(n), DiffLineType::Removed) => (
            format!("{:>4}", n),
            Style::default().fg(Color::Red).bg(Color::Rgb(60, 30, 30)),
        ),
        (Some(n), DiffLineType::Added) => (
            format!("{:>4}", n),
            Style::default().fg(Color::Green).bg(Color::Rgb(40, 60, 40)),
        ),
        (None, _) => (
            "    ".to_string(),
            Style::default(),
        ),
    };

    let span = Span::styled(text, style);
    span.render(Rect::new(x, y, 4, 1), buf);

    // Separator
    let sep = Span::styled(" │ ", Style::default().fg(Color::DarkGray));
    sep.render(Rect::new(x + 4, y, 3, 1), buf);
}
```

## Navigation: Next/Prev Hunk

### Hunk Navigation State Machine

```
  ┌─────────────────────────────────────────────┐
  │  Hunk 2/4                                   │
  │                                              │
  │  ... context lines ...                       │
  │  ─────────────────────────                   │
  │  - removed line          ◄── current hunk    │
  │  + added line                                │
  │  ─────────────────────────                   │
  │  ... context lines ...                       │
  │                                              │
  │  n: next hunk  N: prev hunk                  │
  │  Enter: jump to file  Esc: dismiss           │
  └─────────────────────────────────────────────┘
```

```rust
/// Navigate between hunks
impl DiffState {
    pub fn next_hunk(&mut self) {
        if self.current_hunk_index + 1 < self.current_file_diff().hunks.len() {
            self.current_hunk_index += 1;
            self.scroll_to_hunk(self.current_hunk_index);
        } else if self.current_diff_index + 1 < self.diffs.len() {
            // Move to next file
            self.current_diff_index += 1;
            self.current_hunk_index = 0;
            self.scroll_to_hunk(0);
        }
    }

    pub fn prev_hunk(&mut self) {
        if self.current_hunk_index > 0 {
            self.current_hunk_index -= 1;
            self.scroll_to_hunk(self.current_hunk_index);
        } else if self.current_diff_index > 0 {
            // Move to previous file, last hunk
            self.current_diff_index -= 1;
            let last_hunk = self.current_file_diff().hunks.len().saturating_sub(1);
            self.current_hunk_index = last_hunk;
            self.scroll_to_hunk(last_hunk);
        }
    }

    /// Scroll the view so the given hunk is centered
    fn scroll_to_hunk(&mut self, hunk_index: usize) {
        let hunk = &self.current_file_diff().hunks[hunk_index];
        let target_line = hunk.new_range.0; // Start line of the hunk
        // Center the hunk in the visible area
        // (actual scroll offset depends on viewport height)
        self.scroll_offset = target_line.saturating_sub(5) as u16;
    }

    fn current_file_diff(&self) -> &FileDiff {
        &self.diffs[self.current_diff_index]
    }
}
```

## Real-Time Diff Streaming

### How Diffs Stream In as the Agent Edits

When the agent calls `FileEditor`, the tool emits signals at multiple stages:

```
Time ──────────────────────────────────────────────────────►

Agent decides to edit         FileEditor begins       FileEditor
token.rs                      edit operation           completes
    │                              │                      │
    ▼                              ▼                      ▼
┌─────────┐    ┌─────────────────┐    ┌─────────────────┐
│ Signal: │    │ Signal:         │    │ Signal:         │
│ ToolCal │    │ EditReceipt     │    │ EditReceipt     │
│ lStart  │    │ (partial,       │    │ (complete,      │
│         │    │  hunks: [...])  │    │  hunks: [...],  │
│ Display:│    │                 │    │  approved: None) │
│ "Editing│    │ Display:        │    │                 │
│  token. │    │ Show diff with  │    │ Display:        │
│  rs..." │    │ spinner for     │    │ Full diff       │
│         │    │ in-progress     │    │ rendered        │
└─────────┘    │ hunks           │    │                 │
               └─────────────────┘    └─────────────────┘
```

### Streaming Diff Rendering

```rust
/// Handle EditReceipt signals, supporting partial/streaming diffs
impl DiffState {
    pub fn handle_edit_receipt(&mut self, receipt: EditReceipt) {
        // Find existing diff for this file or create new
        let diff_index = self.diffs.iter().position(|d| d.file_path == receipt.file_path);

        match diff_index {
            Some(idx) => {
                // Update existing diff (agent made another edit to same file)
                let existing = &mut self.diffs[idx];

                // Merge hunks: if overlapping, replace; if new, append
                for hunk in receipt.hunks {
                    let overlap = existing.hunks.iter().position(|h| {
                        h.old_range.0 <= hunk.old_range.0 + hunk.old_range.1
                        && hunk.old_range.0 <= h.old_range.0 + h.old_range.1
                    });

                    match overlap {
                        Some(pos) => existing.hunks[pos] = hunk, // Replace overlapping hunk
                        None => existing.hunks.push(hunk),        // Append new hunk
                    }
                }

                // Sort hunks by line number
                existing.hunks.sort_by_key(|h| h.old_range.0);

                // Update new content
                existing.new_content = receipt.new_content;
            }
            None => {
                // New file diff
                self.diffs.push(FileDiff {
                    file_path: receipt.file_path.clone(),
                    hunks: receipt.hunks,
                    receipt,
                    old_content: String::new(), // Will be populated lazily
                    new_content: String::new(),
                });

                // Auto-navigate to newest diff
                self.current_diff_index = self.diffs.len() - 1;
                self.current_hunk_index = 0;
            }
        }
    }
}
```

### Visual: Diff Streaming Animation

```
Frame 1: Agent starts editing token.rs
┌─────────────────────────────────────────────┐
│ ✎ Editing src/auth/token.rs...              │
│ ◌ Computing diff...                         │
│                                             │
└─────────────────────────────────────────────┘

Frame 2: First hunk received (streaming)
┌─────────────────────────────────────────────┐
│ src/auth/token.rs (1 hunk, streaming...)    │
═══════════════════════════════════════════════
│ 14 │     let now = SystemTime::now()        │
│ 15 │-        .duration_since(UNIX_EPOCH)?   │
│ 15 │+        .duration_since(UNIX_EPOCH)?   │
│ 16 │+        .as_secs();          ◌ more... │
═══════════════════════════════════════════════

Frame 3: Second hunk received
┌─────────────────────────────────────────────┐
│ src/auth/token.rs (2 hunks)                 │
═══════════════════════════════════════════════
│ 14 │     let now = SystemTime::now()        │
│ 15 │-        .duration_since(UNIX_EPOCH)?   │
│ 15 │+        .duration_since(UNIX_EPOCH)?   │
│ 16 │+        .as_secs();                    │
│ 17 │                                        │
│ 18 │-    if token.expiry < now {            │
│ 18 │+    if token.expiry <= now {           │
│ 19 │         return Ok(false);              │
═══════════════════════════════════════════════
 Hunk 1/2 │ n next │ N prev │ a approve │ r reject
```

## Integration with the Approval System

### Approval-Required Diffs

When the agent's tool call has `requires_confirmation: true`, the diff viewer enters
approval mode:

```
┌──────────────────────────────────────────────────────────────┐
│ ⚠ APPROVAL REQUIRED                                          │
│ src/auth/token.rs — 2 hunks, 4 lines changed                │
════════════════════════════════════════════════════════════════
│ 14 │     let now = SystemTime::now()                         │
│ 15 │-        .duration_since(UNIX_EPOCH)?.as_secs();         │
│ 15 │+        .duration_since(UNIX_EPOCH)?                    │
│ 16 │+        .as_secs();                                     │
│ 17 │                                                         │
│ 18 │-    if token.expiry < now {                             │
│ 18 │+    if token.expiry <= now {                            │
│ 19 │         return Ok(false);                               │
════════════════════════════════════════════════════════════════
│                                                              │
│  [a] Approve   [r] Reject   [e] Edit   [s] Skip hunk       │
│  [A] Approve all remaining   [R] Reject all                 │
│                                                              │
│  Risk: MEDIUM │ Agent: file-editor-01 │ Tool: FileEditor    │
└──────────────────────────────────────────────────────────────┘
```

### Approval Actions

```rust
/// Actions available when viewing an unapproved diff
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiffApprovalAction {
    /// Approve this hunk — apply the change
    ApproveHunk,
    /// Reject this hunk — revert the change
    RejectHunk,
    /// Edit the hunk — open in $EDITOR for manual modification
    EditHunk,
    /// Skip this hunk — leave for later decision
    SkipHunk,
    /// Approve all remaining hunks in this file
    ApproveAll,
    /// Reject all remaining hunks in this file
    RejectAll,
    /// Approve all pending diffs across all files
    ApproveAllFiles,
}

impl DiffState {
    pub fn handle_approval_action(&mut self, action: DiffApprovalAction, state: &mut AppState) {
        match action {
            DiffApprovalAction::ApproveHunk => {
                let diff = self.current_file_diff_mut();
                diff.hunks[self.current_hunk_index].approved = Some(true);
                state.approval_queue.resolve_current(ApprovalResult::Approved);
                self.next_hunk();
            }
            DiffApprovalAction::RejectHunk => {
                let diff = self.current_file_diff_mut();
                diff.hunks[self.current_hunk_index].approved = Some(false);
                state.approval_queue.resolve_current(ApprovalResult::Rejected);
                self.next_hunk();
            }
            DiffApprovalAction::EditHunk => {
                // Suspend TUI, open $EDITOR with the new content
                // On return, read the edited content and create a new hunk
                let path = self.current_file_diff().file_path.clone();
                state.suspend_for_editor(&path);
            }
            DiffApprovalAction::ApproveAll => {
                let diff = self.current_file_diff_mut();
                for hunk in &mut diff.hunks {
                    hunk.approved = Some(true);
                }
                state.approval_queue.resolve_current(ApprovalResult::Approved);
                self.next_file();
            }
            DiffApprovalAction::RejectAll => {
                let diff = self.current_file_diff_mut();
                for hunk in &mut diff.hunks {
                    hunk.approved = Some(false);
                }
                state.approval_queue.resolve_current(ApprovalResult::Rejected);
                self.next_file();
            }
            _ => {}
        }
    }
}
```

### Hunk-Level Approval Indicators

```
Approved hunks shown with ✓, rejected with ✗, pending with ?:

  ✓ 15 │-        .duration_since(UNIX_EPOCH)?.as_secs();
  ✓ 15 │+        .duration_since(UNIX_EPOCH)?
  ✓ 16 │+        .as_secs();
  ? 18 │-    if token.expiry < now {
  ? 18 │+    if token.expiry <= now {
```

### Multi-File Diff Navigation

When the agent edits multiple files, the diff viewer provides file-level navigation:

```
┌──────────────────────────────────────────────────────────────┐
│ Files: [1] src/auth/token.rs  [2] src/auth/mod.rs  [3] ...  │
│                                                                │
│  [1] token.rs  — 2 hunks, 4 lines (1 pending)                │
│  [2] mod.rs    — 1 hunk,  3 lines (approved ✓)               │
│  [3] config.rs — 1 hunk,  2 lines (pending)                  │
│                                                                │
│  1/2/3: jump to file │ ]: next file │ [: prev file            │
└──────────────────────────────────────────────────────────────┘
```

## Diff Statistics Bar

```rust
/// Render diff statistics
fn render_diff_stats(buf: &mut Buffer, rect: Rect, diff: &FileDiff) {
    let total_added = diff.hunks.iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| matches!(l, DiffLine::Added { .. }))
        .count();
    let total_removed = diff.hunks.iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| matches!(l, DiffLine::Removed { .. }))
        .count();

    // Visual: +++++--- style stat bar
    let total = total_added + total_removed;
    let add_width = (rect.width as usize * total_added / total.max(1)) as u16;
    let rem_width = rect.width.saturating_sub(add_width);

    // Green bar for additions
    let add_bar = Span::styled(
        "+".repeat(add_width as usize),
        Style::default().fg(Color::Black).bg(Color::Green),
    );
    // Red bar for deletions
    let rem_bar = Span::styled(
        "-".repeat(rem_width as usize),
        Style::default().fg(Color::Black).bg(Color::Red),
    );

    Line::from(vec![add_bar, rem_bar]).render(rect, buf);

    // Text summary
    let summary = format!(" +{} -{} ~{} hunks ", total_added, total_removed, diff.hunks.len());
    let summary_span = Span::styled(summary, Style::default().fg(Color::White).bg(Color::DarkGray));
    summary_span.render(Rect::new(rect.x, rect.y, summary.len() as u16, 1), buf);
}
```

## Context Line Configuration

```rust
/// Number of context lines around each hunk (configurable)
pub struct DiffConfig {
    /// Lines of context around hunks (default: 3)
    pub context_lines: usize,

    /// Maximum line length before truncation (default: 120)
    pub max_line_length: usize,

    /// Whether to show whitespace markers
    pub show_whitespace: bool,

    /// Whether to auto-scroll to new hunks
    pub auto_scroll: bool,

    /// Tab width for display
    pub tab_width: usize,
}
```

The context line count is adjustable at runtime:

```
  =/-  Decrease/increase context lines
  0    Show no context (hunks only)
  3    Default context (3 lines)
  U    Show entire file
```
