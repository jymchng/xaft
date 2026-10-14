# PRD 49 — Markdown Rendering (F26)

## Status: Draft
## Depends on: PRD 09, PRD 10 (TUI architecture), PRD 50 (F27 code block rendering)

---

## 1. Problem

xaft's TUI currently renders agent output as plain text. When an agent produces
Markdown — which every modern LLM does by default — the user sees raw syntax:

```
## Summary

The refactoring is **complete**. The following changes were made:

- Replaced `random.choice` with a CSPRNG
- Added `~~deprecated~~` marker to old API

| Function | Status |
|----------|--------|
| `get_user` | ✓ Done |
```

Instead of a formatted, readable document. Raw syntax is noisy and breaks the
cognitive contract of a conversational UI: the agent is speaking to a human, not
writing a `.md` file.

Claude Code renders Markdown inline in the terminal. xaft must do the same.

---

## 2. Goals

1. Agent output (`LineKind::AgentText`) is parsed as CommonMark Markdown and
   rendered as styled terminal text before it reaches the screen.
2. **Headers** (`#`, `##`, `###`): bold + accent colour + separator glyph.
3. **Bold** (`**…**`): bold attribute.
4. **Italic** (`*…*` / `_…_`): italic attribute (dim on terminals that don't support it).
5. **Strikethrough** (`~~…~~`): strikethrough attribute.
6. **Unordered lists** (`-`, `*`, `+`): rendered with bullet `•` and hanging indent.
7. **Ordered lists** (`1.`, `2.`): rendered with number and hanging indent.
8. **Tables**: column-aligned with `│` borders and `─` dividers, respecting
   terminal width.
9. **Links**: inline text rendered underlined; URL shown in dim parentheses after.
10. **Block quotes** (`> `): every line prefixed with dim `│ `.
11. **Horizontal rules** (`---`, `***`, `___`): a 60-cell `─` separator
    (already used as the xaft TUI separator style).
12. **Code spans** (`` `…` ``) and **code blocks** (` ``` `) are passed
    through to F27 (PRD 50) unchanged; this PRD handles all non-code Markdown.
13. Rendering is disabled by config (`[tui] render_markdown = false`) for users
    who prefer raw output.
14. When disabled or when the renderer encounters a fatal parse error, the
    original text is emitted as-is. Rendering never loses content.

---

## 3. Non-Goals

- Syntax highlighting inside code blocks (F27 / PRD 50).
- Rendering Markdown in user messages (only agent output is styled).
- HTML pass-through (raw HTML in Markdown is emitted as-is, no rendering).
- Images (` ![alt](url)` is rendered as `[image: alt]` in dim colour).
- Definition lists, footnotes, task-list checkboxes (CommonMark extensions —
  rendered as plain text unless `pulldown_cmark`'s `ENABLE_TASKLISTS` is set).
- LaTeX / math blocks.
- Rendering in headless mode (no TUI, no ANSI).

---

## 4. Architecture

### 4.1 Rich-span model: `StyledSpan` and `StyledLine` extension

The current `StyledLine` holds a single `text: String` styled by `LineKind`.
That is sufficient for one-colour lines but cannot represent inline bold, italic,
underline, or multiple colours on the same line (e.g. bold header with dim
separator glyph).

`StyledLine` gains an optional `spans` field. When `spans` is `Some`, the renderer
iterates them instead of printing `text` as a flat string:

```rust
/// A single inline segment with its own styling.
#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text:          String,
    pub bold:          bool,
    pub italic:        bool,
    pub underline:     bool,
    pub strikethrough: bool,
    pub dim:           bool,
    /// Override fg colour. `None` inherits from the enclosing `LineKind`.
    pub fg:            Option<crossterm::style::Color>,
}

impl StyledSpan {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), bold: false, italic: false,
               underline: false, strikethrough: false, dim: false, fg: None }
    }
    pub fn bold(text: impl Into<String>) -> Self {
        Self { bold: true, ..Self::plain(text) }
    }
    pub fn dim(text: impl Into<String>) -> Self {
        Self { dim: true, ..Self::plain(text) }
    }
    // … builder methods for each attribute …
}
```

`StyledLine` becomes:

```rust
pub struct StyledLine {
    /// Plain-text content (used when `spans` is `None`, and as fallback for
    /// width calculations and test assertions).
    pub text: String,
    pub kind: LineKind,
    pub timestamp: Instant,
    pub agent: Option<String>,
    /// When `Some`, the renderer uses these spans for inline styling instead
    /// of printing `text` as a flat string.
    pub spans: Option<Vec<StyledSpan>>,
}
```

`text` always equals `spans.iter().map(|s| s.text.as_str()).collect::<String>()`
when `spans` is `Some`. This invariant is maintained by `MarkdownRenderer` and
checked in tests.

**Backwards compatibility**: every existing `StyledLine::new(…)` call produces
`spans = None`. All existing rendering paths are unchanged. The new span renderer
is a new `if let Some(spans) = &line.spans` branch in `write_styled_line`.

### 4.2 `MarkdownRenderer`

A new module `crates/xaft-tui/src/render/markdown.rs`:

```rust
/// Parses a Markdown text string and produces styled terminal lines.
pub struct MarkdownRenderer {
    /// Terminal column count — used for word-wrap and table layout.
    term_cols: usize,
    /// Left margin for content (default 2 for the `  ` indent xaft uses).
    indent: usize,
    /// Config opt-out flag (from `TuiConfig::render_markdown`).
    enabled: bool,
}

impl MarkdownRenderer {
    pub fn new(term_cols: usize) -> Self { … }
    pub fn with_indent(mut self, indent: usize) -> Self { … }
    pub fn disabled() -> Self { … }  // always passes text through

    /// Render `text` to a list of styled lines.
    ///
    /// Never panics. On parse error, returns the raw text as `AgentText` lines.
    pub fn render(&self, text: &str) -> Vec<StyledLine> { … }
}
```

### 4.3 Parser: `pulldown_cmark`

The renderer uses `pulldown_cmark` (not `termimad`) as the Markdown parser.
`pulldown_cmark` produces a structured `Event` stream; we map that to
`Vec<StyledLine>` directly, giving us full control over styling and layout.
`termimad` is used only for table rendering (see §4.7) where its column-width
and padding logic is most valuable.

```toml
# crates/xaft-tui/Cargo.toml
pulldown-cmark = "0.13"
termimad       = { version = "0.34", optional = true }

[features]
default   = ["memory", "session", "markdown"]
markdown  = ["dep:pulldown-cmark", "dep:termimad"]
```

Making both deps optional keeps the dependency footprint small for integrators
that don't need TUI rendering.

### 4.4 Event-to-line mapping

The `MarkdownRenderer::render` method drives a state machine over
`pulldown_cmark` events:

```rust
fn render(&self, text: &str) -> Vec<StyledLine> {
    if !self.enabled {
        return self.passthrough(text);
    }

    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options);

    let mut ctx = RenderCtx::new(self.term_cols, self.indent);
    for event in parser {
        ctx.handle(event);
    }
    ctx.finish()
}
```

`RenderCtx` holds:
- `lines: Vec<StyledLine>` — accumulates output lines.
- `span_stack: Vec<SpanState>` — active inline annotations (bold, italic, etc.).
- `list_stack: Vec<ListState>` — for nested lists.
- `in_blockquote: usize` — blockquote nesting depth.
- `table_builder: Option<TableBuilder>` — active table being constructed.
- `current_line: Vec<StyledSpan>` — spans for the line being built.
- `code_buffer: Option<CodeBlockBuffer>` — collects F27 code fences.

### 4.5 Header rendering

```
Event::Start(Tag::Heading { level, .. })  →  push SpanState::Bold + set HeaderLevel
Event::End(Tag::Heading { level, .. })    →  flush, emit styled header line(s)
```

Level → rendering:

| Level | Glyph  | Style                                    | Example                     |
|-------|--------|------------------------------------------|-----------------------------|
| H1    | `──`   | Bold + accent colour + surrounding `─` line | `── Summary ────────────` |
| H2    | `▸`    | Bold + accent colour                     | `▸ Summary`                 |
| H3    | `›`    | Bold + fg colour                         | `› Summary`                 |
| H4–H6 | (none) | Bold only                                | `Summary`                   |

H1 also emits a following `─`-repeat separator spanning `term_cols` chars
(using the existing `LineKind::Separator` style), making major sections visually
distinct from body text without consuming extra vertical space.

The header line itself is a `StyledLine` with `spans`:
```
[StyledSpan("▸ ", dim=true), StyledSpan("Summary", bold=true, fg=Some(theme.accent))]
```

Because theme colours aren't known at render time (the renderer runs in `AppState`,
before the renderer sees a `Theme`), the `fg` field of `StyledSpan` stores a
semantic colour tag via a newtype:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanColor {
    Inherit,       // use the LineKind's default colour
    Accent,        // theme.accent
    Dim,           // theme.dim
    Success,       // theme.success
    Warning,       // theme.warning
    Error,         // theme.error
    Code,          // theme.tool (reused for inline code fg)
}
```

`StyledSpan.fg` becomes `Option<SpanColor>`. `write_styled_line` resolves
`SpanColor → Color` via the active `Theme` at draw time. This keeps
`MarkdownRenderer` theme-agnostic and testable without a `Theme` instance.

### 4.6 Inline elements

Inline formatting is tracked via a stack of active span attributes:

```rust
Event::Start(Tag::Strong)      → push Bold
Event::Start(Tag::Emphasis)    → push Italic
Event::Start(Tag::Strikethrough) → push Strikethrough
Event::Start(Tag::Link { dest_url, .. }) → push Link { url }
Event::Text(s)                 → emit StyledSpan(s, current attrs from stack)
Event::End(Tag::Strong)        → pop Bold
```

For links:
- The link text is rendered with `underline: true`.
- After the closing `]`, a dim `(url)` span is appended.
- Long URLs are truncated at 80 chars with `…` suffix.

```
  → see the [docs](https://example.com/very/long/path) for details
  →   see the docs (https://example.com/very/long/p…)
```

For images:
- `Event::Start(Tag::Image { dest_url, .. })` → `[image: <alt>]` in `SpanColor::Dim`.
- Alt text is collected from nested `Text` events.

For code spans (`` `code` ``):
- `Event::Code(s)` → `StyledSpan { text: s, fg: SpanColor::Code, .. }`.
- Note: `Event::Code` is a leaf event (not Start/End); no nesting.
- The F27 renderer does NOT process code spans — only fenced/indented code
  *blocks* are its domain. Code spans are styled but not syntax-highlighted.

### 4.7 Lists

```rust
Event::Start(Tag::List(None))    → push UnorderedList { indent: current + 2 }
Event::Start(Tag::List(Some(n))) → push OrderedList { start: n, counter: n, indent: current + 2 }
Event::Start(Tag::Item)          → prepare bullet/number
Event::End(Tag::Item)            → pop item, emit newline
Event::End(Tag::List(_))         → pop list
```

**Unordered lists**: each item begins with `  • ` (2-space indent + bullet + space).
Nested lists: another 2-space indent per level, bullet changes: `• → ‣ → ◦`.

**Ordered lists**: each item begins with `  N. ` (2-space indent + number + period + space).
The counter increments per item; nesting starts a new counter.

**Hanging indent**: continuation lines within an item (if the item wraps) are
indented by the same prefix width so text aligns with the first line's content.

Example (80-column output):
```
  • First bullet
  • Second bullet with a long description that wraps onto the
    next line at the correct indent level
    ‣ Nested item under second bullet
    ‣ Another nested item
  • Third bullet
```

Each word-wrapped item line is emitted as a separate `StyledLine` with `spans`
carrying the correct span stack for any inline bold/italic within the item.

### 4.8 Tables

Tables are the most complex element. They require:
1. Collecting all cell text.
2. Computing column widths (max cell width per column, capped so total fits `term_cols`).
3. Emitting header row, divider row, and data rows with `│` borders.

`termimad` is used here: after collecting the raw table events from `pulldown_cmark`,
the table source is rendered by `termimad::MadSkin::term_text` to a string with
ANSI-escaped column formatting. That string is then split by lines and emitted as
`StyledLine { text: line, spans: None, kind: LineKind::AgentText }` lines.

The reason for using `termimad` for tables specifically: its column-width allocation
and padding logic handles multi-column overflow, minimum column widths, and alignment
correctly without re-implementing the algorithm.

**Table width cap**: if a table is wider than `term_cols - 4`, columns are truncated
with `…` to fit. A table that truly cannot fit at minimum column widths (2 chars each)
is emitted as its raw Markdown source as a fallback.

Visual format (80 columns):
```
  │ Function   │ Status  │
  ├────────────┼─────────┤
  │ get_user   │ ✓ Done  │
  │ post_item  │ ✗ TODO  │
```

### 4.9 Block quotes

```rust
Event::Start(Tag::BlockQuote) → increment in_blockquote
Event::End(Tag::BlockQuote)   → decrement in_blockquote
```

Every `CommitLine` produced while `in_blockquote > 0` has its `spans` prepended
with `nesting` repetitions of `StyledSpan("│ ", dim=true, fg=SpanColor::Dim)`:

```
  │ This is a quoted paragraph.
  │ │ This is a doubly-quoted paragraph.
```

Block quote lines also use a slightly dimmer `SpanColor::Dim` for the body text.

### 4.10 Horizontal rules

```rust
Event::Rule → emit StyledLine::new("─".repeat(60), LineKind::Separator)
```

Reuses the existing `Separator` style (dim grey `─` chars). The width is fixed at
60 (matching the existing separator usage) rather than full terminal width, to
avoid visual dominance.

### 4.11 Code block pass-through

```rust
Event::Start(Tag::CodeBlock(kind)) → open CodeBlockBuffer { language: … }
Event::Text(s)                     → append to CodeBlockBuffer (NOT to current line)
Event::End(Tag::CodeBlock(_))      → flush CodeBlockBuffer as LineKind::CodeBlock
```

Code blocks are emitted as `StyledLine { kind: LineKind::CodeBlock, .. }` with
`text` holding the raw code and `agent` holding the language tag. F27 (PRD 50)
processes these lines. If F27 is disabled, `CodeBlock` lines render the same as
`AgentText` (monochrome, no syntax highlight).

```rust
pub enum LineKind {
    // … existing …
    /// A fenced or indented code block; consumed by F27 for syntax highlighting.
    /// `text` is the raw source; the language hint is in `agent` (field reused).
    CodeBlock,
}
```

No new field is added to `StyledLine` for the language tag; the `agent` field
(originally for the LLM agent name) is repurposed as a `content_hint` field
in this context. If this dual use becomes confusing, a refactor to rename it
`meta: Option<String>` is the suggested follow-up.

### 4.12 Integration point: `AppState::handle_event`

The renderer is called in the `TuiEvent::AgentOutput` arm:

```rust
TuiEvent::AgentOutput { agent_name, content } => {
    self.current_agent = agent_name.clone();
    if self.stream_active {
        self.push_pipeline_mutation(RenderMutation::FlushStream);
        self.stream_active = false;
    }

    // Markdown rendering.
    let lines = self.markdown_renderer.render(&content);
    for line in lines {
        self.push_pipeline_mutation(RenderMutation::CommitLine(
            line.with_agent(&agent_name)
        ));
    }
}
```

`AppState` gains:

```rust
pub markdown_renderer: MarkdownRenderer,
```

Initialised in `AppState::new`:

```rust
markdown_renderer: MarkdownRenderer::new(120)  // updated on Resize events
    .with_indent(2),
```

On `TuiEvent::Resize`:
```rust
self.markdown_renderer = MarkdownRenderer::new(w as usize).with_indent(2);
```

### 4.13 `write_styled_line` update

The renderer gains a new branch for lines with spans:

```rust
fn write_styled_line(&mut self, line: &StyledLine, theme: &Theme) -> io::Result<()> {
    if let Some(spans) = &line.spans {
        return self.write_spans(spans, theme);
    }
    // … existing flat-text path …
}

fn write_spans(&mut self, spans: &[StyledSpan], theme: &Theme) -> io::Result<()> {
    for span in spans {
        let fg = match span.fg {
            Some(SpanColor::Accent)  => theme.accent,
            Some(SpanColor::Dim)     => theme.dim,
            Some(SpanColor::Success) => theme.success,
            Some(SpanColor::Warning) => theme.warning,
            Some(SpanColor::Error)   => theme.error,
            Some(SpanColor::Code)    => theme.tool,
            Some(SpanColor::Inherit) | None => theme.fg,
        };
        if span.bold          { queue!(self.out, SetAttribute(Attribute::Bold))?; }
        if span.italic        { queue!(self.out, SetAttribute(Attribute::Italic))?; }
        if span.underline     { queue!(self.out, SetAttribute(Attribute::Underlined))?; }
        if span.strikethrough { queue!(self.out, SetAttribute(Attribute::CrossedOut))?; }
        queue!(self.out, SetForegroundColor(fg), Print(&span.text), SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}
```

---

## 5. Word Wrap

Long agent paragraphs must be wrapped at `term_cols`. The markdown renderer
applies wrap **after** building spans, not before:

1. `Event::Text(s)` adds spans to `current_line`.
2. `Event::SoftBreak` / `Event::HardBreak` / `Event::End(Tag::Paragraph)` trigger
   line commit.
3. Before committing, the line is checked for width:
   - Total width = sum of `display_width(span.text)` for all spans.
   - If ≤ `term_cols - indent`: commit as a single `StyledLine`.
   - If > `term_cols - indent`: split into multiple `StyledLine`s by word-wrapping
     the flat text, then re-assign spans to each wrapped line proportionally
     (spans that span a wrap point are split at the word boundary).

Word-wrap-aware span splitting:
```rust
fn wrap_spans(spans: Vec<StyledSpan>, max_width: usize) -> Vec<Vec<StyledSpan>> {
    // Flatten to (char_idx, span_idx, char) triples.
    // Find word boundaries. Split at terminal-width column.
    // Reassemble spans per line, splitting any span that crosses a break point.
}
```

This is the most complex part of the implementation. The existing `word_wrap` helper
in `renderer.rs` operates on flat `String`; a new `span_word_wrap` function will be
added alongside it.

---

## 6. Streaming interaction

Agent output arrives via streaming tokens (`TuiEvent::StreamToken`) before the
final `TuiEvent::AgentOutput`. Stream tokens are rendered unformatted (they are
partial Markdown that cannot be correctly parsed mid-stream). When
`TuiEvent::AgentOutput` arrives, `FlushStream` is emitted first (clearing the
partial stream line), then the fully parsed Markdown lines are committed.

This is unchanged from the current flow — the only difference is that the committed
lines now carry spans instead of flat text for agent output.

---

## 7. `XaftConfig` knobs

```toml
[tui]
# Enable Markdown rendering in agent output.
# When false, agent text is displayed as raw Markdown syntax.
render_markdown = true

# Minimum number of tool calls in a burst before tables collapse.
# Only used by F27 code blocks; preserved here for reference.
# (see PRD 50)

# H1 headers emit a full-width separator line below them.
markdown_h1_separator = true

# Maximum column width in rendered tables (chars). Wider columns are truncated.
markdown_table_max_col_width = 40

# Maximum total rows of table output. Tables beyond this are rendered raw.
markdown_table_max_rows = 50

# When true, block quotes display with a │ prefix. When false, they are
# rendered as indented plain text.
markdown_blockquote_bar = true
```

---

## 8. `TuiConfig` additions

```rust
pub struct TuiConfig {
    // … existing …
    pub render_markdown:              bool,
    pub markdown_h1_separator:        bool,
    pub markdown_table_max_col_width: usize,
    pub markdown_table_max_rows:      usize,
    pub markdown_blockquote_bar:      bool,
}
```

Defaults in `defaults.rs`:
```rust
render_markdown:              true,
markdown_h1_separator:        true,
markdown_table_max_col_width: 40,
markdown_table_max_rows:      50,
markdown_blockquote_bar:      true,
```

---

## 9. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/render/markdown.rs` | **New** — `MarkdownRenderer`, `RenderCtx`, `SpanColor`, `StyledSpan`, word-wrap helpers |
| `crates/xaft-tui/src/render/mod.rs` | **New** — `pub mod markdown` |
| `crates/xaft-tui/src/transcript.rs` | Add `StyledSpan`, `SpanColor`; add `spans` field to `StyledLine`; add `CodeBlock` to `LineKind` |
| `crates/xaft-tui/src/renderer.rs` | `write_styled_line` span branch; `write_spans`; `span_word_wrap`; resolve `SpanColor→Color` at draw time |
| `crates/xaft-tui/src/state.rs` | Add `markdown_renderer: MarkdownRenderer`; call in `AgentOutput` arm; update on `Resize` |
| `crates/xaft-tui/src/lib.rs` | `pub mod render`; re-export `MarkdownRenderer`, `StyledSpan`, `SpanColor` |
| `crates/xaft-config/src/types.rs` | 5 new `TuiConfig` fields |
| `crates/xaft-config/src/defaults.rs` | Defaults for above |
| `crates/xaft-tui/Cargo.toml` | Add `pulldown-cmark`, `termimad` (feature-gated) |
| `crates/xaft-tui/tests/markdown_rendering.rs` | **New** — integration tests |

---

## 10. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | Agent output `## Summary\n\nDone.` renders as a bold header line `▸ Summary` followed by `Done.`, with no `#` or `##` in the output. |
| AC2 | `**bold text**` produces a `StyledSpan` with `bold: true`; no `**` characters appear in rendered output. |
| AC3 | `*italic*` produces a `StyledSpan` with `italic: true`. |
| AC4 | `~~strikethrough~~` produces a `StyledSpan` with `strikethrough: true`. |
| AC5 | An unordered list of 3 items renders each item on its own line prefixed with `  • `. |
| AC6 | An ordered list renders `  1. First`, `  2. Second`, etc. |
| AC7 | A nested list indents by 2 additional chars and uses `  ‣` bullet. |
| AC8 | A Markdown table with 3 columns renders with `│` column separators aligned to column widths. |
| AC9 | A table wider than `term_cols` truncates columns with `…` rather than overflowing. |
| AC10 | `[link text](https://example.com)` renders as `link text (https://example.com)` with underline on `link text`. |
| AC11 | `> quote text` renders as `│ quote text` with dim `│` prefix. |
| AC12 | `---` in body text renders as a 60-cell `─` separator, not three hyphens. |
| AC13 | A fenced code block is emitted as `LineKind::CodeBlock` and is NOT processed by the markdown renderer (raw code preserved). |
| AC14 | `render_markdown = false` in config causes agent text to be rendered without any span processing (raw Markdown syntax visible). |
| AC15 | Malformed Markdown (unclosed `**`, mismatched list nesting) is rendered without panic and produces at minimum the raw text content. |
| AC16 | The `text` field of any `StyledLine` produced by the renderer equals the concatenation of all span `text` fields. |
| AC17 | H1 heading followed by `markdown_h1_separator = true` emits a `LineKind::Separator` line immediately after the header. |

---

## 11. Tests

### `crates/xaft-tui/tests/markdown_rendering.rs`

```rust
#[test]
fn renders_h2_as_bold_accent_spans() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("## Summary\n\nDone.");
    // First line should contain a bold span with "Summary".
    let header = &lines[0];
    let spans = header.spans.as_ref().expect("header must have spans");
    assert!(spans.iter().any(|s| s.bold && s.text.contains("Summary")));
    // No '#' characters in any span text.
    assert!(!lines.iter().any(|l| l.text.contains('#')));
}

#[test]
fn renders_bold_inline() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("The **quick** brown fox.");
    let spans = lines[0].spans.as_ref().unwrap();
    let bold_span = spans.iter().find(|s| s.bold).expect("bold span expected");
    assert_eq!(bold_span.text, "quick");
    assert!(!lines[0].text.contains("**"));
}

#[test]
fn renders_italic_inline() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("The *quick* brown fox.");
    let spans = lines[0].spans.as_ref().unwrap();
    assert!(spans.iter().any(|s| s.italic && s.text == "quick"));
}

#[test]
fn renders_strikethrough() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("The ~~old~~ API is gone.");
    let spans = lines[0].spans.as_ref().unwrap();
    assert!(spans.iter().any(|s| s.strikethrough && s.text == "old"));
    assert!(!lines[0].text.contains("~~"));
}

#[test]
fn renders_unordered_list_with_bullet() {
    let r = MarkdownRenderer::new(80).with_indent(2);
    let lines = r.render("- Alpha\n- Beta\n- Gamma");
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("• Alpha")));
    assert!(texts.iter().any(|t| t.contains("• Beta")));
    assert!(texts.iter().any(|t| t.contains("• Gamma")));
}

#[test]
fn renders_ordered_list_with_numbers() {
    let r = MarkdownRenderer::new(80).with_indent(2);
    let lines = r.render("1. One\n2. Two\n3. Three");
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("1. One")));
    assert!(texts.iter().any(|t| t.contains("2. Two")));
}

#[test]
fn renders_nested_list_with_deeper_indent() {
    let r = MarkdownRenderer::new(80).with_indent(2);
    let md = "- Outer\n  - Inner\n  - Inner 2\n- Outer 2";
    let lines = r.render(md);
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    // Inner items must be indented further than outer.
    let inner = texts.iter().find(|t| t.contains("Inner")).unwrap();
    let outer = texts.iter().find(|t| t.contains("Outer") && !t.contains("Inner")).unwrap();
    let inner_indent = inner.len() - inner.trim_start().len();
    let outer_indent = outer.len() - outer.trim_start().len();
    assert!(inner_indent > outer_indent);
}

#[test]
fn renders_table_with_borders() {
    let r = MarkdownRenderer::new(80);
    let md = "| Name | Value |\n|------|-------|\n| foo | bar |";
    let lines = r.render(md);
    let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains('│'), "table must use │ borders: {joined}");
    assert!(joined.contains("Name"), "header row must be present: {joined}");
    assert!(joined.contains("foo"), "data row must be present: {joined}");
}

#[test]
fn table_does_not_exceed_term_cols() {
    let r = MarkdownRenderer::new(40);
    let md = "| Column1 | Column2 | Column3 | Column4 |\n|---------|---------|---------|---------||\n| val1    | val2    | val3    | val4    |";
    let lines = r.render(md);
    for line in &lines {
        assert!(
            crate::renderer::display_width(&line.text) <= 40,
            "line wider than term_cols: {:?}", line.text
        );
    }
}

#[test]
fn renders_link_with_url_suffix() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("See [the docs](https://example.com/docs).");
    let spans = lines[0].spans.as_ref().unwrap();
    // Link text must be underlined.
    assert!(spans.iter().any(|s| s.underline && s.text == "the docs"));
    // URL must appear as dim span.
    assert!(spans.iter().any(|s| s.dim && s.text.contains("https://example.com/docs")));
    // No raw `[`, `]`, `(`, `)` or URL in a non-dim span.
    assert!(!spans.iter().any(|s| !s.dim && s.text.contains("https://")));
}

#[test]
fn renders_blockquote_with_bar_prefix() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("> Quoted text\n> second line");
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert!(texts.iter().all(|t| t.contains("│")));
    // Body text must not begin with '>'.
    assert!(!texts.iter().any(|t| t.trim_start().starts_with('>')));
}

#[test]
fn renders_blockquote_disabled_by_config() {
    let r = MarkdownRenderer::new(80).with_blockquote_bar(false);
    let lines = r.render("> Quoted text");
    // No │ prefix when bar is disabled.
    assert!(!lines.iter().any(|l| l.text.contains('│')));
    // Text content must still be present.
    assert!(lines.iter().any(|l| l.text.contains("Quoted text")));
}

#[test]
fn renders_horizontal_rule_as_separator() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("before\n\n---\n\nafter");
    assert!(lines.iter().any(|l| {
        l.kind == LineKind::Separator && l.text.chars().all(|c| c == '─')
    }));
}

#[test]
fn code_block_passed_through_as_code_block_kind() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("Text\n\n```rust\nfn main() {}\n```\n\nMore text");
    // One line must have CodeBlock kind.
    assert!(lines.iter().any(|l| l.kind == LineKind::CodeBlock));
    // The code block line's text must contain the raw source.
    let cb = lines.iter().find(|l| l.kind == LineKind::CodeBlock).unwrap();
    assert!(cb.text.contains("fn main()"));
    // The language hint must be in `agent`.
    assert_eq!(cb.agent.as_deref(), Some("rust"));
}

#[test]
fn code_span_rendered_with_code_color() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("Use `println!` for output.");
    let spans = lines[0].spans.as_ref().unwrap();
    let code_span = spans.iter().find(|s| s.text == "println!").expect("code span expected");
    assert_eq!(code_span.fg, Some(SpanColor::Code));
}

#[test]
fn render_markdown_false_passes_through_raw() {
    let r = MarkdownRenderer::disabled();
    let md = "## Header\n\n**bold**";
    let lines = r.render(md);
    // Raw text preserved, including Markdown syntax.
    let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("##"), "raw markdown must be preserved when disabled");
    assert!(joined.contains("**bold**"));
}

#[test]
fn malformed_markdown_does_not_panic() {
    let r = MarkdownRenderer::new(80);
    let inputs = [
        "**unclosed bold",
        "| broken | table",
        "> deeply\n>>> nested\n> > mixed",
        "- item\n  - nested\n    - triple\n  - back",
        "```\nunclosed code block",
        "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6",
        "~~unclosed strike",
        "[link without closing paren](https://example.com",
    ];
    for input in &inputs {
        let result = std::panic::catch_unwind(|| {
            MarkdownRenderer::new(80).render(input)
        });
        assert!(result.is_ok(), "render panicked on input: {input:?}");
    }
}

#[test]
fn styled_line_text_equals_span_concatenation() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("Hello **world** and *earth*.");
    for line in &lines {
        if let Some(spans) = &line.spans {
            let concat: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(
                line.text, concat,
                "text/span invariant violated on line: {:?}", line.text
            );
        }
    }
}

#[test]
fn h1_emits_separator_line_when_configured() {
    let r = MarkdownRenderer::new(80).with_h1_separator(true);
    let lines = r.render("# Top-Level Header");
    assert!(
        lines.iter().any(|l| l.kind == LineKind::Separator),
        "H1 separator not emitted: {lines:?}"
    );
}

#[test]
fn image_rendered_as_alt_placeholder() {
    let r = MarkdownRenderer::new(80);
    let lines = r.render("![A diagram](diagram.png)");
    assert!(lines.iter().any(|l| l.text.contains("image") || l.text.contains("A diagram")));
    // No raw `![` in output.
    assert!(!lines.iter().any(|l| l.text.contains("![")));
}
```

### Unit tests in `crates/xaft-tui/src/render/markdown.rs`

```rust
#[test]
fn span_word_wrap_single_line_no_split() {
    let spans = vec![StyledSpan::plain("hello world")];
    let result = span_word_wrap(spans, 80);
    assert_eq!(result.len(), 1);
}

#[test]
fn span_word_wrap_splits_across_span_boundary() {
    // "foo " (bold) + "bar baz" (plain), wrap at 7 → "foo bar" / "baz"
    let spans = vec![
        StyledSpan::bold("foo "),
        StyledSpan::plain("bar baz"),
    ];
    let result = span_word_wrap(spans, 7);
    assert_eq!(result.len(), 2);
    // First line contains bold text.
    assert!(result[0].iter().any(|s| s.bold));
}

#[test]
fn display_width_correct_for_unicode() {
    // Emoji is 2 wide; ASCII is 1 wide.
    assert_eq!(display_width("hello"), 5);
    assert_eq!(display_width("🦀"), 2);
    assert_eq!(display_width("héllo"), 5);
}
```

---

## 12. Edge Cases

| Case | Behaviour |
|------|-----------|
| Empty agent message | `render("")` → empty `Vec<StyledLine>`. |
| All-whitespace message | Returns whitespace lines as `AgentText`, no spans. |
| Message with only a code block | Returns one `CodeBlock` line; no other content. |
| Table with 1 column | Valid single-column table with `│` borders. |
| Table with 0 rows (headers only) | Header row emitted; no data rows. |
| Very narrow terminal (`term_cols = 20`) | Tables fall back to raw Markdown if columns cannot reach minimum width. |
| Agent name with special chars | `with_agent(&agent_name)` is called after rendering; no sanitisation needed. |
| Markdown in a background pipeline (PRD 33d) | Rendering happens in `push_pipeline_mutation` path; buffered as rendered spans. |
| `render_markdown = true` with F27 disabled | `CodeBlock` lines render as plain `AgentText` (fallback in `write_styled_line`). |
| Agent produces raw HTML | Pass-through: `Event::Html(s)` → plain text span, no ANSI. |
| NUL bytes in agent text | Stripped before passing to `pulldown_cmark` (same as bash-mode guard). |
| Single `#` with no text | Renders as empty header line (bold empty span). |
| Paragraph immediately after list | No extra blank line emitted between list and following paragraph. |

---

## 13. Performance considerations

- `pulldown_cmark` is a zero-copy, event-driven parser with O(n) complexity.
  Typical agent messages are < 4 KB; rendering is < 1 ms.
- `span_word_wrap` operates on char-indexed spans; worst case is O(n · m) where
  n = line count and m = spans per line. Both are small in practice.
- The `MarkdownRenderer` instance is reused across calls (held in `AppState`).
  No heap allocation per render beyond the output `Vec<StyledLine>`.
- Streaming tokens (`StreamToken`) bypass the renderer entirely; only the final
  `AgentOutput` event triggers rendering. No per-token overhead.

---

## 14. Interaction matrix

| Scenario | Behaviour |
|----------|-----------|
| `render_markdown = true` + agent sends raw text (no Markdown) | `pulldown_cmark` parses paragraphs and emits them as `AgentText` with `spans: Some([plain span])`. Output visually identical to current. |
| PRD 33d background mode active | Rendered `StyledLine`s (with spans) are buffered in `bg.buffered` and replayed on re-attach. Spans survive the buffer unchanged. |
| PRD 33b collapse groups | `OpenCollapseGroup` / `CloseCollapseGroup` bracket the rendered lines exactly as before; the renderer emits multiple `CommitLine` mutations per agent turn, and the grouping logic in `AppState` still works. |
| `/compact` (PRD 36) | Compact mode replaces the conversation with a summary. The summary is itself an agent message and will be markdown-rendered normally. |
| PRD 27 F2 code block (F27) | `CodeBlock` lines from this renderer are the input to F27. The contract: `kind == CodeBlock`, `text` = raw source, `agent` = language hint. |
| Resize event | `AppState` reconstructs `MarkdownRenderer::new(new_cols)`; already-committed lines are in the terminal's scrollback and cannot be re-rendered (append-only architecture). |
